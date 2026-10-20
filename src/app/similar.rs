//! Bounded suggestions from the current profile's installed metadata sources.
use super::*;
use std::time::Instant;

const LIMIT: usize = 24;
const RESPONSE_LIMIT: usize = 2 * 1024 * 1024;
const TTL: Duration = Duration::from_secs(15 * 60);

#[derive(Default)]
pub(super) struct Cache {
    entries: HashMap<String, (Instant, Vec<MetaPreview>)>,
}

fn inventory_key(installed: &[Installed]) -> String {
    serde_json::to_string(
        &installed
            .iter()
            .filter(|addon| addon.enabled && addon.available)
            .map(|addon| (&addon.url, addon.generation, &addon.manifest))
            .collect::<Vec<_>>(),
    )
    .unwrap_or_default()
}

impl Bridge {
    /// Completion runs on a worker in the originating profile context. Callers
    /// publishing Slint models must use profiles::invoke_from_event_loop.
    pub(super) fn fetch_similar(
        &self,
        type_: &str,
        id: &str,
        then: impl FnOnce(Vec<MetaPreview>) + Send + 'static,
    ) {
        let type_ = type_.to_owned();
        let id = id.to_owned();
        let context = profiles::capture();
        let settings = context
            .session
            .as_ref()
            .and_then(|session| session.providers.get())
            .cloned()
            .unwrap_or_else(nova_providers::plugin_settings::capture_settings_capability);
        let settings_generation =
            settings.with(nova_providers::plugin_settings::settings_generation);
        let inventory = if profiles::enabled() {
            context
                .session
                .as_ref()
                .and_then(|session| session.provider_inventory().ok())
        } else {
            Some(nova_providers::OwnedProviderSnapshot::capture())
        };
        let (installed, cache) = {
            let state = self.shared.lock().unwrap();
            (state.installed.clone(), state.similar_cache.clone())
        };
        let authority = inventory_key(&installed);
        let key = serde_json::to_string(&(&authority, settings_generation, &type_, &id))
            .unwrap_or_default();
        let shared = self.shared.clone();
        profiles::thread::spawn(move || {
            profiles::with_context(context.clone(), || {
                let Some(inventory) = inventory else {
                    then(Vec::new());
                    return;
                };
                inventory.with(|| {
                    settings.with(|| {
                        let current = || {
                            profiles::is_current(&context)
                                && !context.session.as_ref().is_some_and(|session| {
                                    session.cancelled.load(Ordering::Acquire)
                                })
                                && settings.is_current()
                                && nova_providers::plugin_settings::settings_generation()
                                    == settings_generation
                                && inventory_key(&shared.lock().unwrap().installed) == authority
                        };
                        if !current() {
                            then(Vec::new());
                            return;
                        }
                        let cached = cache
                            .lock()
                            .unwrap()
                            .entries
                            .get(&key)
                            .filter(|(at, _)| at.elapsed() < TTL)
                            .map(|(_, rows)| rows.clone());
                        if let Some(rows) = cached {
                            then(rows);
                            return;
                        }
                        let started = Instant::now();
                        let mut remaining_requests = 4;
                        let mut fetch = |url: &str| {
                            let remaining =
                                Duration::from_secs(12).saturating_sub(started.elapsed());
                            if remaining_requests == 0 || remaining.is_zero() || !current() {
                                return None;
                            }
                            remaining_requests -= 1;
                            net::fetch_metadata_bytes(
                                url,
                                remaining.min(Duration::from_secs(4)),
                                RESPONSE_LIMIT,
                            )
                            .ok()
                        };
                        let rows = retrieve(&installed, &type_, &id, &mut fetch);
                        if !current() {
                            then(Vec::new());
                            return;
                        }
                        let mut cache = cache.lock().unwrap();
                        cache.entries.retain(|_, (at, _)| at.elapsed() < TTL);
                        if cache.entries.len() >= 64
                            && let Some(oldest) = cache
                                .entries
                                .iter()
                                .min_by_key(|(_, (at, _))| *at)
                                .map(|(key, _)| key.clone())
                        {
                            cache.entries.remove(&oldest);
                        }
                        cache.entries.insert(key, (Instant::now(), rows.clone()));
                        drop(cache);
                        then(rows);
                    });
                });
            });
        });
    }

