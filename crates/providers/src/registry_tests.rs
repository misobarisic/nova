use super::*;
use std::{
    collections::BTreeMap,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct CanonicalMetadata {
    media: Option<MediaItem>,
    episodes: Vec<EpisodeStreamIds>,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct EpisodeStreamIds {
    number: u32,
    ids: Vec<String>,
    metadata: Option<MappedVideoMetadata>,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct MappedVideoMetadata {
    season: u32,
    episode: u32,
    title: Option<String>,
    released: Option<String>,
    thumbnail: Option<String>,
    overview: Option<String>,
}

fn lookup_canonical_metadata(
    provider: &JsProvider,
    host: Arc<dyn ProviderHost>,
    canonical_host: &dyn ProviderHost,
    media: &MediaItem,
    details: &Value,
) -> Result<CanonicalMetadata, ProviderHostError> {
    let sequences = provider
        .call(
            host,
            json!({"op":"sourceSequences","source":details["mappingContext"]}),
        )
        .map_err(|e| ProviderHostError(e.to_string()))?;
    let sources: Vec<crate::SourceSequence> =
        serde_json::from_value(sequences).map_err(|e| ProviderHostError(e.to_string()))?;
    let addon = addons::Addon::new("https://v3-cinemeta.strem.io").unwrap();
    let response =
        canonical_host.get(&addon.meta_url("series", "tt5095466"), &Default::default())?;
    let canonical = addons::Addon::parse_meta(&response.body).unwrap().unwrap();
    let adapter = crate::StremioProvider::new(
        addon,
        serde_json::from_str(r#"{"id":"fixture","name":"Fixture","version":"1"}"#).unwrap(),
    );
    let item = adapter.convert_detail(canonical.clone()).0;
    let mappings = crate::sequence::map_episodes(&item, &canonical.videos, &sources);
    let native: Vec<Episode> = serde_json::from_value(details["episodes"].clone()).unwrap();
    let episodes = mappings
        .into_iter()
        .filter_map(|(id, target)| {
            let episode = native.iter().find(|e| e.stable_id() == id)?;
            let video = canonical.videos.iter().find(|v| v.id == target)?;
            let title = video.label();
            Some(EpisodeStreamIds {
                number: episode.number,
                ids: vec![target],
                metadata: Some(MappedVideoMetadata {
                    season: video.season?,
                    episode: video.episode_number()?,
                    title: (!crate::sequence::episode_title(&title).is_empty()).then_some(title),
                    released: video.released.clone(),
                    thumbnail: video.thumbnail.clone(),
                    overview: video.overview.clone().or_else(|| {
                        video
                            .extra
                            .get("description")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    }),
                }),
            })
        })
        .collect::<Vec<_>>();
    let _ = media;
    Ok(CanonicalMetadata {
        media: (!episodes.is_empty()).then_some(item),
        episodes,
    })
}

struct EnrichedFixtureHost {
    inner: Arc<dyn ProviderHost>,
    canonical: CanonicalMetadata,
}
impl ProviderHost for EnrichedFixtureHost {
    fn get(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
    ) -> Result<crate::HttpResponse, ProviderHostError> {
        self.inner.get(url, headers)
    }
    fn storage_get(&self, key: &str) -> Option<String> {
        self.inner.storage_get(key)
    }
    fn storage_set(&self, key: &str, value: &str) -> Result<(), ProviderHostError> {
        self.inner.storage_set(key, value)
    }
    fn log(&self, message: &str) {
        self.inner.log(message);
    }
    fn metadata_available(&self, _caller: &str) -> bool {
        true
    }
    fn metadata_enrich(
        &self,
        request: crate::EnrichmentRequest,
        _caller: &str,
        _budget: Duration,
    ) -> crate::EnrichmentResult {
        let mut details = request.details;
        if let Some(media) = &self.canonical.media {
            crate::metadata::merge_media(&mut details.media, media);
        }
        let mut episode_metadata = std::collections::BTreeMap::new();
        for mapping in &self.canonical.episodes {
            if let Some(episode) = details.episodes.iter().find(|e| e.number == mapping.number) {
                let mut value = crate::metadata::EpisodeEnrichment {
                    ids: mapping.ids.clone(),
                    ..Default::default()
                };
                if let Some(m) = &mapping.metadata {
                    value.season = Some(m.season);
                    value.episode = Some(m.episode);
                    value.title = m.title.clone();
                    value.released = m.released.clone();
                    value.thumbnail = m.thumbnail.clone();
                    value.overview = m.overview.clone();
                }
                episode_metadata.insert(episode.stable_id(), value);
            }
        }
        crate::EnrichmentResult {
            inventory_revision: crate::metadata_revision(),
            status: "confirmed".into(),
            details,
            connections: vec![],
            episode_metadata,
        }
    }
}

struct FixtureHost {
    state: Arc<crate::MemoryProviderHost>,
    calls: Mutex<Vec<String>>,
    revision: AtomicUsize,
    embedded: std::sync::atomic::AtomicBool,
    foreign: AtomicUsize,
}

impl FixtureHost {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: crate::MemoryProviderHost::new(),
            calls: Mutex::new(Vec::new()),
            revision: AtomicUsize::new(1),
            embedded: std::sync::atomic::AtomicBool::new(false),
            foreign: AtomicUsize::new(0),
        })
    }
}

impl ProviderHost for FixtureHost {
    fn get(
        &self,
        raw_url: &str,
        headers: &BTreeMap<String, String>,
    ) -> Result<crate::HttpResponse, ProviderHostError> {
        self.calls.lock().unwrap().push(raw_url.into());
        let url = Url::parse(raw_url).unwrap();
        let revision = self.revision.load(Ordering::Relaxed);
        let foreign = self.foreign.load(Ordering::Relaxed);
        let body = match url.path() {
                "/filter" if foreign > 0 => {
                    let shows = if foreign == 3 {
                        vec![("long-show", "Long Show", "TV")]
                    } else {
                        let mut shows = vec![("geass", "Code Geass: Lelouch of the Rebellion", "TV"), ("geass-r2", "Code Geass: Lelouch of the Rebellion R2", "TV"), ("geass-movie", "Code Geass: Lelouch of the Rebellion Movie", "Movie")];
                        if foreign == 2 { shows.push(("geass-copy", "Code Geass: Lelouch of the Rebellion", "TV")); }
                        shows
                    };
                    let mut html = String::from("<div class='ani items'>");
                    for (path, title, kind) in shows {
                        html.push_str(&format!("<div class='item'><a class='name' href='/watch/{path}'>{title}</a><div class='right'>{kind}</div></div>"));
                    }
                    html.push_str("</div>");
                    html
                }
                "/most-viewed/" | "/latest-updated/" | "/filter" => {
                    let page = url.query_pairs().find(|(key, _)| key == "page").unwrap().1;
                    let count = if page == "1" { 24 } else { 1 };
                    let mut html = String::from("<div class='ani items'>");
                    for index in 0..count {
                        html.push_str(&format!("<div class='item'><a class='name' href='/watch/show-{page}-{index}.abc/ep-1'>Show {index}</a><div class='poster'><img data-src='/poster.jpg'></div></div>"));
                    }
                    html.push_str("</div>");
                    if page == "1" { html.push_str("<nav><ul class='pagination'><li class='active'>1</li><li>2</li></ul></nav>"); }
                    html
                }
                "/watch/show.abc" => "<div data-id='abc'></div><h1 class='title'>Show</h1><div class='poster'><img src='/poster.jpg'></div><div class='synopsis'><div class='shorting'><div class='content'>Synopsis</div></div></div><div class='names font-italic'>Alias; Another</div><div class='bmeta'><a href='/genre/action'>Action</a><a href='/filter?year=2020'>2020</a></div>".into(),
                "/watch/geass" | "/watch/geass-copy" | "/watch/geass-r2" | "/watch/long-show" => {
                    let (id, title, year) = match url.path() {
                        "/watch/geass-r2" => ("r2", "Code Geass: Lelouch of the Rebellion R2", "2008"),
                        "/watch/long-show" => ("long", "Long Show", "2000"),
                        _ => ("geass", "Code Geass: Lelouch of the Rebellion", "2006"),
                    };
                    format!("<div data-id='{id}'></div><h1 class='title'>{title}</h1><div class='bmeta'><a href='/filter?year={year}'>{year}</a></div>")
                }
                "/ajax/episode/list/geass" | "/ajax/episode/list/r2" | "/ajax/episode/list/long" => {
                    let episodes = if url.path().ends_with("/r2") { vec![(1, "The Day a Demon Awakens")] }
                        else if url.path().ends_with("/long") { vec![(1,"First"),(2,"Second"),(3,"Third")] }
                        else { vec![(1,"The Day a New Demon Was Born")] };
                    let mut html = String::from("<div class='episodes'><ul>");
                    for (number, title) in episodes {
                        html.push_str(&format!("<li><a data-num='{number}' data-ids='token-{revision}' data-mal='27' data-slug='show' data-timestamp='1700000001'><span class='d-title'>{title}</span></a></li>"));
                    }
                    html.push_str("</ul></div>");
                    json!({"result":html}).to_string()
                }
                "/ajax/episode/list/abc" => {
                    let vrf = url.query_pairs().find(|(key, _)| key == "vrf").unwrap().1;
                    assert_eq!(vrf, "cEpfZl93dmttS2M9");
                    json!({"result":format!("<div class='episodes'><ul><li><a data-num='1' data-ids='token-{revision}' data-mal='27' data-slug='show' data-timestamp='170000000{revision}'>1</a><span class='d-title'>The start</span></li></ul></div>")}).to_string()
                }
                "/ajax/server/list" if self.embedded.load(Ordering::Relaxed) => json!({"result":"<div class='servers'><div class='type' data-type='sub'><label>SUB</label><ul><li data-link-id='broken'>Broken</li><li data-link-id='image'>HD-1</li><li data-link-id='encrypted'>Vidstream-2</li><li data-link-id='plain'>Plain</li><li data-link-id='mew'>Mewcdn</li></ul></div></div>"}).to_string(),
                "/ajax/server/list" => json!({"result":"<div class='servers'><div class='type' data-type='sub'><label>Sub</label><ul><li data-link-id='direct'>Direct</li><li data-link-id='embed'>Embedded</li><li class='download-icon' data-link-id='download'>Download</li></ul></div></div>"}).to_string(),
                "/ajax/server" if self.embedded.load(Ordering::Relaxed) => {
                    let id = url.query_pairs().find(|(key, _)| key == "get").unwrap().1;
                    let target = match id.as_ref() {
                        "image" => "https://megaplay.buzz/stream/image?s=tcdn".to_owned(),
                        "mew" => {
                            use base64::Engine;
                            format!("https://mewcdn.online/player/plyr.php#{}", base64::engine::general_purpose::STANDARD.encode("https://origin.example/master.m3u8"))
                        }
                        _ => format!("https://megaplay.buzz/stream/{id}"),
                    };
                    json!({"result":{"url":target}}).to_string()
                }
                "/ajax/server" => {
                    let id = url.query_pairs().find(|(key, _)| key == "get").unwrap().1;
                    json!({"result":{"url":if id == "direct" { "https://media.example/video.mp4" } else { "https://media.example/embed/123" }}}).to_string()
                }
                "/stream/broken" => return Err(ProviderHostError("fixture host unavailable".into())),
                "/stream/encrypted" => "<main data-id='encrypted'>File encrypted</main>".into(),
                "/stream/plain" => "<title>File 99</title>".into(),
                "/stream/getSources" => {
                    assert_eq!(headers.get("X-Requested-With").map(String::as_str), Some("XMLHttpRequest"));
                    let id = url.query_pairs().find(|(key, _)| key == "id").unwrap().1;
                    if id == "encrypted" {
                        json!({"enc":"wdeBruh3qqn_i5wUNnyaPcjCE_9a111S_RppcCnRoC38amAaffSKD9TBNQPLAaJLu1Wubwd8BmyjYHWpom9qr-XFCEplG1DLJvALczHfag1WT_z1sqOWQX1RMYpfz9K3GpZPSOlwXodCezSyKekkeQ","tracks":[{"file":"https://cdn.example/chi.vtt","label":"Chinese (Simplified)","kind":"captions"},{"file":"https://cdn.example/eng.vtt","label":"English","kind":"captions"},{"file":"https://cdn.example/thumbs.vtt","label":"Thumbnails","kind":"thumbnails"}]}).to_string()
                    } else {
                        assert_eq!(id, "99");
                        json!({"enc":"invalid", "sources":"https://cdn.example/plain/master.m3u8?token=existing"}).to_string()
                    }
                }
                "/player/plyr.php" => "<script>var HOST_MAP = {'origin.example':'proxy.example'};</script>".into(),
                "/api/mal/27/show/1700000002" | "/api/mal/27/show/1700000001" => json!({"status":true,"Mapper":{"sub":{"url":"https://media.example/master.m3u8"},"dub":{"url":"https://media.example/stream/embed.mp4"}}}).to_string(),
                path if path.starts_with("/catalog/") => json!({"metas":[{"id":"tt123","type":"series","name":"Show","releaseInfo":"2020","background":"https://image.example/bg"},{"id":"tt456","type":"series","name":"Show","releaseInfo":"2000"}]}).to_string(),
                _ => return Err(ProviderHostError(format!("unexpected fixture URL {raw_url}"))),
            };
        Ok(crate::HttpResponse {
            status: 200,
            final_url: raw_url.into(),
            body: body.into_bytes(),
            content_type: None,
        })
    }
    fn storage_get(&self, key: &str) -> Option<String> {
        self.state.storage_get(key)
    }
    fn storage_set(&self, key: &str, value: &str) -> Result<(), ProviderHostError> {
        self.state.storage_set(key, value)
    }
    fn log(&self, _message: &str) {}
}

fn provider() -> JsProvider {
    JsProvider::new(
        serde_json::from_str(bundled_providers()[0].plugin_manifest).unwrap(),
        serde_json::from_str(bundled_providers()[0].addon_manifest).unwrap(),
        bundled_providers()[0].source,
    )
}

fn request(url: &str) -> ProtocolRequest {
    parse_protocol_request(&decoded_path_segments(&Url::parse(url).unwrap()), "anikoto").unwrap()
}

#[test]
fn catalogs_use_real_page_size_and_source_scoped_ids() {
    let host = FixtureHost::new();
    let addon = addons::Addon::new(ANIKOTO_PROVIDER_URL).unwrap();
    let provider = provider();
    let first = provider
        .protocol_response(
            host.clone(),
            request(&addon.catalog_url("series", "anikoto.popular", &[])),
        )
        .unwrap();
    let first = addons::Addon::parse_catalog(&serde_json::to_vec(&first).unwrap()).unwrap();
    assert_eq!(first.len(), 24);
    assert!(first[0].id.starts_with("anikoto:"));
    assert_eq!(
        first[0].poster.as_deref(),
        Some("https://anikototv.to/poster.jpg")
    );
    let second = provider
        .protocol_response(
            host.clone(),
            request(&addon.catalog_url("series", "anikoto.popular", &[("skip", "24")])),
        )
        .unwrap();
    assert_eq!(second["metas"].as_array().unwrap().len(), 1);
    let end = provider
        .protocol_response(
            host.clone(),
            request(&addon.catalog_url("series", "anikoto.popular", &[("skip", "25")])),
        )
        .unwrap();
    assert!(end["metas"].as_array().unwrap().is_empty());
    let calls = host.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert!(calls[1].ends_with("page=2"));
    assert!(
        serde_json::from_str::<Manifest>(bundled_providers()[0].addon_manifest)
            .unwrap()
            .accepts("meta", "series", &first[0].id)
    );
    assert!(
        !serde_json::from_str::<Manifest>(bundled_providers()[0].addon_manifest)
            .unwrap()
            .accepts("stream", "series", "tt123:1:1")
    );
}

#[test]
fn fresh_tokens_keep_episode_ids_and_filter_embed_pages() {
    let host = FixtureHost::new();
    let addon = addons::Addon::new(ANIKOTO_PROVIDER_URL).unwrap();
    let provider = provider();
    let media_id = "anikoto:L3dhdGNoL3Nob3cuYWJj";
    let first = provider
        .protocol_response(host.clone(), request(&addon.meta_url("series", media_id)))
        .unwrap();
    let meta = addons::Addon::parse_meta(&serde_json::to_vec(&first).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(meta.videos.len(), 1);
    assert_eq!(meta.videos[0].name, "Episode 1: The start");
    assert_eq!(first["meta"]["mal_id"], "27");
    assert_eq!(first["meta"]["year"], "2020");
    host.revision.store(2, Ordering::Relaxed);
    let second = provider
        .protocol_response(host.clone(), request(&addon.meta_url("series", media_id)))
        .unwrap();
    assert_eq!(
        first["meta"]["videos"][0]["id"],
        second["meta"]["videos"][0]["id"]
    );
    let streams = provider
        .protocol_response(
            host.clone(),
            request(&addon.stream_url("series", &meta.videos[0].id)),
        )
        .unwrap();
    let streams = addons::Addon::parse_streams(&serde_json::to_vec(&streams).unwrap()).unwrap();
    assert_eq!(streams.len(), 2);
    assert_eq!(
        streams[0].url.as_deref(),
        Some("https://media.example/video.mp4")
    );
    assert_eq!(
        streams[0].extra["behaviorHints"]["proxyHeaders"]["request"]["Origin"],
        "https://anikototv.to"
    );
    assert!(
        host.calls
            .lock()
            .unwrap()
            .iter()
            .any(|url| url.ends_with("servers=token-2"))
    );
    assert!(
        !host
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|url| url.contains("get=download"))
    );
}

#[test]
fn encoded_search_and_vrf_preserve_unicode_and_padding() {
    let host = FixtureHost::new();
    let addon = addons::Addon::new(ANIKOTO_PROVIDER_URL).unwrap();
    provider()
        .protocol_response(
            host.clone(),
            request(&addon.catalog_url("series", "anikoto.search", &[("search", "A & B/東京")])),
        )
        .unwrap();
    let calls = host.calls.lock().unwrap();
    let url = Url::parse(&calls[0]).unwrap();
    let query = url.query_pairs().collect::<BTreeMap<_, _>>();
    assert_eq!(query["keyword"], "A & B/東京");
    assert_eq!(query["vrf"], "cEpfVnd6M2lwS01RMVBsQ0lOSEpnRUVwOEd6Rk1xY2Q=");
}

fn foreign_lookup(season: u32) -> StreamLookupRequest {
    StreamLookupRequest {
        media_id: "tt0994314".into(),
        media_type: "series".into(),
        title: "Code Geass".into(),
        year: Some("2006–2008".into()),
        season,
        episode: 1,
        absolute_episode: Some(if season == 1 { 1 } else { 26 }),
        season_episode_count: None,
        series_episode_count: None,
        episode_title: Some(
            if season == 1 {
                "The Day a New Demon Was Born"
            } else {
                "The Day a Demon Awakens"
            }
            .into(),
        ),
        released: Some(
            if season == 1 {
                "2006-10-05"
            } else {
                "2008-04-06"
            }
            .into(),
        ),
    }
}

#[test]
fn foreign_library_ids_resolve_separate_seasons_and_cache_only_source_mappings() {
    let host = FixtureHost::new();
    host.foreign.store(1, Ordering::Relaxed);
    let provider = provider();
    for (season, path) in [(1, "/watch/geass/ep-1"), (2, "/watch/geass-r2/ep-1")] {
        let lookup = foreign_lookup(season);
        let url = stream_lookup_url(ANIKOTO_PROVIDER_URL, &lookup).unwrap();
        let ProtocolRequest::Lookup { request: decoded } = request(&url) else {
            panic!("expected lookup")
        };
        assert_eq!(decoded, lookup);
        let result = provider
            .protocol_response(host.clone(), request(&url))
            .unwrap();
        let streams = addons::Addon::parse_streams(&serde_json::to_vec(&result).unwrap()).unwrap();
        assert_eq!(streams.len(), 2);
        assert_eq!(
            streams[0].extra["behaviorHints"]["proxyHeaders"]["request"]["Referer"],
            format!("https://anikototv.to{path}")
        );
        let searches = host
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|url| url.contains("/filter?"))
            .count();
        provider
            .protocol_response(host.clone(), request(&url))
            .unwrap();
        assert_eq!(
            host.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|url| url.contains("/filter?"))
                .count(),
            searches
        );
        // The input identifier is never rewritten into the source ID.
        assert_eq!(lookup.media_id, "tt0994314");
    }
}

