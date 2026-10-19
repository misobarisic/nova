//! Public Stremio addon directories. Listings are transient, never installed
//! automatically, and use the ordinary profile-owned install/sync path.
use super::*;
use crate::AddonBrowserRow;
use std::time::Instant;

const OFFICIAL: &str =
    "https://raw.githubusercontent.com/Stremio/stremio-official-addons/master/index.json";
const OFFICIAL_PAGE: &str = "https://github.com/Stremio/stremio-official-addons";
const RESPONSE_LIMIT: usize = 4 * 1024 * 1024;
const ENTRY_LIMIT: usize = 500;
const CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone)]
struct Entry {
    url: String,
    manifest: Manifest,
    configurable: bool,
}
#[derive(Default)]
struct Source {
    entries: Vec<Entry>,
    name: String,
    url: String,
    fetched: Option<Instant>,
    busy: bool,
    failed: bool,
    generation: u64,
}
#[derive(Default)]
pub(super) struct State {
    sources: [Source; 2],
    directory_url: String,
    logos_requested: HashSet<String>,
    logos_active: usize,
    notice: Option<&'static str>,
}

fn public_url(raw: &str) -> Option<url::Url> {
    net::public_directory::public_url(raw)
}

fn parse_entries(bytes: &[u8], official: bool) -> Result<Vec<Entry>, ()> {
    if bytes.len() > RESPONSE_LIMIT {
        return Err(());
    }
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| ())?;
    let rows = if official {
        value.as_array()
    } else {
        value.get("addons").and_then(|v| v.as_array())
    }
    .ok_or(())?;
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for row in rows.iter().take(ENTRY_LIMIT) {
        let Some(raw) = row.get("transportUrl").and_then(|v| v.as_str()) else {
            continue;
        };
        // Addon currently strips query parameters; do not silently install a
        // different configuration from a query-encoded directory entry.
        let Some(transport) = public_url(raw).filter(|u| u.query().is_none()) else {
            continue;
        };
        if !transport.path().ends_with("/manifest.json") {
            continue;
        }
        let Some(mut manifest) = row
            .get("manifest")
            .cloned()
            .and_then(|v| serde_json::from_value::<Manifest>(v).ok())
        else {
            continue;
        };
        if manifest.id.trim().is_empty()
            || manifest.version.trim().is_empty()
            || manifest.name.trim().is_empty()
            || manifest.name.len() > 512
            || matches!(
                manifest.id.as_str(),
                "org.stremio.local" | "com.stremio.local"
            )
        {
            continue;
        }
        let Ok(addon) = Addon::new(transport.as_str()) else {
            continue;
        };
        let url = addon.base_url().to_owned();
        if !seen.insert(url.clone()) {
            continue;
        }
        manifest.logo = manifest.logo.filter(|url| public_url(url).is_some());
        if let Some(description) = &mut manifest.description {
            *description = description.chars().take(2000).collect();
        }
        let configurable = manifest.extra.get("behaviorHints").is_some_and(|hints| {
            ["configurable", "configurationRequired"]
                .iter()
                .any(|key| hints.get(key).and_then(|v| v.as_bool()) == Some(true))
        });
        entries.push(Entry {
            url,
            manifest,
            configurable,
        });
    }
    Ok(entries)
}