    pub(super) fn load_detail_similar(&self) {
        if !self
            .app()
            .is_some_and(|app| app.get_detail_tab() == 0 && !app.get_stream_selector_open())
        {
            return;
        }
        let Some((type_, id, token)) = self
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_ref()
            .filter(|modal| modal.details.related.is_empty())
            .map(|m| (m.type_.clone(), m.id.clone(), m.open_token.clone()))
        else {
            return;
        };
        let authority = inventory_key(&self.shared.lock().unwrap().installed);
        let settings = nova_providers::plugin_settings::capture_settings_capability();
        let generation = settings.with(nova_providers::plugin_settings::settings_generation);
        let bridge = self.clone();
        self.fetch_similar(&type_.clone(), &id.clone(), move |rows| {
            let _ = profiles::invoke_from_event_loop(move || {
                if !settings.is_current()
                    || settings.with(nova_providers::plugin_settings::settings_generation)
                        != generation
                {
                    return;
                }
                let changed = {
                    let mut state = bridge.shared.lock().unwrap();
                    if inventory_key(&state.installed) != authority {
                        return;
                    }
                    if let Some(modal) = state.modal_item.as_mut()
                        && modal.type_ == type_
                        && modal.id == id
                        && Arc::ptr_eq(&modal.open_token, &token)
                        && modal.details.related.is_empty()
                        && !rows.is_empty()
                    {
                        modal.details.related = rows;
                        true
                    } else {
                        false
                    }
                };
                if changed {
                    bridge.publish_detail_extras();
                }
            });
        });
    }
}