#[test]
fn foreign_lookup_maps_continuous_episode_numbers_and_rejects_ambiguity() {
    let host = FixtureHost::new();
    host.foreign.store(3, Ordering::Relaxed);
    let mut lookup = foreign_lookup(2);
    lookup.media_id = "kitsu:123".into();
    lookup.title = "Long Show".into();
    lookup.year = Some("2000".into());
    lookup.released = Some("2001-01-01".into());
    lookup.absolute_episode = Some(3);
    lookup.episode_title = Some("Third".into());
    let result = provider()
        .protocol_response(
            host.clone(),
            request(&stream_lookup_url(ANIKOTO_PROVIDER_URL, &lookup).unwrap()),
        )
        .unwrap();
    assert_eq!(result["streams"].as_array().unwrap().len(), 2);
    assert_eq!(
        result["streams"][0]["behaviorHints"]["proxyHeaders"]["request"]["Referer"],
        "https://anikototv.to/watch/long-show/ep-3"
    );

    let host = FixtureHost::new();
    host.foreign.store(2, Ordering::Relaxed);
    let result = provider()
        .protocol_response(
            host.clone(),
            request(&stream_lookup_url(ANIKOTO_PROVIDER_URL, &foreign_lookup(1)).unwrap()),
        )
        .unwrap();
    assert!(result["streams"].as_array().unwrap().is_empty());
    assert!(
        !host
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|url| url.contains("/ajax/server"))
    );
}