fn directory_endpoint(bytes: &[u8], base: &str) -> Result<(String, String), ()> {
    let manifest = Addon::parse_manifest(bytes).map_err(|_| ())?;
    if !manifest.has_resource("addon_catalog") {
        return Err(());
    }
    let catalogs = manifest
        .extra
        .get("addonCatalogs")
        .and_then(|v| v.as_array())
        .ok_or(())?;
    let catalog = catalogs
        .iter()
        .find(|c| c.get("type").and_then(|v| v.as_str()) == Some("all"))
        .or_else(|| catalogs.first())
        .ok_or(())?;
    let kind = catalog
        .get("type")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or(())?;
    let id = catalog
        .get("id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or(())?;
    // Use URL path segments: directory metadata must not inject query strings
    // or replace the origin while constructing the standard resource URL.
    let mut endpoint =
        url::Url::parse(&format!("{}/", base.trim_end_matches('/'))).map_err(|_| ())?;
    endpoint
        .path_segments_mut()
        .map_err(|_| ())?
        .pop_if_empty()
        .push("addon_catalog")
        .push(kind)
        .push(&format!("{id}.json"));
    Ok((
        endpoint.to_string(),
        manifest.name.chars().take(512).collect(),
    ))
}

fn capability_caption(manifest: &Manifest) -> String {
    [
        (manifest.has_catalogs(), "Catalogs"),
        (manifest.has_meta(), "Metadata"),
        (manifest.has_streams(), "Streams"),
        (manifest.has_subtitles(), "Subtitles"),
    ]
    .into_iter()
    .filter(|(has, _)| *has)
    .map(|(_, label)| text::tr(label))
    .collect::<Vec<_>>()
    .join(" · ")
}
fn entry_matches(entry: &Entry, query: &str, filter: i32) -> bool {
    let matches_filter = match filter {
        1 => entry.manifest.has_catalogs(),
        2 => entry.manifest.has_streams(),
        3 => entry.manifest.has_subtitles(),
        _ => true,
    };
    matches_filter
        && (query.is_empty()
            || entry.manifest.name.to_lowercase().contains(query)
            || entry
                .manifest
                .description
                .as_deref()
                .unwrap_or_default()
                .to_lowercase()
                .contains(query)
            || entry.manifest.id.to_lowercase().contains(query))
}

impl Bridge {
    pub(super) fn load_addon_browser(&self, force: bool) {
        let Some(app) = self.app() else { return };
        let index = usize::from(app.get_addon_browser_source() == 1);
        let raw_directory = app.get_addon_browser_directory_url().trim().to_string();
        let directory = if index == 1 {
            let Some(url) = public_url(&raw_directory)
                .filter(|url| url.scheme() == "https" && url.query().is_none())
            else {
                self.shared.lock().unwrap().addon_browser.notice =
                    Some("Enter a public HTTPS addon-directory manifest URL.");
                self.apply_addon_browser_rows();
                return;
            };
            let Ok(addon) = Addon::new(url.as_str()) else {
                return;
            };
            addon.base_url().to_string()
        } else {
            String::new()
        };
        let generation = {
            let mut state = self.shared.lock().unwrap();
            let browser = &mut state.addon_browser;
            if index == 1 && browser.directory_url != directory {
                browser.directory_url = directory.clone();
                let generation = browser.sources[1].generation.wrapping_add(1);
                browser.sources[1] = Source {
                    generation,
                    ..Source::default()
                };
            }
            browser.notice = None;
            if force {
                browser.logos_requested.clear();
            }
            let source = &mut browser.sources[index];
            if source.busy
                || (!force
                    && source
                        .fetched
                        .is_some_and(|time| time.elapsed() < CACHE_TTL))
            {
                drop(state);
                self.apply_addon_browser_rows();
                return;
            }
            source.busy = true;
            source.failed = false;
            source.generation = source.generation.wrapping_add(1);
            if index == 0 {
                source.name = "Stremio official addons".into();
                source.url = OFFICIAL_PAGE.into();
            } else {
                source.url = directory.clone();
            }
            source.generation
        };
        self.apply_addon_browser_rows();
        let bridge = self.clone();
        let context = profiles::capture();
        let url = if index == 0 {
            OFFICIAL.into()
        } else {
            Addon::new(&directory).unwrap().manifest_url()
        };
        net::public_directory::fetch_document(url, RESPONSE_LIMIT, move |result| {
            if index == 0 {
                let entries = result
                    .map_err(|_| ())
                    .and_then(|bytes| parse_entries(&bytes, true));
                bridge.finish_addon_browser(context, index, generation, None, entries);
                return;
            }
            let endpoint = result
                .map_err(|_| ())
                .and_then(|bytes| directory_endpoint(&bytes, &directory));
            match endpoint {
                Ok((url, name)) => {
                    net::public_directory::fetch_document(url, RESPONSE_LIMIT, move |result| {
                        let entries = result
                            .map_err(|_| ())
                            .and_then(|bytes| parse_entries(&bytes, false));
                        bridge.finish_addon_browser(
                            context,
                            index,
                            generation,
                            Some(name),
                            entries,
                        );
                    });
                }
                Err(()) => bridge.finish_addon_browser(context, index, generation, None, Err(())),
            }
        });
    }

    fn finish_addon_browser(
        &self,
        context: profiles::Context,
        index: usize,
        generation: u64,
        name: Option<String>,
        result: Result<Vec<Entry>, ()>,
    ) {
        let bridge = self.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if !profiles::is_current(&context) {
                return;
            }
            profiles::with_context(context, || {
                {
                    let mut state = bridge.shared.lock().unwrap();
                    let source = &mut state.addon_browser.sources[index];
                    if source.generation != generation {
                        return;
                    }
                    source.busy = false;
                    source.failed = result.is_err();
                    if let Ok(entries) = result {
                        source.entries = entries;
                        source.fetched = Some(Instant::now());
                        if let Some(name) = name {
                            source.name = name;
                        }
                    }
                }
                bridge.apply_addon_browser_rows();
            });
        });
    }

    pub(super) fn apply_addon_browser_rows(&self) {
        let Some(app) = self.app() else { return };
        let index = usize::from(app.get_addon_browser_source() == 1);
        let query = app.get_addon_browser_search().trim().to_lowercase();
        let filter = app.get_addon_browser_filter();
        let (rows, requests) = {
            let mut state = self.shared.lock().unwrap();
            let source = &state.addon_browser.sources[index];
            app.set_addon_browser_busy(source.busy);
            let stremio_addons = public_url(&source.url)
                .is_some_and(|url| url.host_str() == Some("stremio-addons.net"));
            app.set_addon_browser_source_name(if index == 0 {
                text::tr("Stremio official addons").into()
            } else if stremio_addons {
                "Stremio Addons".into()
            } else {
                source.name.clone().into()
            });
            app.set_addon_browser_source_url(if stremio_addons {
                "https://stremio-addons.net".into()
            } else {
                source.url.clone().into()
            });
            let status = state.addon_browser.notice.unwrap_or(if source.failed {
                "Could not load this directory. Retry to try again."
            } else if source.busy {
                "Loading addon directory…"
            } else {
                ""
            });
            app.set_addon_browser_status(text::tr(status).into());
            let mut requests = HashSet::new();
            let rows = source
                .entries
                .iter()
                .filter(|entry| entry_matches(entry, &query, filter))
                .map(|entry| {
                    let logo = entry
                        .manifest
                        .logo
                        .as_deref()
                        .and_then(|url| {
                            let pixels =
                                decoded_cache_get(&image_cache_key(url, ImageUse::Fixed(Some(64))));
                            if pixels.is_none() {
                                requests.insert(url.to_string());
                            }
                            pixels
                        })
                        .map(Image::from_rgba8)
                        .unwrap_or_default();
                    AddonBrowserRow {
                        label: entry.manifest.name.clone().into(),
                        description: entry
                            .manifest
                            .description
                            .clone()
                            .unwrap_or_default()
                            .into(),
                        capabilities: capability_caption(&entry.manifest).into(),
                        url: entry.url.clone().into(),
                        logo,
                        initial: entry
                            .manifest
                            .name
                            .chars()
                            .next()
                            .unwrap_or('?')
                            .to_uppercase()
                            .to_string()
                            .into(),
                        configurable: entry.configurable,
                        installed: state.installed.iter().any(|a| {
                            a.available
                                && (a.url == entry.url
                                    || (entry.configurable && a.manifest.id == entry.manifest.id))
                        }),
                        busy: state.refreshing.contains(&entry.url),
                    }
                })
                .collect::<Vec<_>>();
            let browser = &mut state.addon_browser;
            let requests = requests
                .into_iter()
                .filter(|url| !browser.logos_requested.contains(url))
                .take(8usize.saturating_sub(browser.logos_active))
                .collect::<Vec<_>>();
            browser.logos_requested.extend(requests.iter().cloned());
            browser.logos_active += requests.len();
            (rows, requests)
        };
        app.set_addon_browser_rows(Rc::new(VecModel::from(rows)).into());
        // Only the bounded result set requests logos; at most eight run
        // concurrently, and each completion lets the next batch progress.
        for url in requests {
            let bridge = self.clone();
            let context = profiles::capture();
            let revision = image_settings_revision();
            net::public_directory::fetch_logo(url.clone(), move |pixels| {
                let loaded = pixels.is_some();
                let _ = slint::invoke_from_event_loop(move || {
                    if profiles::is_current(&context) {
                        profiles::with_context(context, || {
                            let mut state = bridge.shared.lock().unwrap();
                            state.addon_browser.logos_active =
                                state.addon_browser.logos_active.saturating_sub(1);
                            if loaded || revision != image_settings_revision() {
                                state.addon_browser.logos_requested.remove(&url);
                            }
                            drop(state);
                            bridge.apply_addon_browser_rows();
                        });
                    }
                });
            });
        }
    }

    pub(super) fn addon_browser_action(&self, url: &str) {
        let Some(app) = self.app() else { return };
        let index = usize::from(app.get_addon_browser_source() == 1);
        let entry = self.shared.lock().unwrap().addon_browser.sources[index]
            .entries
            .iter()
            .find(|entry| entry.url == url)
            .cloned();
        let Some(entry) = entry else { return };
        if entry.configurable {
            self.shared.lock().unwrap().addon_browser.notice = Some(
                "Complete configuration in your browser, then paste the configured install URL into Nova's Installed tab.",
            );
            if crate::player::open_browser(&format!(
                "{}/configure",
                entry.url.trim_end_matches('/')
            ))
            .is_err()
            {
                self.shared.lock().unwrap().addon_browser.notice = Some(
                    "Could not open the browser. Copy the addon URL and configure it in your browser.",
                );
            }
        } else {
            self.shared.lock().unwrap().addon_browser.notice = None;
            self.add_addon(&entry.url);
        }
        self.apply_addon_browser_rows();
    }

    pub(super) fn addon_browser_install_error(&self, url: &str) {
        let mut state = self.shared.lock().unwrap();
        if state
            .addon_browser
            .sources
            .iter()
            .any(|source| source.entries.iter().any(|entry| entry.url == url))
        {
            state.addon_browser.notice = Some(
                "Addon installation failed. You can retry Install or use its URL in the Installed tab.",
            );
        }
        drop(state);
        self.apply_addon_browser_rows();
    }

    pub(super) fn open_addon_browser_source(&self) {
        let Some(app) = self.app() else { return };
        let url = app.get_addon_browser_source_url();
        if public_url(&url).is_some() {
            let _ = crate::player::open_browser(&url);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn listing(url: &str, hints: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"transportUrl":url,"manifest":{"id":"test.addon","name":"Fixture","version":"1","description":"Find me","resources":["stream","subtitles"],"behaviorHints":hints}})
    }
    #[test]
    fn parses_both_directory_shapes_and_filters_untrusted_rows() {
        let valid = listing(
            "https://addon.example/config/manifest.json",
            serde_json::json!({"configurationRequired":true}),
        );
        let rows = serde_json::json!([valid.clone(), valid.clone(), listing("http://127.0.0.1/manifest.json", serde_json::json!({})), listing("file:///tmp/manifest.json", serde_json::json!({})), listing("https://addon.example/manifest.json?token=x", serde_json::json!({})), {"manifest":{}}]);
        let official = parse_entries(&serde_json::to_vec(&rows).unwrap(), true).unwrap();
        assert_eq!(official.len(), 1);
        assert_eq!(official[0].url, "https://addon.example/config");
        assert!(official[0].configurable);
        assert!(entry_matches(&official[0], "find", 3));
        assert!(!entry_matches(&official[0], "find", 1));
        let standard = serde_json::json!({"addons":[valid]});
        assert_eq!(
            parse_entries(&serde_json::to_vec(&standard).unwrap(), false)
                .unwrap()
                .len(),
            1
        );
        assert!(parse_entries(b"{}", false).is_err());
    }
    #[test]
    fn directory_manifest_uses_declared_catalog_and_safe_path_segments() {
        let raw = br#"{"id":"directory","version":"1","name":"Directory","resources":["addon_catalog"],"addonCatalogs":[{"type":"all","id":"a/b?query"}]}"#;
        let (url, name) = directory_endpoint(raw, "https://directory.example/base").unwrap();
        assert_eq!(
            url,
            "https://directory.example/base/addon_catalog/all/a%2Fb%3Fquery.json"
        );
        assert_eq!(name, "Directory");
        assert!(
            directory_endpoint(
                br#"{"id":"a","version":"1","name":"A"}"#,
                "https://directory.example"
            )
            .is_err()
        );
        for url in [
            "https://localhost/manifest.json",
            "https://source.local/manifest.json",
            "https://user:secret@addon.example/manifest.json",
            "stremio://addon.example/manifest.json",
            "https://[::1]/manifest.json",
        ] {
            assert!(public_url(url).is_none(), "{url}");
        }
    }
}