fn retrieve(
    installed: &[Installed],
    type_: &str,
    id: &str,
    fetch: &mut impl FnMut(&str) -> Option<Vec<u8>>,
) -> Vec<MetaPreview> {
    // Without the originating catalog URL an opaque ID cannot establish
    // identity across wildcard manifests. Explicit Detail extensions still
    // work through their original metadata response.
    if type_.is_empty()
        || id.is_empty()
        || id.len() > 512
        || id.chars().any(char::is_control)
        || nova_providers::ExternalId::parse(id).is_none()
    {
        return Vec::new();
    }
    let enabled = installed.iter().filter(|a| a.enabled && a.available);
    let mut seed = None;
    for addon in enabled
        .clone()
        .filter(|a| a.manifest.accepts("meta", type_, id))
        .take(2)
    {
        let Ok(endpoint) = Addon::new(&addon.url) else {
            continue;
        };
        let Some(item) = fetch(&endpoint.meta_url(type_, id))
            .and_then(|bytes| Addon::parse_meta(&bytes).ok().flatten())
            .filter(|item| detail::meta_matches_request(item, type_, id))
        else {
            continue;
        };
        let mut explicit = detail_extras::DetailExtras::from_item(&item).related;
        if !explicit.is_empty() {
            for preview in &mut explicit {
                // Opaque provider IDs must keep the installed source that
                // actually supplied them, including related-title extensions.
                preview
                    .extra
                    .insert("novaSourceUrl".into(), addon.url.clone().into());
            }
            return explicit;
        }
        if seed.is_none()
            || seed
                .as_ref()
                .is_some_and(|old: &MetaItem| old.preview.genres.is_empty())
        {
            seed = Some(item);
        }
    }
    let Some(seed) = seed else { return Vec::new() };
    let mut urls = Vec::new();
    for addon in enabled {
        let Ok(endpoint) = Addon::new(&addon.url) else {
            continue;
        };
        for catalog in catalog::browsable_catalogs(&addon.manifest).filter(|c| c.type_ == type_) {
            if catalog
                .extra
                .iter()
                .any(|extra| extra.is_required && extra.name != "genre")
            {
                continue;
            }
            let Some(genre_extra) = catalog.extra.iter().find(|e| e.name == "genre") else {
                continue;
            };
            for genre in seed.preview.genres.iter().take(12) {
                let genre = if genre_extra.options.is_empty() && !genre_extra.options_required {
                    Some(genre.as_str())
                } else {
                    genre_extra
                        .options
                        .iter()
                        .find(|g| g.eq_ignore_ascii_case(genre))
                        .map(String::as_str)
                };
                if let Some(genre) = genre {
                    let url = endpoint.catalog_url(type_, &catalog.id, &[("genre", genre)]);
                    if !urls.iter().any(|(existing, _)| existing == &url) && urls.len() < 2 {
                        urls.push((url, addon.url.clone()));
                    }
                }
            }
        }
    }
    let candidates = urls
        .iter()
        .filter_map(|(url, owner)| {
            let bytes = fetch(url)?;
            let rows = Addon::parse_catalog(&bytes).ok()?;
            Some(
                rows.into_iter()
                    .take(100)
                    .map(|mut preview| {
                        preview
                            .extra
                            .insert("novaSourceUrl".into(), owner.clone().into());
                        preview
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .flatten()
        .collect();
    rank(&seed, candidates)
}

fn labels(values: &[String]) -> std::collections::BTreeSet<String> {
    values
        .iter()
        .take(100)
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

fn rank(seed: &MetaItem, candidates: Vec<MetaPreview>) -> Vec<MetaPreview> {
    let genres = labels(&seed.preview.genres);
    let people = labels(
        &seed
            .cast
            .iter()
            .chain(&seed.director)
            .cloned()
            .collect::<Vec<_>>(),
    );
    let mut seen = std::collections::HashSet::new();
    let mut scored = candidates
        .into_iter()
        .filter_map(|candidate| {
            if candidate.type_ != seed.preview.type_
                || candidate.id == seed.preview.id
                || candidate.id.is_empty()
                || candidate.id.len() > 512
                || candidate.id.chars().any(char::is_control)
                || candidate.title().trim().is_empty()
                || !seen.insert((candidate.type_.clone(), candidate.id.clone()))
            {
                return None;
            }
            let shared_genres = genres.intersection(&labels(&candidate.genres)).count();
            let candidate_people = ["cast", "director"]
                .into_iter()
                .filter_map(|key| {
                    candidate
                        .extra
                        .get(key)
                        .and_then(serde_json::Value::as_array)
                })
                .flatten()
                .take(100)
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let shared_people = people.intersection(&labels(&candidate_people)).count();
            // One broad genre or a word in the title is not enough evidence.
            (shared_genres >= 2 || shared_people > 0)
                .then_some((shared_genres + shared_people * 3, candidate))
        })
        .collect::<Vec<_>>();
    scored.sort_by_key(|row| std::cmp::Reverse(row.0));
    scored.into_iter().take(LIMIT).map(|(_, row)| row).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ranks_only_metadata_evidence_and_excludes_self_duplicates_and_other_types() {
        let seed: MetaItem = serde_json::from_value(json!({"id":"seed","type":"series","name":"Example", "genres":["Drama","Crime"], "cast":["Actor"]})).unwrap();
        let rows = json!([
            {"id":"word","type":"series","name":"Example sequel","genres":["Drama"]},
            {"id":"good","type":"series","name":"Different","genres":["crime","Drama"]},
            {"id":"good","type":"series","name":"Duplicate","genres":["Crime","Drama"]},
            {"id":"person","type":"series","name":"Cast match","cast":["actor"]},
            {"id":"seed","type":"series","name":"Self","genres":["Crime","Drama"]},
            {"id":"movie","type":"movie","name":"Wrong type","genres":["Crime","Drama"]}
        ]);
        let ranked = rank(&seed, serde_json::from_value(rows).unwrap());
        assert_eq!(
            ranked.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            ["person", "good"]
        );
    }

    #[test]
    fn installed_authority_exact_identity_and_explicit_related_win() {
        let mut addon = Installed {
            url: "https://installed.example".into(), label: "Installed".into(), enabled: true,
            configure_ok: None, available: true, generation: 1,
            manifest: serde_json::from_value(json!({"id":"test", "version":"1.0.0", "name":"Test", "resources":["meta","catalog"], "types":["series"], "catalogs":[{"id":"top", "type":"series", "name":"Top", "extra":[{"name":"genre","options":["Crime"]}]}]})).unwrap(),
        };
        let mut calls = 0;
        assert!(
            retrieve(&[addon.clone()], "series", "private:123", &mut |_| {
                panic!("opaque seed must not query wildcard sources")
            })
            .is_empty()
        );
        let explicit = retrieve(&[addon.clone()], "series", "tt100", &mut |url| {
            calls += 1;
            assert!(url.starts_with("https://installed.example/meta/series/tt100.json"));
            Some(serde_json::to_vec(&json!({"meta":{"id":"tt100", "type":"series", "name":"Seed", "related":[{"id":"related","type":"series","name":"Actual relation", "novaSourceUrl":"https://forged.example"}]}})).unwrap())
        });
        assert_eq!(calls, 1);
        assert_eq!(explicit[0].id, "related");
        assert_eq!(
            explicit[0].extra["novaSourceUrl"],
            "https://installed.example"
        );
        addon.enabled = false;
        assert!(
            retrieve(&[addon.clone()], "series", "tt100", &mut |_| panic!(
                "disabled source requested"
            ))
            .is_empty()
        );
        addon.enabled = true;
        assert!(retrieve(&[addon], "series", "tt100", &mut |_| Some(br#"{"meta":{"id":"wrong","type":"series","name":"Wrong","genres":["Drama","Crime"]}}"#.to_vec())).is_empty());
    }

    #[test]
    fn fallback_uses_only_advertised_encoded_genres_and_bounded_catalogs() {
        let manifest = serde_json::from_value(json!({
            "id":"genres", "version":"1.0.0", "name":"Genres", "types":["series"],
            "resources":["meta", "catalog"], "catalogs":[
                {"id":"search-only", "type":"series", "name":"Search", "extra":[{"name":"search", "isRequired":true}, {"name":"genre"}]},
                {"id":"no-filter", "type":"series", "name":"No filter"},
                {"id":"popular", "type":"series", "name":"Popular", "extra":[{"name":"genre", "options":["Action & Adventure", "Crime", "Drama"]}]},
                {"id":"other", "type":"series", "name":"Other", "extra":[{"name":"genre"}]}
            ]
        })).unwrap();
        let addon = Installed {
            url: "https://installed.example".into(),
            label: "Installed".into(),
            enabled: true,
            configure_ok: None,
            available: true,
            generation: 1,
            manifest,
        };
        let mut requests = Vec::new();
        let rows = retrieve(&[addon], "series", "tt100", &mut |url| {
            requests.push(url.to_owned());
            Some(serde_json::to_vec(&if url.contains("/meta/") {
                json!({"meta":{"id":"tt100", "type":"series", "name":"Seed", "genres":["Not advertised", "Action & Adventure", "Crime", "Drama"]}})
            } else {
                json!({"metas":[
                    {"id":"related", "type":"series", "name":"Related", "genres":["Crime", "Drama"], "novaSourceUrl":"https://forged.example"},
                    {"id":"broad", "type":"series", "name":"Unrelated", "genres":["Drama"]}
                ]})
            }).unwrap())
        });
        assert_eq!(requests.len(), 3);
        assert!(
            requests[1].contains("/catalog/series/popular/genre=Action%20%26%20Adventure.json")
        );
        assert!(requests[2].contains("/catalog/series/popular/genre=Crime.json"));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "related");
        assert_eq!(rows[0].extra["novaSourceUrl"], "https://installed.example");
    }
}