#[test]
fn foreign_lookup_rejects_wrong_year_unknown_episodes_and_specials() {
    let host = FixtureHost::new();
    host.foreign.store(1, Ordering::Relaxed);
    let provider = provider();
    for (year, episode) in [("2020", 1), ("2006", 99)] {
        let mut lookup = foreign_lookup(1);
        lookup.year = Some(year.into());
        lookup.episode = episode;
        let result = provider
            .protocol_response(
                host.clone(),
                request(&stream_lookup_url(ANIKOTO_PROVIDER_URL, &lookup).unwrap()),
            )
            .unwrap();
        assert!(result["streams"].as_array().unwrap().is_empty());
    }
    let mut lookup = foreign_lookup(1);
    lookup.season = 0;
    assert!(stream_lookup_url(ANIKOTO_PROVIDER_URL, &lookup).is_err());
    lookup.season = u32::MAX;
    assert!(stream_lookup_url(ANIKOTO_PROVIDER_URL, &lookup).is_err());
    lookup.season = 1;
    lookup.title = "Title / 東京 & \"quoted\"".into();
    let ProtocolRequest::Lookup { request: decoded } =
        request(&stream_lookup_url(ANIKOTO_PROVIDER_URL, &lookup).unwrap())
    else {
        panic!("expected lookup")
    };
    assert_eq!(decoded, lookup);
}

struct MappingEntry {
    id: &'static str,
    title: &'static str,
    year: u32,
    count: u32,
    episodes: Vec<(u32, String)>,
}

fn mapping_entry(id: &'static str, title: &'static str, year: u32, count: u32) -> MappingEntry {
    MappingEntry {
        id,
        title,
        year,
        count,
        episodes: (1..=count)
            .map(|number| (number, format!("Episode {number}")))
            .collect(),
    }
}

struct MappingHost {
    inner: Arc<FixtureHost>,
    entries: Vec<MappingEntry>,
    hidden_entry: Option<&'static str>,
}

impl ProviderHost for MappingHost {
    fn get(
        &self,
        raw_url: &str,
        headers: &BTreeMap<String, String>,
    ) -> Result<crate::HttpResponse, ProviderHostError> {
        let url = Url::parse(raw_url).unwrap();
        let body = if url.path() == "/filter" {
            let mut html = String::from("<div class='ani items'>");
            for entry in &self.entries {
                if self.hidden_entry == Some(entry.id) {
                    continue;
                }
                html.push_str(&format!("<div class='item'><a class='name' href='/watch/{}'>{}</a><div class='right'>TV</div></div>", entry.id, entry.title));
            }
            html.push_str("</div>");
            html
        } else if let Some(entry) = self
            .entries
            .iter()
            .find(|entry| url.path() == format!("/watch/{}", entry.id))
        {
            // The live site uses year[]=, and declares the full episode
            // count separately from its currently available episode rows.
            format!(
                "<div data-id='{}'></div><h1 class='title'>{}</h1><div class='bmeta'><div>Premiered: <a href='/filter?year[]={}'>{}</a></div><div>Episodes: <span>{}</span></div></div>",
                entry.id, entry.title, entry.year, entry.year, entry.count
            )
        } else if let Some(entry) = self
            .entries
            .iter()
            .find(|entry| url.path() == format!("/ajax/episode/list/{}", entry.id))
        {
            let mut html = String::from("<div class='episodes'><ul>");
            for (number, title) in &entry.episodes {
                html.push_str(&format!("<li><a data-num='{number}' data-ids='{}-{number}'><span class='d-title'>{title}</span></a></li>", entry.id));
            }
            html.push_str("</ul></div>");
            json!({"result": html}).to_string()
        } else {
            return self.inner.get(raw_url, headers);
        };
        self.inner.calls.lock().unwrap().push(raw_url.into());
        Ok(crate::HttpResponse {
            status: 200,
            final_url: raw_url.into(),
            body: body.into_bytes(),
            content_type: None,
        })
    }

    fn storage_get(&self, key: &str) -> Option<String> {
        self.inner.storage_get(key)
    }

    fn storage_set(&self, key: &str, value: &str) -> Result<(), ProviderHostError> {
        self.inner.storage_set(key, value)
    }

    fn log(&self, _message: &str) {}
}

fn mapping_host(entries: Vec<MappingEntry>) -> Arc<MappingHost> {
    Arc::new(MappingHost {
        inner: FixtureHost::new(),
        entries,
        hidden_entry: None,
    })
}

fn mapping_lookup(title: &str, season: u32, episode: u32) -> StreamLookupRequest {
    StreamLookupRequest {
        media_id: "kitsu:original-library-id".into(),
        media_type: "series".into(),
        title: title.into(),
        year: Some("2015".into()),
        season,
        episode,
        absolute_episode: Some(episode + (season - 1) * 24),
        season_episode_count: Some(24),
        series_episode_count: Some(24),
        episode_title: Some(format!("Episode {episode}")),
        released: None,
    }
}

fn mapped_referer(host: Arc<MappingHost>, lookup: &StreamLookupRequest) -> Option<String> {
    let result = provider()
        .protocol_response(
            host,
            request(&stream_lookup_url(ANIKOTO_PROVIDER_URL, lookup).unwrap()),
        )
        .unwrap();
    result["streams"][0]["behaviorHints"]["proxyHeaders"]["request"]["Referer"]
        .as_str()
        .map(String::from)
}

#[test]
fn merged_metadata_season_aligns_two_native_seasons_at_both_boundaries() {
    let host = mapping_host(vec![
        mapping_entry("asterisk", "The Asterisk War", 2015, 12),
        mapping_entry("asterisk-2", "The Asterisk War Season 2", 2016, 12),
    ]);
    for (episode, entry, native_episode) in [
        (1, "asterisk", 1),
        (12, "asterisk", 12),
        (13, "asterisk-2", 1),
        (24, "asterisk-2", 12),
    ] {
        let mut lookup = mapping_lookup("The Asterisk War", 1, episode);
        lookup.released = Some(
            if episode <= 12 {
                "2015-12-01"
            } else {
                "2016-06-01"
            }
            .into(),
        );
        // A complete merged season is sufficient even when later metadata
        // seasons are absent; future native parts still prove its boundary.
        lookup.series_episode_count = None;
        assert_eq!(
            mapped_referer(host.clone(), &lookup),
            Some(format!(
                "https://anikototv.to/watch/{entry}/ep-{native_episode}"
            ))
        );
        assert_eq!(lookup.media_id, "kitsu:original-library-id");
    }
}

#[test]
fn split_metadata_seasons_align_inside_one_native_entry_using_series_total() {
    let host = mapping_host(vec![mapping_entry(
        "asterisk",
        "The Asterisk War",
        2015,
        24,
    )]);
    for (season, episode, absolute) in [(1, 12, 12), (2, 1, 13), (2, 12, 24)] {
        let mut lookup = mapping_lookup("The Asterisk War", season, episode);
        lookup.season_episode_count = Some(12);
        lookup.absolute_episode = Some(absolute);
        assert_eq!(
            mapped_referer(host.clone(), &lookup),
            Some(format!("https://anikototv.to/watch/asterisk/ep-{absolute}"))
        );
    }
    let mut partial = mapping_lookup("The Asterisk War", 2, 1);
    partial.season_episode_count = Some(12);
    partial.series_episode_count = None;
    partial.absolute_episode = Some(13);
    assert_eq!(mapped_referer(host, &partial), None);
}

#[test]
fn unequal_cour_lengths_align_across_multiple_metadata_seasons() {
    let host = mapping_host(vec![
        mapping_entry("part-1", "Split Show Part 1", 2015, 10),
        mapping_entry("part-2", "Split Show 2nd Part", 2015, 14),
        mapping_entry("s2-part-1", "Split Show Season 2 Part 1", 2016, 8),
        mapping_entry("s2-part-2", "Split Show Season 2 Cour 2", 2016, 16),
    ]);
    for (season, episode, entry, native_episode) in [
        (1, 11, "part-2", 1),
        (2, 1, "s2-part-1", 1),
        (2, 9, "s2-part-2", 1),
        (2, 24, "s2-part-2", 16),
    ] {
        let mut lookup = mapping_lookup("Split Show", season, episode);
        lookup.series_episode_count = Some(48);
        assert_eq!(
            mapped_referer(host.clone(), &lookup),
            Some(format!(
                "https://anikototv.to/watch/{entry}/ep-{native_episode}"
            ))
        );
    }
}

#[test]
fn unique_episode_titles_can_override_different_numbers_without_guessing_repeated_titles() {
    let mut entry = mapping_entry("show", "Mapped Show", 2015, 24);
    entry.episodes[1].1 = "The Phoenix Festa".into();
    let mut lookup = mapping_lookup("Mapped Show", 1, 3);
    lookup.episode_title = Some("Episode 3: The Phoenix Festa".into());
    assert_eq!(
        mapped_referer(mapping_host(vec![entry]), &lookup),
        Some("https://anikototv.to/watch/show/ep-2".into())
    );

    let mut entry = mapping_entry("show", "Mapped Show", 2015, 24);
    entry.episodes[1].1 = "The Phoenix Festa".into();
    entry.episodes[5].1 = "The Phoenix Festa".into();
    // No count evidence and a conflicting local title cannot distinguish
    // the two same-named episodes.
    lookup.season_episode_count = None;
    lookup.series_episode_count = None;
    entry.episodes[2].1 = "Another episode".into();
    assert_eq!(mapped_referer(mapping_host(vec![entry]), &lookup), None);
}

#[test]
fn verified_sequence_alignment_tolerates_different_episode_title_wording() {
    let mut first = mapping_entry("first", "The Asterisk War", 2015, 12);
    first.episodes[11].1 = "The Gravi-Sheath".into();
    let host = mapping_host(vec![
        first,
        mapping_entry("second", "The Asterisk War Season 2", 2016, 12),
    ]);
    let mut lookup = mapping_lookup("The Asterisk War", 1, 12);
    lookup.episode_title = Some("The Gravisheath".into());
    assert_eq!(
        mapped_referer(host, &lookup),
        Some("https://anikototv.to/watch/first/ep-12".into())
    );
}

#[test]
fn sequence_mapping_rejects_missing_parts_duplicate_editions_and_recaps() {
    let lookup = mapping_lookup("Mapped Show", 1, 13);
    let cases = vec![
        vec![
            mapping_entry("first", "Mapped Show", 2015, 12),
            mapping_entry("third", "Mapped Show Season 3", 2016, 12),
        ],
        vec![
            mapping_entry("first", "Mapped Show", 2015, 12),
            mapping_entry("copy", "Mapped Show", 2015, 12),
            mapping_entry("second", "Mapped Show Season 2", 2016, 12),
        ],
        vec![
            mapping_entry("first", "Mapped Show", 2015, 13),
            mapping_entry("second", "Mapped Show Season 2", 2016, 12),
        ],
        {
            let mut first = mapping_entry("first", "Mapped Show", 2015, 12);
            first.episodes.remove(4);
            vec![
                first,
                mapping_entry("second", "Mapped Show Season 2", 2016, 12),
            ]
        },
    ];
    for entries in cases {
        let host = mapping_host(entries);
        assert_eq!(mapped_referer(host.clone(), &lookup), None);
        assert!(
            !host
                .inner
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|url| url.contains("/ajax/server"))
        );
    }
}

#[test]
fn declared_counts_align_ongoing_parts_but_missing_episodes_have_no_streams() {
    let mut second = mapping_entry("second", "Mapped Show Season 2", 2016, 12);
    second.episodes.truncate(8);
    let host = mapping_host(vec![
        mapping_entry("first", "Mapped Show", 2015, 12),
        second,
    ]);
    assert_eq!(
        mapped_referer(host.clone(), &mapping_lookup("Mapped Show", 1, 20)),
        Some("https://anikototv.to/watch/second/ep-8".into())
    );
    assert_eq!(
        mapped_referer(host, &mapping_lookup("Mapped Show", 1, 24)),
        None
    );
}

struct CanonicalFixtureHost {
    meta: Value,
}

impl ProviderHost for CanonicalFixtureHost {
    fn get(
        &self,
        url: &str,
        _headers: &BTreeMap<String, String>,
    ) -> Result<crate::HttpResponse, ProviderHostError> {
        let parsed = Url::parse(url).unwrap();
        let body = if parsed.path().starts_with("/catalog/") {
            json!({"metas":[self.meta.clone(), {"id":"tt999", "type":"series", "name":"The Asterisk War Sucks", "releaseInfo":"2015"}]})
        } else {
            json!({"meta": self.meta})
        };
        Ok(crate::HttpResponse {
            status: 200,
            final_url: url.into(),
            body: serde_json::to_vec(&body).unwrap(),
            content_type: None,
        })
    }
    fn storage_get(&self, _key: &str) -> Option<String> {
        None
    }
    fn storage_set(&self, _key: &str, _value: &str) -> Result<(), ProviderHostError> {
        Ok(())
    }
    fn log(&self, _message: &str) {}
}

fn canonical_fixture(seasons: u32, count: u32) -> CanonicalFixtureHost {
    let videos = (1..=seasons).flat_map(|season| (1..=count).map(move |episode| {
            json!({"id":format!("tt5095466:{season}:{episode}"), "name":format!("Episode {episode}"), "season":season, "episode":episode})
        })).collect::<Vec<_>>();
    CanonicalFixtureHost {
        meta: json!({"id":"tt5095466", "type":"series", "name":"The Asterisk War", "releaseInfo":"2015–2016", "videos":videos}),
    }
}

#[test]
fn reverse_mapping_confirms_canonical_episode_ids_across_season_layouts() {
    use base64::Engine;
    let cases = [
        (
            vec![
                mapping_entry("asterisk", "The Asterisk War", 2015, 12),
                mapping_entry("asterisk-2", "The Asterisk War Season 2", 2016, 12),
            ],
            "asterisk-2",
            1,
            24,
            1,
            "tt5095466:1:13",
        ),
        (
            vec![mapping_entry("asterisk", "The Asterisk War", 2015, 24)],
            "asterisk",
            2,
            12,
            13,
            "tt5095466:2:1",
        ),
    ];
    for (entries, source, seasons, count, native_episode, canonical_id) in cases {
        let host = mapping_host(entries);
        let provider = provider();
        let source_id =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!("/watch/{source}"));
        let details = provider
            .call(host.clone(), json!({"op":"details", "sourceId":source_id}))
            .unwrap();
        let media: MediaItem = serde_json::from_value(details["media"].clone()).unwrap();
        let canonical = lookup_canonical_metadata(
            &provider,
            host.clone(),
            &canonical_fixture(seasons, count),
            &media,
            &details,
        )
        .unwrap();
        assert_eq!(
            canonical
                .episodes
                .iter()
                .find(|alias| alias.number == native_episode)
                .unwrap()
                .ids,
            vec![canonical_id]
        );
        assert_eq!(media.provider_id, "anikoto");
        // Mapping only fetches metadata; no expiring playback URLs or
        // server requests are involved in canonical identity resolution.
        assert!(
            !host
                .inner
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|url| url.contains("/ajax/server"))
        );
    }
}

#[test]
fn reverse_mapping_keeps_opened_later_season_amid_side_stories_and_missing_search_rows() {
    use base64::Engine;
    let title = "That Time I Got Reincarnated as a Slime";
    for hidden_entry in [None, Some("slime-s4")] {
        let host = Arc::new(MappingHost {
            inner: FixtureHost::new(),
            hidden_entry,
            entries: vec![
                mapping_entry(
                    "ova",
                    "That Time I Got Reincarnated as a Slime OVA",
                    2021,
                    2,
                ),
                mapping_entry(
                    "oad",
                    "That Time I Got Reincarnated as a Slime OAD",
                    2021,
                    3,
                ),
                mapping_entry(
                    "visions",
                    "That Time I Got Reincarnated as a Slime: Visions of Coleus",
                    2023,
                    3,
                ),
                mapping_entry(
                    "extra",
                    "That Time I Got Reincarnated as a Slime: Side Story",
                    2024,
                    3,
                ),
                mapping_entry("slime", title, 2018, 24),
                mapping_entry(
                    "slime-s2",
                    "That Time I Got Reincarnated as a Slime Season 2",
                    2021,
                    12,
                ),
                mapping_entry(
                    "slime-s2-p2",
                    "That Time I Got Reincarnated as a Slime 2nd Season Part 2",
                    2021,
                    12,
                ),
                mapping_entry(
                    "slime-s3",
                    "That Time I Got Reincarnated as a Slime Season 3",
                    2024,
                    24,
                ),
                mapping_entry(
                    "slime-s4",
                    "That Time I Got Reincarnated as a Slime Season 4",
                    2026,
                    24,
                ),
            ],
        });
        let provider = provider();
        let source_id = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("/watch/slime-s4");
        let details = provider
            .call(host.clone(), json!({"op":"details", "sourceId":source_id}))
            .unwrap();
        let media: MediaItem = serde_json::from_value(details["media"].clone()).unwrap();
        let mut canonical = canonical_fixture(4, 24);
        canonical.meta["name"] = json!(title);
        canonical.meta["id"] = json!("tt9054364");
        canonical.meta["releaseInfo"] = json!("2018–2026");
        for video in canonical.meta["videos"].as_array_mut().unwrap() {
            video["id"] = json!(format!(
                "tt9054364:{}:{}",
                video["season"], video["episode"]
            ));
            video["name"] = json!(format!(
                "Canonical title {} {}",
                video["season"], video["episode"]
            ));
        }
        let resolved =
            lookup_canonical_metadata(&provider, host.clone(), &canonical, &media, &details)
                .unwrap();
        let aliases = resolved.episodes;
        assert_eq!(aliases.len(), 24);
        for alias in &aliases {
            assert_eq!(alias.ids, vec![format!("tt9054364:4:{}", alias.number)]);
        }
        assert_eq!(
            host.inner
                .calls
                .lock()
                .unwrap()
                .iter()
                .filter(|url| url.contains("/watch/slime-s4"))
                .count(),
            1,
            "the known source fetch should be reused"
        );
    }
}

#[test]
fn shortened_series_titles_need_unique_consistent_episode_anchors() {
    use base64::Engine;
    for (native_titles, canonical_titles, mapped_numbers) in [
        (
            vec!["Arrival", "The White Knight", "Battle at Kyushu"],
            vec!["Arrival", "The White Knight", "The Battle for Kyushu"],
            vec![1, 2, 3],
        ),
        (
            vec!["Arrival", "An Unknown Knight", "Battle at Kyushu"],
            vec!["Arrival", "The White Knight", "The Battle for Kyushu"],
            vec![1],
        ),
        (
            vec!["Arrival", "The White Knight", "Battle at Kyushu", "Return"],
            vec!["Arrival", "The White Knight", "Return", "Another Battle"],
            vec![1, 2, 4],
        ),
        (
            vec!["Arrival", "Return", "Return"],
            vec!["Arrival", "Return", "The Battle for Kyushu"],
            vec![1],
        ),
    ] {
        let mut entry = mapping_entry(
            "geass",
            "Code Geass: Lelouch of the Rebellion",
            2006,
            native_titles.len() as u32,
        );
        entry.episodes = native_titles
            .into_iter()
            .enumerate()
            .map(|(index, title)| (index as u32 + 1, title.into()))
            .collect();
        let host = mapping_host(vec![entry]);
        let provider = provider();
        let source_id = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("/watch/geass");
        let details = provider
            .call(host.clone(), json!({"op":"details", "sourceId":source_id}))
            .unwrap();
        let media = serde_json::from_value(details["media"].clone()).unwrap();
        let canonical = CanonicalFixtureHost {
            meta: json!({
                "id":"tt0994314", "type":"series", "name":"Code Geass", "releaseInfo":"2006–2008",
                // This null caused live Code Geass metadata to be rejected.
                "director":null,
                "videos":canonical_titles.into_iter().enumerate().map(|(index,title)| json!({
                    "id":format!("tt0994314:1:{}", index + 1), "season":1, "episode":index + 1, "name":title
                })).collect::<Vec<_>>()
            }),
        };
        let resolved =
            lookup_canonical_metadata(&provider, host, &canonical, &media, &details).unwrap();
        let mut numbers = resolved
            .episodes
            .iter()
            .map(|episode| episode.number)
            .collect::<Vec<_>>();
        numbers.sort_unstable();
        assert_eq!(numbers, mapped_numbers);
    }
}

#[test]
fn mapped_external_metadata_uses_canonical_numbers_but_preserves_native_playback() {
    use base64::Engine;
    let cases = [
        (
            vec![mapping_entry("asterisk", "The Asterisk War", 2015, 24)],
            "asterisk",
            2,
            12,
            13,
            2,
            1,
            "Divine Revelations",
            "2016-04-02T13:30:00Z",
        ),
        (
            vec![
                mapping_entry("asterisk", "The Asterisk War", 2015, 12),
                mapping_entry("asterisk-2", "The Asterisk War Season 2", 2016, 12),
            ],
            "asterisk-2",
            1,
            24,
            1,
            1,
            13,
            "Divine Revelations",
            "2016-04-02T13:30:00Z",
        ),
        (
            vec![
                mapping_entry("slime", "That Time I Got Reincarnated as a Slime", 2018, 24),
                mapping_entry(
                    "slime-2",
                    "That Time I Got Reincarnated as a Slime Season 2",
                    2021,
                    24,
                ),
                mapping_entry(
                    "slime-3",
                    "That Time I Got Reincarnated as a Slime Season 3",
                    2024,
                    24,
                ),
                mapping_entry(
                    "slime-4",
                    "That Time I Got Reincarnated as a Slime Season 4",
                    2026,
                    24,
                ),
            ],
            "slime-4",
            4,
            24,
            1,
            4,
            1,
            "New Days",
            "2026-04-03T14:00:00Z",
        ),
    ];
    for (
        entries,
        source,
        seasons,
        count,
        native_number,
        canonical_season,
        canonical_episode,
        title,
        released,
    ) in cases
    {
        let host = mapping_host(entries);
        let provider = provider();
        let source_id =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!("/watch/{source}"));
        let details = provider
            .call(host.clone(), json!({"op":"details", "sourceId":source_id}))
            .unwrap();
        let media: MediaItem = serde_json::from_value(details["media"].clone()).unwrap();
        let native = details["episodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|episode| episode["number"] == native_number)
            .unwrap();
        let native: Episode = serde_json::from_value(native.clone()).unwrap();
        let mut canonical = canonical_fixture(seasons, count);
        let canonical_id = if source.starts_with("slime") {
            "tt9054364"
        } else {
            "tt5095466"
        };
        if source.starts_with("slime") {
            canonical.meta["id"] = json!(canonical_id);
            canonical.meta["name"] = json!("That Time I Got Reincarnated as a Slime");
            canonical.meta["releaseInfo"] = json!("2018–2026");
        }
        canonical.meta["background"] = json!("https://images.example/canonical-background.jpg");
        canonical.meta["description"] = json!("The matched series description");
        // Real episode descriptions regularly put the cache above the
        // JS's per-entry size cap; this remains bounded host metadata.
        let overview = "An external episode description. ".repeat(90);
        for video in canonical.meta["videos"].as_array_mut().unwrap() {
            video["id"] = json!(format!(
                "{canonical_id}:{}:{}",
                video["season"], video["episode"]
            ));
            video["description"] = json!(overview);
            if video["season"] == canonical_season && video["episode"] == canonical_episode {
                video["name"] = json!(title);
                video["released"] = json!(released);
                video["thumbnail"] = json!("https://images.example/canonical-episode.jpg");
            }
        }
        let resolved =
            lookup_canonical_metadata(&provider, host.clone(), &canonical, &media, &details)
                .unwrap();
        assert_eq!(
            resolved.episodes.len(),
            details["episodes"].as_array().unwrap().len()
        );
        let serialized = serde_json::to_string(&resolved).unwrap();
        assert!(serialized.len() > 16 * 1024);
        assert!(serialized.len() < 256 * 1024);
        let enriched_host = Arc::new(EnrichedFixtureHost {
            inner: host.clone(),
            canonical: resolved.clone(),
        });
        for _ in 0..2 {
            let response = provider
                .protocol_response(
                    enriched_host.clone(),
                    ProtocolRequest::Meta {
                        source_id: source_id.clone(),
                    },
                )
                .unwrap();
            let meta = &response["meta"];
            assert_eq!(meta["id"], media.stable_id());
            assert_eq!(meta["name"], media.title);
            assert_eq!(meta["year"], json!(media.year));
            assert_eq!(meta["imdb_id"], canonical_id);
            assert_eq!(meta["background"], canonical.meta["background"]);
            assert_eq!(meta["description"], canonical.meta["description"]);
            let video = meta["videos"]
                .as_array()
                .unwrap()
                .iter()
                .find(|video| video["id"] == native.stable_id())
                .unwrap();
            assert_eq!(video["name"], title);
            assert_eq!(video["season"], canonical_season);
            assert_eq!(video["episode"], canonical_episode);
            assert_eq!(video["number"], canonical_episode);
            assert_eq!(video["released"], released);
            assert_eq!(
                video["thumbnail"],
                "https://images.example/canonical-episode.jpg"
            );
            assert_eq!(video["overview"], overview);
            assert_eq!(
                video["novaStreamIds"],
                json!([format!(
                    "{canonical_id}:{canonical_season}:{canonical_episode}"
                )])
            );
        }
        let addon = addons::Addon::new(ANIKOTO_PROVIDER_URL).unwrap();
        let streams = provider
            .protocol_response(
                host.clone(),
                request(&addon.stream_url("series", &native.stable_id())),
            )
            .unwrap();
        assert_eq!(
            streams["streams"][0]["behaviorHints"]["proxyHeaders"]["request"]["Referer"],
            format!("https://anikototv.to/watch/{source}/ep-{native_number}")
        );
        assert!(
            !host
                .inner
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|url| url.contains("cinemeta")),
            "cached canonical metadata should not need another external fetch"
        );
    }
}

#[test]
fn canonical_candidate_rejects_remake_ambiguity_and_conflicting_years() {
    let source = MediaItem {
        title: "The Asterisk War Season 2".into(),
        year: Some("2016".into()),
        media_type: "series".into(),
        ..Default::default()
    };
    let candidate = |year: &str| MediaItem {
        title: "The Asterisk War".into(),
        year: Some(year.into()),
        media_type: "series".into(),
        ..Default::default()
    };
    assert_eq!(
        crate::sequence::candidate_rank(&source, &candidate("2020")),
        0
    );
    assert_eq!(
        crate::sequence::candidate_rank(&source, &candidate("2015")),
        2
    );
    assert_eq!(
        crate::sequence::candidate_rank(&source, &candidate("2014")),
        2
    );
}

#[test]
fn confirmed_reverse_aliases_are_attached_without_replacing_source_episode_ids() {
    let host = mapping_host(vec![mapping_entry(
        "asterisk",
        "The Asterisk War",
        2015,
        24,
    )]);
    use base64::Engine;
    let source_id = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("/watch/asterisk");
    let provider = provider();
    let enriched_host = Arc::new(EnrichedFixtureHost {
        inner: host.clone(),
        canonical: CanonicalMetadata {
            media: None,
            episodes: vec![EpisodeStreamIds {
                number: 13,
                ids: vec!["tt5095466:1:13".into()],
                metadata: None,
            }],
        },
    });
    let result = provider
        .protocol_response(enriched_host, ProtocolRequest::Meta { source_id })
        .unwrap();
    assert_eq!(
        result["meta"]["videos"][12]["novaStreamIds"],
        json!(["tt5095466:1:13"])
    );
    assert!(
        result["meta"]["videos"][12]["id"]
            .as_str()
            .unwrap()
            .starts_with("anikoto:ep:")
    );
}

#[test]
fn embedded_servers_resolve_signed_hls_subtitles_and_plain_fallback() {
    let host = FixtureHost::new();
    host.embedded.store(true, Ordering::Relaxed);
    let addon = addons::Addon::new(ANIKOTO_PROVIDER_URL).unwrap();
    let provider = provider();
    let meta = provider
        .protocol_response(
            host.clone(),
            request(&addon.meta_url("series", "anikoto:L3dhdGNoL3Nob3cuYWJj")),
        )
        .unwrap();
    let episode = meta["meta"]["videos"][0]["id"].as_str().unwrap();
    let result = provider
        .protocol_response(host.clone(), request(&addon.stream_url("series", episode)))
        .unwrap();
    let streams = addons::Addon::parse_streams(&serde_json::to_vec(&result).unwrap()).unwrap();
    // One failed embed must not discard other servers; image-wrapped
    // tcdn is omitted before any embed request is made.
    assert_eq!(streams.len(), 4); // three embeds plus the direct mapper
    let encrypted = &streams[0];
    let url = Url::parse(encrypted.url.as_deref().unwrap()).unwrap();
    assert_eq!(
        url.path(),
        format!("/{}/{}/master.m3u8", "a".repeat(32), "b".repeat(32))
    );
    let token = url
        .query_pairs()
        .find(|(name, _)| name == "token")
        .unwrap()
        .1;
    let (payload, signature) = token.split_once('.').unwrap();
    use base64::Engine;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .unwrap();
    let key = aws_lc_rs::hmac::Key::new(
        aws_lc_rs::hmac::HMAC_SHA256,
        b"MpCdnT0k3n!9f2K#xQ7vL5mR8wN1pY4s",
    );
    let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(signature)
        .unwrap();
    aws_lc_rs::hmac::verify(&key, &payload, &signature).unwrap();
    let payload = String::from_utf8(payload).unwrap();
    let (expiry, path) = payload.split_once('|').unwrap();
    let expiry: u64 = expiry.parse().unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!((now + 85..=now + 95).contains(&expiry));
    assert_eq!(path, format!("{}/{}", "a".repeat(32), "b".repeat(32)));
    assert_eq!(encrypted.subtitles.len(), 2);
    assert_eq!(encrypted.subtitles[0].lang.as_deref(), Some("eng"));
    assert_eq!(
        encrypted.extra["behaviorHints"]["proxyHeaders"]["request"]["Origin"],
        "https://megaplay.buzz"
    );
    assert_eq!(
        streams[1].url.as_deref(),
        Some("https://cdn.example/plain/master.m3u8?token=existing")
    );
    assert_eq!(
        streams[2].url.as_deref(),
        Some("https://proxy.example/master.m3u8")
    );
    assert!(
        !host
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|url| url.contains("/stream/image"))
    );
}

#[test]
fn external_matching_is_optional_and_does_not_change_provider_identity() {
    let source = MediaItem {
        provider_id: "anikoto".into(),
        source_id: "opaque".into(),
        title: "Show".into(),
        media_type: "series".into(),
        year: Some("2020".into()),
        ..Default::default()
    };
    let mut canonical = source.clone();
    canonical.provider_id = "other".into();
    canonical.source_id = "tt123".into();
    canonical.external_ids.imdb = Some("tt123".into());
    assert_eq!(
        crate::match_metadata(&source, std::slice::from_ref(&canonical)),
        crate::MetadataMatch::Exact(0)
    );
    let mut enriched = source.clone();
    crate::metadata::merge_media(&mut enriched, &canonical);
    assert_eq!(enriched.stable_id(), source.stable_id());
    assert_eq!(enriched.external_ids.imdb.as_deref(), Some("tt123"));
}

#[test]
fn synthetic_transport_keeps_http_addons_and_rejects_unknown_providers() {
    assert!(fetch_builtin_addon("https://example.com/manifest.json").is_none());
    assert!(
        fetch_builtin_addon("nova-provider://unknown/manifest.json")
            .unwrap()
            .is_err()
    );
    let bytes = fetch_builtin_addon("nova-provider://anikoto/manifest.json")
        .unwrap()
        .unwrap();
    assert_eq!(
        addons::Addon::parse_manifest(&bytes).unwrap().name,
        "AniKoto"
    );
}
