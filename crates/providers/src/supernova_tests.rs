//! Bundled-source fixtures use real QuickJS and parsing capabilities. HTTP and
//! cache persistence are fixture-local so tests cannot reset another test's
//! global preferences while it is running.
use crate::{
    HttpBody, HttpRequest, HttpResponse, PluginManifest, PluginRuntime, ProviderError,
    ProviderHost, ProviderHostError,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex},
};

const KEY: &str = "fixture-private-api-key";
const CACHE_SHIM: &str = r#"
    nova.cache.get = key => nova.storage.get('fixture-cache:' + key);
    nova.cache.set = (key,value,ttl) => nova.storage.set('fixture-cache:' + key, value);
    nova.cache.getMemory = key => nova.storage.get('fixture-memory:' + key);
    nova.cache.setMemory = (key,value,ttl) => nova.storage.set('fixture-memory:' + key, value);
    // Source fixtures capture publications locally; runtime/registry fixtures
    // separately exercise generation and ownership guards against real hosts.
    nova.streams.publish = rows => nova.storage.set('fixture-publication', rows);
"#;

#[derive(Default)]
struct FixtureHost {
    responses: Mutex<HashMap<String, HttpResponse>>,
    requests: Mutex<Vec<HttpRequest>>,
    storage: Mutex<HashMap<String, String>>,
    logs: Mutex<Vec<String>>,
    publications: Mutex<Vec<Value>>,
}
impl FixtureHost {
    fn response(&self, route: &str, status: u16, body: impl Into<Vec<u8>>) {
        self.responses.lock().unwrap().insert(
            route.into(),
            HttpResponse {
                status,
                body: body.into(),
                ..Default::default()
            },
        );
    }
    fn json(&self, route: &str, data: Value) {
        self.response(route, 200, serde_json::to_vec(&data).unwrap());
    }
    fn account(&self, hash: &str, files: Value) {
        self.json(
            "/v1/api/torrents/checkcached",
            json!({"success":true,"data":{hash:{"hash":hash,"files":files}}}),
        );
    }
}
impl ProviderHost for FixtureHost {
    fn get(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
    ) -> Result<HttpResponse, ProviderHostError> {
        self.request(&HttpRequest::get(url, headers))
    }
    fn request(&self, request: &HttpRequest) -> Result<HttpResponse, ProviderHostError> {
        self.requests.lock().unwrap().push(request.clone());
        let url = url::Url::parse(&request.url).unwrap();
        let responses = self.responses.lock().unwrap();
        let mut response = responses
            .get(&request.url)
            .or_else(|| {
                responses.get(&format!(
                    "{}{}",
                    url.host_str().unwrap_or_default(),
                    url.path()
                ))
            })
            .or_else(|| responses.get(url.path()))
            .cloned()
            .ok_or_else(|| ProviderHostError(format!("missing fixture route {}", url.path())))?;
        response.final_url = request.url.clone();
        Ok(response)
    }
    fn storage_get(&self, key: &str) -> Option<String> {
        self.storage.lock().unwrap().get(key).cloned()
    }
    fn storage_set(&self, key: &str, value: &str) -> Result<(), ProviderHostError> {
        if key.ends_with(":fixture-publication") {
            self.publications
                .lock()
                .unwrap()
                .push(serde_json::from_str(value).unwrap());
            return Ok(());
        }
        self.storage
            .lock()
            .unwrap()
            .insert(key.into(), value.into());
        Ok(())
    }
    fn publish_streams(&self, rows: &str) -> Result<(), ProviderHostError> {
        self.publications
            .lock()
            .unwrap()
            .push(serde_json::from_str(rows).unwrap());
        Ok(())
    }
    fn log(&self, message: &str) {
        self.logs.lock().unwrap().push(message.into());
    }
}
fn runtime() -> PluginRuntime {
    let definition = crate::bundled_providers()
        .iter()
        .find(|p| p.url == crate::SUPERNOVA_PROVIDER_URL)
        .unwrap();
    let manifest: PluginManifest = serde_json::from_str(definition.plugin_manifest).unwrap();
    PluginRuntime::new(manifest, format!("{}\n{}", definition.source, CACHE_SHIM))
}
fn settings(source: &str, mode: &str) -> Value {
    json!({format!("sources.{source}"):true,"quality.preferred":"1080p","seeders.minimum":0,"playback.mode":mode,"torbox.enabled":mode!="direct","torbox.apiKey":KEY})
}
fn lookup() -> Value {
    json!({"mediaId":"tt123","mediaType":"series","title":"Example","year":"2020","season":1,"episode":2,"absoluteEpisode":2})
}
fn run_lookup_raw(host: Arc<FixtureHost>, source: &str, mode: &str) -> Value {
    runtime()
        .invoke(
            host,
            &json!({"op":"lookupStreams","lookup":lookup(),"settings":settings(source,mode)}),
        )
        .unwrap()
}
fn run_lookup(host: Arc<FixtureHost>, source: &str, mode: &str) -> Value {
    let result = run_lookup_raw(host, source, mode);
    result.get("streams").cloned().unwrap_or(result)
}
fn bstring(value: &str) -> String {
    format!("{}:{value}", value.len())
}
fn torrent(names: &[&str]) -> (Vec<u8>, String) {
    let info = if names.len() == 1 {
        format!(
            "d6:lengthi4e4:name{}12:piece lengthi4e6:pieces20:01234567890123456789e",
            bstring(names[0])
        )
    } else {
        let files = names
            .iter()
            .map(|name| format!("d6:lengthi4e4:pathl{}ee", bstring(name)))
            .collect::<String>();
        format!(
            "d5:filesl{files}e4:name7:Example12:piece lengthi{}e6:pieces20:01234567890123456789e",
            4 * names.len()
        )
    };
    let bytes = format!("d4:info{info}e").into_bytes();
    let hash = crate::inspect_torrent_metadata(&bytes).unwrap().info_hash;
    (bytes, hash)
}
fn nyaa(title: &str, hash: &str) -> String {
    format!(
        r#"<table class="torrent-list"><tbody><tr><td>Anime</td><td><a class="comments" href="/view/7#comments">3</a><a href="/view/7">{title}</a></td><td><a href="/download/7.torrent">torrent</a><a href="magnet:?xt=urn:btih:{hash}">magnet</a></td><td>1.2 GiB</td><td>2020</td><td>12</td></tr></tbody></table>"#
    )
}
fn nyaa_fixture(names: &[&str], title: &str) -> (Arc<FixtureHost>, String) {
    let host = Arc::new(FixtureHost::default());
    let (bytes, hash) = torrent(names);
    host.response("nyaa.si/", 200, nyaa(title, &hash).into_bytes());
    host.response("/download/7.torrent", 200, bytes);
    (host, hash)
}

#[test]
fn nyaa_descriptions_upgrade_proved_streams_and_survive_result_cache_hits() {
    for mode in ["direct", "cached"] {
        let (host, hash) = nyaa_fixture(&["Example S01E02.mkv"], "Example S01E02 2020");
        host.account(
            &hash,
            json!([{"id":77,"name":"Example S01E02.mkv","size":4}]),
        );
        host.response("/view/7", 200, format!(r#"<html><body>
          <div class="panel-body"><kbd>{hash}</kbd></div>
          <a href="/download/7.torrent">torrent</a><a href="magnet:?xt=urn:btih:{hash}">magnet</a>
          <div id="torrent-description">HEVC 10bit SoftSubbed - 1920 x 1080&#10;**Audio**: Japanese - English&#10;**Subtitles**: French&#10;Plot: An English traveller meets a German friend.</div>
          <div id="comments">Audio: German</div></body></html>"#));
        let first = run_lookup(host.clone(), "nyaa", mode);
        assert_eq!(first.as_array().unwrap().len(), 1, "{mode}: {first}");
        let row = &first[0];
        assert!(row["title"].as_str().unwrap().contains("1080p"), "{row}");
        let description = row["description"].as_str().unwrap();
        assert!(description.contains("🎞️ HEVC"), "{description}");
        assert!(
            description.contains("🌐 Audio: Japanese · English"),
            "{description}"
        );
        assert!(
            description.contains("💬 Subtitles: French"),
            "{description}"
        );
        assert!(!description.contains("German"));
        if mode == "cached" {
            let publications = host.publications.lock().unwrap();
            assert!(
                publications.first().unwrap()[0]["description"]
                    .as_str()
                    .unwrap()
                    .find("🌐 Audio")
                    .is_none()
            );
            assert_eq!(publications.first().unwrap()[0]["id"], row["id"]);
        }
        let calls = host.requests.lock().unwrap().len();
        let second = run_lookup_raw(host.clone(), "nyaa", mode);
        assert_eq!(second["cacheHit"], true);
        assert_eq!(host.requests.lock().unwrap().len(), calls);
        assert!(
            second["streams"][0]["description"]
                .as_str()
                .unwrap()
                .contains("💬 Subtitles: French")
        );
    }
}
fn cached_fixture() -> (Arc<FixtureHost>, String, Value) {
    let (host, hash) = nyaa_fixture(&["Example S01E02.mkv"], "Example S01E02 2020 1080p");
    // Cache IDs intentionally differ from P2P indices and account file IDs.
    let files = json!([{"id":77,"name":"Example S01E02.mkv","size":4}]);
    host.account(&hash, files);
    let rows = run_lookup(host.clone(), "nyaa", "cached");
    assert_eq!(rows.as_array().unwrap().len(), 1);
    let token = rows[0]["resolver"]["token"].clone();
    (host, hash, token)
}
fn resolve(host: Arc<FixtureHost>, token: Value) -> Result<Value, ProviderError> {
    let result = runtime().invoke(
        host,
        &json!({"op":"resolveStream","token":token,"settings":settings("nyaa","cached")}),
    )?;
    if let Some(error) = result.get("error") {
        return Err(ProviderError::Script(
            error["message"].as_str().unwrap_or("Provider error").into(),
        ));
    }
    Ok(result)
}

#[test]
fn source_html_json_and_rss_feed_real_torrent_file_selection() {
    let (host, hash) = nyaa_fixture(&["Example S01E02.mkv"], "[Group] Example S01E02 2020 1080p");
    let rows = run_lookup(host.clone(), "nyaa", "direct");
    assert_eq!(rows[0]["info_hash"], hash);
    assert_eq!(rows[0]["file_idx"], 0);
    assert_eq!(rows[0]["seeders"], 12);
    assert_eq!(rows[0]["size"], 4);
    assert!(
        host.requests
            .lock()
            .unwrap()
            .iter()
            .all(|r| r.method == "GET")
    );
    // The persisted discovery record contains verified index, no credential,
    // account object, debrid URL, or deferred playback token.
    let cached = host
        .storage
        .lock()
        .unwrap()
        .values()
        .cloned()
        .collect::<Vec<_>>()
        .join("");
    assert!(!cached.contains(KEY));
    assert!(!cached.contains("resolver"));
    assert!(!cached.contains("requestdl"));
    let calls = host.requests.lock().unwrap().len();
    let cached_rows = run_lookup(host.clone(), "nyaa", "direct");
    assert_eq!(cached_rows[0]["info_hash"], rows[0]["info_hash"]);
    assert!(
        cached_rows[0]["description"]
            .as_str()
            .unwrap()
            .contains("Cached discovery; peer availability unverified.")
    );
    assert_eq!(host.requests.lock().unwrap().len(), calls);

    for rss in [false, true] {
        let host = Arc::new(FixtureHost::default());
        let (bytes, hash) = torrent(&["Example S01E02.mkv"]);
        let torrent_url = "https://animetosho.org/torrent/7.torrent";
        if rss {
            let body = format!(
                r#"<?xml version="1.0"?><rss><channel><item><title>Example S01E02 2020 1080p</title><description><![CDATA[<a href="magnet:?xt=urn:btih:{hash}">Download</a>]]></description><enclosure url="{torrent_url}"/></item></channel></rss>"#
            );
            host.response("/feed/json", 200, body.into_bytes());
        } else {
            host.json("/feed/json",json!([{"title":"Example S01E02 2020 1080p","info_hash":hash,"total_size":4,"seeders":10,"torrent_url":torrent_url}]));
        }
        host.response("/torrent/7.torrent", 200, bytes);
        let rows = run_lookup(host, "animetosho", "direct");
        assert_eq!(rows[0]["info_hash"], hash, "RSS={rss}");
        assert_eq!(rows[0]["file_idx"], 0, "RSS={rss}");
    }
    let host = Arc::new(FixtureHost::default());
    let (bytes, hash) = torrent(&["Example S01E02.mkv"]);
    host.json("/q.php",json!([{"name":"Example S01E02 2020 1080p","info_hash":hash,"size":"4","seeders":"10","imdb":"tt123"}]));
    host.response(
        &format!("/torrent/{}.torrent", hash.to_uppercase()),
        200,
        bytes,
    );
    let rows = run_lookup(host, "apibay", "direct");
    assert_eq!(rows[0]["info_hash"], hash);
    assert_eq!(rows[0]["file_idx"], 0);
}

#[test]
fn wrong_episode_missing_hash_and_ambiguous_files_are_not_playable() {
    for title in ["Example S01E03 2020 1080p"] {
        let (host, _) = nyaa_fixture(&["Example S01E02.mkv"], title);
        assert_eq!(run_lookup(host, "nyaa", "direct"), json!([]), "{title}");
    }
    let (host, _) = nyaa_fixture(&["Example S01E02.mkv"], "Example S01E02 2020");
    host.response(
        "nyaa.si/",
        200,
        nyaa("Example S01E02 2020", "not-a-hash").into_bytes(),
    );
    assert_eq!(run_lookup(host, "nyaa", "direct"), json!([]));
    let (host, _) = nyaa_fixture(
        &["Example S01E02.mkv", "Example S01E02.mp4"],
        "Example S01E02 2020",
    );
    assert_eq!(run_lookup(host, "nyaa", "direct"), json!([]));
    let (host, hash) = nyaa_fixture(
        &["Example S01E01.mkv", "Example S01E02.mkv", "sample.mp4"],
        "Example S01E02 2020",
    );
    let rows = run_lookup(host, "nyaa", "direct");
    assert_eq!(rows[0]["info_hash"], hash);
    assert_eq!(rows[0]["file_idx"], 1);
    let (host, _) = nyaa_fixture(&["Example S01E02.mkv"], "Example S01E02 2020");
    let (other, _) = torrent(&["Unrelated S01E02.mkv"]);
    host.response("/download/7.torrent", 200, other);
    assert_eq!(run_lookup(host, "nyaa", "direct"), json!([]));
}

#[test]
fn cached_torbox_rows_are_owned_key_free_and_search_never_creates() {
    let (host, _, token) = cached_fixture();
    assert!(!token.as_str().unwrap().contains(KEY));
    let calls = host.requests.lock().unwrap();
    assert!(calls.iter().all(|r| r.method == "GET"));
    assert!(
        !calls
            .iter()
            .any(|r| r.url.contains("createtorrent") || r.url.contains("requestdl"))
    );
    assert!(calls.iter().any(|r| r.url.contains("checkcached")
        && r.headers.get("Authorization") == Some(&format!("Bearer {KEY}"))));
}

#[test]
fn cached_torbox_playback_reselects_account_ids_instead_of_cache_or_p2p_ids() {
    let (host, hash, token) = cached_fixture();
    host.json("/v1/api/torrents/mylist",json!({"success":true,"data":[{"id":99,"hash":hash,"download_finished":true,"download_present":true,"files":[{"id":8,"name":"Example S01E01.mkv"},{"id":42,"name":"Example S01E02.mkv"}]}]}));
    host.json(
        "/v1/api/torrents/requestdl",
        json!({"success":true,"data":"https://cdn.example/play.mp4"}),
    );
    let stream = resolve(host.clone(), token).unwrap();
    assert_eq!(stream["url"], "https://cdn.example/play.mp4");
    let calls = host.requests.lock().unwrap();
    let request = calls.iter().find(|r| r.url.contains("requestdl")).unwrap();
    let url = url::Url::parse(&request.url).unwrap();
    let query = url.query_pairs().collect::<HashMap<_, _>>();
    assert_eq!(query["torrent_id"], "99");
    assert_eq!(query["file_id"], "42");
    assert_eq!(query["token"], KEY);
    assert!(calls.iter().all(|r| r.method == "GET"));
    assert!(!stream.to_string().contains(KEY));
    assert!(!host.logs.lock().unwrap().join("").contains(KEY));
}

#[test]
fn torbox_errors_uncached_state_and_ambiguous_ids_do_not_create_during_search() {
    for (status, body) in [
        (200, json!({"success":false,"error":"AUTH_ERROR"})),
        (401, json!({})),
        (429, json!({})),
        (200, json!({"success":true,"data":{}})),
    ] {
        let (host, _) = nyaa_fixture(&["Example S01E02.mkv"], "Example S01E02 2020");
        host.response(
            "/v1/api/torrents/checkcached",
            status,
            serde_json::to_vec(&body).unwrap(),
        );
        assert_eq!(run_lookup(host.clone(), "nyaa", "cached"), json!([]));
        assert!(
            host.requests
                .lock()
                .unwrap()
                .iter()
                .all(|r| r.method == "GET")
        );
        assert!(!host.logs.lock().unwrap().join("").contains(KEY));
    }
    let (host, hash) = nyaa_fixture(&["Example S01E02.mkv"], "Example S01E02 2020");
    host.account(
        &hash,
        json!([{"id":1,"name":"Example S01E02.mkv"},{"id":2,"name":"Example S01E02.mp4"}]),
    );
    assert_eq!(run_lookup(host, "nyaa", "cached"), json!([]));
    let (host, _, token) = cached_fixture();
    host.json(
        "/v1/api/torrents/checkcached",
        json!({"success":true,"data":{}}),
    );
    assert!(
        resolve(host.clone(), token)
            .unwrap_err()
            .to_string()
            .contains("no longer cached")
    );
    assert!(
        !host
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.method == "POST")
    );
}

#[test]
fn movie_lookup_rejects_other_remake_and_conflicting_imdb() {
    let host = Arc::new(FixtureHost::default());
    let (bytes, hash) = torrent(&["Example 2020.mkv"]);
    host.json(
        "/q.php",
        json!([
            {"name":"Example 2010 1080p","info_hash":hash,"size":"4","seeders":"10"},
            {"name":"Example 2020 1080p","info_hash":hash,"size":"4","seeders":"10","imdb":"tt999"},
            {"name":"Example 2020 1080p","info_hash":hash,"size":"4","seeders":"10","imdb":"tt123"}
        ]),
    );
    host.response(
        &format!("/torrent/{}.torrent", hash.to_uppercase()),
        200,
        bytes,
    );
    let movie = json!({"mediaId":"tt123","mediaType":"movie","title":"Example","year":"2020"});
    let rows = runtime()
        .invoke(
            host,
            &json!({"op":"lookupStreams","lookup":movie,"settings":settings("apibay","direct")}),
        )
        .unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["info_hash"], hash);
}

#[test]
fn confirmed_cached_selection_may_create_only_with_cached_policy_and_verified_ids() {
    let (host, hash, token) = cached_fixture();
    host.json("/v1/api/torrents/mylist", json!({"success":true,"data":[]}));
    host.json(
        "/v1/api/torrents/createtorrent",
        json!({"success":true,"data":{"hash":hash,"torrent_id":123}}),
    );
    host.json("https://api.torbox.app/v1/api/torrents/mylist?id=123&bypass_cache=true",json!({"success":true,"data":{"id":123,"hash":hash,"download_finished":true,"download_present":true,"files":[{"id":91,"name":"Example S01E02.mkv"}]}}));
    host.json(
        "/v1/api/torrents/requestdl",
        json!({"success":true,"data":"https://cdn.example/video.mp4"}),
    );
    assert_eq!(
        resolve(host.clone(), token).unwrap()["url"],
        "https://cdn.example/video.mp4"
    );
    let calls = host.requests.lock().unwrap();
    let created = calls
        .iter()
        .filter(|r| r.method == "POST")
        .collect::<Vec<_>>();
    assert_eq!(created.len(), 1);
    assert!(created[0].url.ends_with("/createtorrent"));
    let Some(HttpBody::Multipart(fields)) = &created[0].body else {
        panic!("create must use documented multipart form")
    };
    assert_eq!(fields["add_only_if_cached"], "true");
    assert_eq!(fields["allow_zip"], "false");
    assert_eq!(fields["magnet"], format!("magnet:?xt=urn:btih:{hash}"));
    assert!(!fields.values().any(|value| value.contains(KEY)));
    let request = calls.iter().find(|r| r.url.contains("requestdl")).unwrap();
    let url = url::Url::parse(&request.url).unwrap();
    let query = url.query_pairs().collect::<HashMap<_, _>>();
    assert_eq!(query["file_id"], "91");
    assert_eq!(query["torrent_id"], "123");
}

#[test]
fn missing_invalid_key_and_selection_rate_limit_return_safe_errors() {
    let (host, _, token) = cached_fixture();
    for key in ["", "invalid key"] {
        let mut preferences = settings("nyaa", "cached");
        preferences["torbox.apiKey"] = json!(key);
        let calls = host.requests.lock().unwrap().len();
        let error = runtime()
            .invoke(
                host.clone(),
                &json!({"op":"resolveStream","token":token,"settings":preferences}),
            )
            .unwrap();
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap()
                .contains("API key")
        );
        assert_eq!(host.requests.lock().unwrap().len(), calls);
    }
    host.response("/v1/api/torrents/checkcached", 429, b"{}".to_vec());
    let error = resolve(host.clone(), token).unwrap_err();
    assert!(error.to_string().contains("rate limited"));
    assert!(!error.to_string().contains(KEY));
    assert!(
        !host
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.method == "POST")
    );
}

#[test]
fn series_remake_year_and_wrong_show_file_are_not_playable() {
    let (host, _) = nyaa_fixture(&["Example S01E02.mkv"], "Example S01E02 2010 1080p");
    assert_eq!(run_lookup(host, "nyaa", "direct"), json!([]));
    let (host, _) = nyaa_fixture(&["Wrong Show S01E02.mkv"], "Example S01E02 2020 1080p");
    assert_eq!(run_lookup(host, "nyaa", "direct"), json!([]));
}

#[test]
fn source_failure_warnings_preserve_other_source_results() {
    let host = Arc::new(FixtureHost::default());
    let (bytes, hash) = torrent(&["Example S01E02.mkv"]);
    host.response("nyaa.si/", 503, b"<h1>Unavailable</h1>".to_vec());
    host.json(
        "/q.php",
        json!([{"name":"Example S01E02 2020 1080p","info_hash":hash,"size":"4","seeders":"10"}]),
    );
    host.response(
        &format!("/torrent/{}.torrent", hash.to_uppercase()),
        200,
        bytes,
    );
    let mut preferences = settings("apibay", "direct");
    preferences["sources.nyaa"] = json!(true);
    let result = runtime()
        .invoke(
            host,
            &json!({"op":"lookupStreams","lookup":lookup(),"settings":preferences}),
        )
        .unwrap();
    assert_eq!(result["streams"].as_array().unwrap().len(), 1);
    assert_eq!(result["streams"][0]["info_hash"], hash);
    let warnings = result["warnings"].as_array().unwrap();
    assert!(!warnings.is_empty());
    assert!(
        warnings
            .iter()
            .all(|warning| warning["source"].as_str() == Some("Nyaa"))
    );
    assert!(!result.to_string().contains(KEY));
}

#[test]
fn already_existing_torbox_torrent_uses_one_fresh_hash_checked_account_list() {
    let (host, hash, token) = cached_fixture();
    host.json("/v1/api/torrents/mylist", json!({"success":true,"data":[]}));
    host.json(
        "/v1/api/torrents/createtorrent",
        json!({"success":false,"error":"TORRENT_ALREADY_EXISTS","detail":"private upstream text"}),
    );
    host.json("https://api.torbox.app/v1/api/torrents/mylist?limit=1000&bypass_cache=true",json!({"success":true,"data":[
        {"id":1,"hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","download_finished":true,"files":[{"id":7,"name":"Example S01E02.mkv"}]},
        {"id":88,"hash":hash,"download_finished":true,"files":[{"id":91,"name":"Example S01E02.mkv"}]}
    ]}));
    host.json(
        "/v1/api/torrents/requestdl",
        json!({"success":true,"data":"https://cdn.example/video.mp4"}),
    );
    assert_eq!(
        resolve(host.clone(), token).unwrap()["url"],
        "https://cdn.example/video.mp4"
    );
    let calls = host.requests.lock().unwrap();
    assert_eq!(
        calls
            .iter()
            .filter(|request| request.method == "POST")
            .count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|request| request.url.contains("limit=1000"))
            .count(),
        1
    );
    let request = calls
        .iter()
        .find(|request| request.url.contains("requestdl"))
        .unwrap();
    let url = url::Url::parse(&request.url).unwrap();
    let query = url.query_pairs().collect::<HashMap<_, _>>();
    assert_eq!(query["torrent_id"], "88");
    assert_eq!(query["file_id"], "91");
}

#[test]
fn movie_file_uploader_prefix_requires_matching_release_group_and_year() {
    for (file, title, expected) in [
        (
            "GROUP-Sintel.2010.1080p.mp4",
            "Sintel.2010.1080p-GROUP",
            true,
        ),
        (
            "GROUP-Sintel.2010.1080p.mp4",
            "Sintel.2010.1080p-OTHER",
            false,
        ),
        ("GROUP-Sintel.2010.1080p.mp4", "Sintel.2010.1080p", false),
        (
            "BehindTheScenes-Sintel.2010.1080p.mp4",
            "Sintel.2010.1080p-GROUP",
            false,
        ),
        (
            "Wrong-Sintel.2010.1080p.mp4",
            "Sintel.2010.1080p-GROUP",
            false,
        ),
        (
            "GROUP-Sintel.2011.1080p.mp4",
            "Sintel.2010.1080p-GROUP",
            false,
        ),
    ] {
        let host = Arc::new(FixtureHost::default());
        let (bytes, hash) = torrent(&[file]);
        host.json(
            "/q.php",
            json!([{"name":title,"info_hash":hash,"size":"4","seeders":"10","imdb":"tt1727587"}]),
        );
        host.response(
            &format!("/torrent/{}.torrent", hash.to_uppercase()),
            200,
            bytes,
        );
        let lookup =
            json!({"mediaId":"tt1727587","mediaType":"movie","title":"Sintel","year":"2010"});
        let result=runtime().invoke(host,&json!({"op":"lookupStreams","lookup":lookup,"settings":settings("apibay","direct")})).unwrap();
        let rows = result.get("streams").unwrap_or(&result).as_array().unwrap();
        assert_eq!(rows.len(), usize::from(expected), "{file} in {title}");
        if expected {
            assert_eq!(rows[0]["file_idx"], 0);
            assert_eq!(rows[0]["info_hash"], hash);
        }
    }
}

#[test]
fn malformed_json_and_xml_feeds_warn_but_valid_empty_feeds_do_not() {
    for (source, route, body) in [
        ("apibay", "/q.php", "{\"error\":\"upstream failure\"}"),
        ("apibay", "/q.php", "not JSON"),
        (
            "animetosho",
            "/feed/json",
            "{\"error\":\"upstream failure\"}",
        ),
        ("animetosho", "/feed/json", "<rss><channel>"),
        (
            "animetosho",
            "/feed/json",
            "<html><body>Unavailable</body></html>",
        ),
    ] {
        let host = Arc::new(FixtureHost::default());
        host.response(route, 200, body.as_bytes().to_vec());
        let result = run_lookup_raw(host, source, "direct");
        assert_eq!(result["streams"], json!([]), "{source}: {body}");
        assert!(
            !result["warnings"].as_array().unwrap().is_empty(),
            "{source}: {body}"
        );
        assert!(!result.to_string().contains("upstream failure"));
    }
    for (source, route, body) in [
        ("apibay", "/q.php", "[]"),
        ("animetosho", "/feed/json", "[]"),
        (
            "animetosho",
            "/feed/json",
            "<?xml version=\"1.0\"?><rss><channel/></rss>",
        ),
        (
            "nyaa",
            "nyaa.si/",
            "<table class=\"torrent-list\"><tbody></tbody></table>",
        ),
    ] {
        let host = Arc::new(FixtureHost::default());
        host.response(route, 200, body.as_bytes().to_vec());
        assert_eq!(
            run_lookup_raw(host, source, "direct"),
            json!([]),
            "{source}: {body}"
        );
    }
}

#[test]
fn combined_episode_markers_cannot_select_only_the_first_episode() {
    for marker in [
        "1x02x03",
        "1x02+03",
        "S01E02+03",
        "S01E02E03",
        "S01E02 S01E03",
        "S01E02 and 03",
    ] {
        let title = format!("Example {marker} 2020 1080p");
        let combined = format!("Example {marker}.mkv");
        let (host, _) = nyaa_fixture(&[&combined], &title);
        assert_eq!(
            run_lookup(host, "nyaa", "direct"),
            json!([]),
            "release {marker}"
        );
        let file = format!("Example {marker}.mkv");
        let (host, _) = nyaa_fixture(&[&file], "Example S01E02 2020 1080p");
        assert_eq!(
            run_lookup(host, "nyaa", "direct"),
            json!([]),
            "file {marker}"
        );
    }
    // Codec and resolution numbers remain independent of episode identity.
    let (host, _) = nyaa_fixture(
        &["Example 1x02 1080p x265.mkv"],
        "Example 1x02 2020 1080p x265",
    );
    assert_eq!(
        run_lookup(host, "nyaa", "direct").as_array().unwrap().len(),
        1
    );
}

#[test]
fn supplementary_movie_and_episode_videos_are_not_the_main_feature() {
    for extra in [
        "BehindTheScenes",
        "Behind.The.Scenes",
        "Deleted.Scenes",
        "Making.Of",
        "Bonus",
        "BTS",
    ] {
        let host = Arc::new(FixtureHost::default());
        let file = format!("Sintel.2010.{extra}.mp4");
        let (bytes, hash) = torrent(&[&file]);
        host.json("/q.php",json!([{"name":"Sintel.2010.1080p","info_hash":hash,"size":"4","seeders":"10","imdb":"tt1727587"}]));
        host.response(
            &format!("/torrent/{}.torrent", hash.to_uppercase()),
            200,
            bytes,
        );
        let lookup =
            json!({"mediaId":"tt1727587","mediaType":"movie","title":"Sintel","year":"2010"});
        let result=runtime().invoke(host,&json!({"op":"lookupStreams","lookup":lookup,"settings":settings("apibay","direct")})).unwrap();
        assert_eq!(
            result.get("streams").unwrap_or(&result),
            &json!([]),
            "movie extra {extra}"
        );
        let file = format!("Example S01E02 {extra}.mkv");
        let (host, _) = nyaa_fixture(&[&file], "Example S01E02 2020 1080p");
        assert_eq!(
            run_lookup(host, "nyaa", "direct"),
            json!([]),
            "episode extra {extra}"
        );
        let title = format!("Example S01E02 2020 {extra}");
        let (host, _) = nyaa_fixture(&["Example S01E02.mkv"], &title);
        assert_eq!(
            run_lookup(host, "nyaa", "direct"),
            json!([]),
            "release extra {extra}"
        );
    }
}

#[test]
fn seeded_discovery_cache_marks_reported_peers_without_source_requests() {
    let definition = crate::bundled_providers()
        .iter()
        .find(|p| p.url == crate::SUPERNOVA_PROVIDER_URL)
        .unwrap();
    let manifest: PluginManifest = serde_json::from_str(definition.plugin_manifest).unwrap();
    let script = format!(
        "{}\n{}",
        definition.source,
        r#"
        Supernova.cacheGet = () => ({records:[{
            title:'Example S01E02 2020 1080p',
            infoHash:'abcdef0123456789abcdef0123456789abcdef01',
            fileIdx:0,seeders:23,sizeBytes:4,sources:['Fixture'],torrentUrl:null
        }],warnings:[]});
    "#
    );
    let host = Arc::new(FixtureHost::default());
    let rows = PluginRuntime::new(manifest, script)
        .invoke(
            host.clone(),
            &json!({
                "op":"lookupStreams","lookup":lookup(),"settings":settings("nyaa","direct")
            }),
        )
        .unwrap();
    let description = rows[0]["description"].as_str().unwrap();
    assert!(description.contains("23 reported seeders"));
    assert!(description.contains("Cached discovery; peer availability unverified."));
    assert!(!description.contains("23 seeders"));
    assert!(host.requests.lock().unwrap().is_empty());
}

#[test]
fn integer_fields_reject_coercion_and_invalid_account_file_ids() {
    let definition = crate::bundled_providers()
        .iter()
        .find(|p| p.url == crate::SUPERNOVA_PROVIDER_URL)
        .unwrap();
    let manifest: PluginManifest = serde_json::from_str(definition.plugin_manifest).unwrap();
    let script = format!(
        "{}\n{}",
        definition.source,
        r#"
        novaProvider.handle = () => ({
            invalid:[false,true,[],[1],{},null,undefined,'','  ','0x10','1e3','1.5','-1',-1,NaN,Infinity,9007199254740992].map(Supernova.number),
            valid:[0,1,1e3,'0','001',' 42 '].map(Supernova.number)
        });
    "#
    );
    let result = PluginRuntime::new(manifest, script)
        .invoke(Arc::new(FixtureHost::default()), &json!({"op":"fixture"}))
        .unwrap();
    assert!(
        result["invalid"]
            .as_array()
            .unwrap()
            .iter()
            .all(Value::is_null)
    );
    assert_eq!(result["valid"], json!([0, 1, 1000, 0, 1, 42]));

    for id in [
        json!(false),
        json!(true),
        json!([1]),
        json!({"id":1}),
        json!("0x2a"),
        json!("4.2e1"),
    ] {
        let (host, hash, token) = cached_fixture();
        host.json(
            "/v1/api/torrents/mylist",
            json!({"success":true,"data":[{
                "id":99,"hash":hash,"download_finished":true,"download_present":true,
                "files":[{"id":id,"name":"Example S01E02.mkv"}]
            }]}),
        );
        assert!(
            resolve(host.clone(), token)
                .unwrap_err()
                .to_string()
                .contains("correct video file")
        );
        let calls = host.requests.lock().unwrap();
        assert!(calls.iter().all(|request| request.method == "GET"));
        assert!(
            !calls
                .iter()
                .any(|request| request.url.contains("requestdl"))
        );
    }
    for id in [json!(42), json!("42"), json!(" 42 ")] {
        let (host, hash, token) = cached_fixture();
        host.json(
            "/v1/api/torrents/mylist",
            json!({"success":true,"data":[{
                "id":99,"hash":hash,"download_finished":true,"download_present":true,
                "files":[{"id":id,"name":"Example S01E02.mkv"}]
            }]}),
        );
        host.json(
            "/v1/api/torrents/requestdl",
            json!({"success":true,"data":"https://cdn.example/video.mp4"}),
        );
        assert_eq!(
            resolve(host.clone(), token).unwrap()["url"],
            "https://cdn.example/video.mp4"
        );
        let calls = host.requests.lock().unwrap();
        let request = calls
            .iter()
            .find(|request| request.url.contains("requestdl"))
            .unwrap();
        assert!(request.url.contains("file_id=42"));
        assert!(calls.iter().all(|request| request.method == "GET"));
    }
}

#[test]
fn captured_steins_gate_release_metadata_keeps_original_and_zero_separate() {
    let definition = crate::bundled_providers()
        .iter()
        .find(|p| p.url == crate::SUPERNOVA_PROVIDER_URL)
        .unwrap();
    let manifest: PluginManifest = serde_json::from_str(definition.plugin_manifest).unwrap();
    // Captured public AnimeTosho metadata from canonical Steins;Gate 01 search.
    // Hashes and labels are source evidence; no torrent payload is bundled.
    let records = json!([
        {"title":"[C204] STEINS;GATE - 01 [BD 720p yuv444p10le flac] [DB7A4A80].mkv","infoHash":"2131a12f646c66b7ded94e66aae12d564afd071e"},
        {"title":"[WhyNotFix] Steins;Gate - 01.mkv","infoHash":"fbfc71fb803011bc082dca4cd9e8a821c6d77cbe"},
        {"title":"[Sour] Steins;Gate 0 Episode 1 Missing Link of the Annihilator -Absolute Zero- English Dub 720p","infoHash":"c9b6224f38f1d57f0b41ed890ed8fb61c435f0b3"},
        {"title":"[Sour] Steins;Gate 0 Episode 1 Missing Link of the Annihilator -Absolute Zero- English Dub 1080p","infoHash":"6434dc2e63a54ab51be24281c0caa21748023adc"}
    ]);
    let script = format!(
        "{}\n{}",
        definition.source,
        r#"
        novaProvider.handle = request => {
            const original = {mediaId:'tt1910272',mediaType:'series',title:'Steins;Gate',year:'2011',season:1,episode:1,absoluteEpisode:1};
            const zero = {...original,mediaId:'tt5514358',title:'Steins;Gate 0',year:'2018'};
            const hash = 'a'.repeat(40);
            const match = (title,lookup) => Supernova.matches({title,infoHash:hash},lookup);
            return {
                original:request.records.filter(row=>Supernova.matches(row,original)).map(row=>row.infoHash),
                zero:request.records.filter(row=>Supernova.matches(row,zero)).map(row=>row.infoHash),
                zeroSingles:['Steins;Gate 0 - 01 [1080p]','SteinsGate0.S01E01.1080p','Steins Gate 0 01 [1080p]'].map(title=>match(title,zero)),
                wrongOriginal:['Steins;Gate 0 - 01 [1080p]','Steins.Gate.0.S01E01','SteinsGate-0-S01E01','Steins;Gate 0 Episode 1 [1080p]'].map(title=>match(title,original)),
                packs:['Steins;Gate 0 - 01 ~ 23','Steins;Gate 0 (01-23) Batch','Steins;Gate 0 S01E01+02'].map(title=>match(title,zero)),
                originalQueries:Supernova.queries(original,true),zeroQueries:Supernova.queries(zero,true),
                yearNotTitle:match('Steins;Gate (2011) Episode 1 [1080p]',original),
                wrongEpisode:match('Steins;Gate 0 - 02 [1080p]',zero),
                uniqueZero:Supernova.chooseFile([{index:0,path:'Steins;Gate 0 - 02.mkv'},{index:1,path:'Steins;Gate 0 - 01.mkv'}],{title:'Steins;Gate 0 - 01 [1080p]',infoHash:hash},zero,false),
                foreignFile:Supernova.chooseFile([{index:0,path:'Steins;Gate 0 - 01.mkv'}],{title:'Steins;Gate - 01 [720p]',infoHash:hash},original,false)
            };
        };
    "#
    );
    let result = PluginRuntime::new(manifest, script)
        .invoke(
            Arc::new(FixtureHost::default()),
            &json!({"op":"fixture","records":records}),
        )
        .unwrap();
    assert_eq!(
        result["original"],
        json!([
            "2131a12f646c66b7ded94e66aae12d564afd071e",
            "fbfc71fb803011bc082dca4cd9e8a821c6d77cbe"
        ])
    );
    assert_eq!(
        result["zero"],
        json!([
            "c9b6224f38f1d57f0b41ed890ed8fb61c435f0b3",
            "6434dc2e63a54ab51be24281c0caa21748023adc"
        ])
    );
    assert_eq!(result["zeroSingles"], json!([true, true, true]));
    assert!(
        result["wrongOriginal"]
            .as_array()
            .unwrap()
            .iter()
            .all(|value| value == false)
    );
    assert!(
        result["packs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|value| value == false)
    );
    assert_eq!(
        result["originalQueries"],
        json!(["Steins;Gate", "Steins;Gate 01"])
    );
    assert_eq!(
        result["zeroQueries"],
        json!(["Steins;Gate 0", "Steins;Gate 0 01"])
    );
    assert_eq!(result["yearNotTitle"], true);
    assert_eq!(result["wrongEpisode"], false);
    assert_eq!(result["uniqueZero"]["selectedId"], 1);
    assert!(result["foreignFile"].is_null());
}

#[test]
fn season_inventory_candidates_require_unique_exact_episode_and_owned_container_proof() {
    let definition = crate::bundled_providers()
        .iter()
        .find(|p| p.url == crate::SUPERNOVA_PROVIDER_URL)
        .unwrap();
    let manifest: PluginManifest = serde_json::from_str(definition.plugin_manifest).unwrap();
    let script = format!(
        "{}\n{}",
        definition.source,
        r#"
        novaProvider.handle = () => {
            const original = {mediaId:'tt1910272',mediaType:'series',title:'Steins;Gate',year:'2011',season:1,episode:2,absoluteEpisode:2};
            const zero = {...original,mediaId:'tt5514358',title:'Steins;Gate 0',year:'2018'};
            const hash = 'a'.repeat(40);
            const record = {title:'Steins;Gate (2011) S01 Complete',infoHash:hash};
            const titleOnly = {...record,title:'Steins;Gate (2011) BluRay 1080p'};
            const choose = (files,entry=record,lookup=original,container=null) => Supernova.chooseFile(files,entry,lookup,false,container);
            return {
                admitted:['Steins;Gate (2011) S01 Complete','Steins;Gate (2011) BluRay 1080p','Steins;Gate - 01 ~ 24 [1080p]','Steins;Gate 1-24 [1080p]'].map(title=>Supernova.candidateMatches({...record,title},original)),
                rejected:['Steins;Gate (2010) Complete','Steins;Gate S02 Complete','Steins;Gate - 03 ~ 24','Steins;Gate 0 (2018) Complete'].map(title=>Supernova.candidateMatches({...record,title},original)),
                strictPack:Supernova.matches(record,original),
                unique:choose([{index:0,path:'Steins;Gate (2011) S01/01.mkv'},{index:1,path:'Steins;Gate (2011) S01/02.mkv'},{index:2,path:'Steins;Gate (2011) S01/03.mkv'}]),
                titleOnly:choose([{index:3,path:'Steins;Gate - 02.mkv'}],titleOnly),
                wrongEpisode:choose([{index:0,path:'Steins;Gate (2011) S01/03.mkv'}]),
                wrongSeason:choose([{index:0,path:'Steins;Gate (2011) S02/02.mkv'}]),
                wrongYear:choose([{index:0,path:'Steins;Gate (2010) S01/02.mkv'}]),
                namedWrongYear:choose([{index:0,path:'Steins;Gate (2010) S01E02.mkv'}]),
                ambiguous:choose([{index:0,path:'Steins;Gate (2011) S01/02.mkv'},{index:1,path:'Steins;Gate (2011) S01/02.mp4'}]),
                noRoot:choose([{index:0,path:'02.mkv'}]),
                explicitForeign:choose([{index:0,path:'Wrong Show S01E02.mkv'}]),
                sequelFile:choose([{index:0,path:'Steins;Gate 0 (2018) S01/02.mkv'}]),
                zero:choose([{index:4,path:'Steins;Gate 0 (2018) S01/02.mkv'}],{...record,title:'Steins;Gate 0 (2018) S01 Complete'},zero),
                verifiedContainer:choose([{index:5,path:'02.mkv'}],record,original,'Steins;Gate (2011) S01 Complete'),
                foreignContainer:choose([{index:5,path:'02.mkv'}],record,original,'Steins;Gate 0 (2018) S01 Complete'),
                seasonTwo:choose([{index:6,path:'Example Season 2/02.mkv'}],{title:'Example Season 2 Complete',infoHash:hash},{title:'Example',mediaType:'series',year:null,season:2,episode:2,absoluteEpisode:14})
            };
        };
    "#
    );
    let result = PluginRuntime::new(manifest, script)
        .invoke(Arc::new(FixtureHost::default()), &json!({"op":"fixture"}))
        .unwrap();
    assert_eq!(result["admitted"], json!([true, true, true, true]));
    assert_eq!(result["rejected"], json!([false, false, false, false]));
    assert_eq!(result["strictPack"], false);
    assert_eq!(result["unique"]["selectedId"], 1);
    assert_eq!(result["titleOnly"]["selectedId"], 3);
    assert_eq!(result["zero"]["selectedId"], 4);
    assert_eq!(result["verifiedContainer"]["selectedId"], 5);
    assert_eq!(result["seasonTwo"]["selectedId"], 6);
    for field in [
        "wrongEpisode",
        "wrongSeason",
        "wrongYear",
        "namedWrongYear",
        "ambiguous",
        "noRoot",
        "explicitForeign",
        "sequelFile",
        "foreignContainer",
    ] {
        assert!(result[field].is_null(), "{field}: {}", result[field]);
    }
}

#[test]
fn season_pack_discovery_emits_only_metadata_verified_episode_index() {
    let (host, hash) = nyaa_fixture(
        &[
            "Example S01E01.mkv",
            "Example S01E02.mkv",
            "Example S01E03.mkv",
        ],
        "Example (2020) S01 Complete 1080p",
    );
    let rows = run_lookup(host, "nyaa", "direct");
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["info_hash"], hash);
    assert_eq!(rows[0]["file_idx"], 1);
    let (host, _) = nyaa_fixture(&["Example S02E02.mkv"], "Example (2020) S01 Complete 1080p");
    assert_eq!(run_lookup(host, "nyaa", "direct"), json!([]));
    let (host, _) = nyaa_fixture(
        &["Example S01E02.mkv", "Example S01E02.mp4"],
        "Example (2020) S01 Complete 1080p",
    );
    assert_eq!(run_lookup(host, "nyaa", "direct"), json!([]));
}

#[test]
fn torbox_checks_bounded_discovery_pool_and_preserves_successful_batches() {
    let definition = crate::bundled_providers()
        .iter()
        .find(|provider| provider.url == crate::SUPERNOVA_PROVIDER_URL)
        .unwrap();
    let manifest: PluginManifest = serde_json::from_str(definition.plugin_manifest).unwrap();
    let script = format!(
        "{}\n{}",
        definition.source,
        r#"
        novaProvider.handle = request => {
            const warnings = [];
            const streams = Supernova.torboxOptions(request.records,request.lookup,request.settings,warnings,3000);
            return {streams,warnings};
        };
        "#
    );
    let records = (1..=151)
        .map(|index| {
            json!({
                "infoHash":format!("{index:040x}"),"title":"Example (2020) S01 Complete 1080p",
                "sources":["Fixture"],"seeders":null,"fileIdx":null,"sizeBytes":4
            })
        })
        .collect::<Vec<_>>();
    let host = Arc::new(FixtureHost::default());
    let selected_hash = records[88]["infoHash"].as_str().unwrap();
    let cached_entry = json!({"hash":selected_hash,"name":"Example (2020) S01 Complete",
        "files":[{"id":77,"name":"01.mkv"},{"id":78,"name":"02.mkv"}]});
    for (index, group) in records[..150].chunks(50).enumerate() {
        let hashes = group
            .iter()
            .map(|record| record["infoHash"].as_str().unwrap())
            .collect::<Vec<_>>()
            .join(",");
        let route = format!(
            "https://api.torbox.app/v1/api/torrents/checkcached?hash={hashes}&format=object&list_files=true"
        );
        match index {
            0 => host.response(&route, 500, b"{}".to_vec()),
            1 => host.json(
                &route,
                json!({"success":true,"data":{selected_hash:cached_entry}}),
            ),
            _ => host.response(&route, 429, b"{}".to_vec()),
        }
    }
    let result = PluginRuntime::new(manifest,script).invoke(host.clone(),&json!({
        "op":"fixture","records":records,"lookup":lookup(),"settings":settings("nyaa","cached")
    })).unwrap();
    assert_eq!(result["streams"].as_array().unwrap().len(), 1);
    assert_eq!(result["warnings"].as_array().unwrap().len(), 2);
    assert!(!result.to_string().contains(KEY));
    let calls = host.requests.lock().unwrap();
    assert_eq!(calls.len(), 3);
    assert!(
        calls
            .iter()
            .all(|request| request.method == "GET" && request.timeout_ms == Some(3000))
    );
    assert!(calls.iter().all(|request| !request.url.contains(KEY)));
    assert!(
        calls
            .iter()
            .all(|request| !request.url.contains(&format!("{:040x}", 151)))
    );
    drop(calls);

    // Generic episode names need a hash-verified torrent container in both
    // availability and account results; cache IDs never become account IDs.
    host.json(
        "/v1/api/torrents/checkcached",
        json!({"success":true,"data":{selected_hash:cached_entry}}),
    );
    host.json(
        "/v1/api/torrents/mylist",
        json!({"success":true,"data":[{
            "id":99,"hash":selected_hash,"name":"Example (2020) S01 Complete",
            "download_finished":true,"download_present":true,
            "files":[{"id":42,"name":"02.mkv"}]
        }]}),
    );
    host.json(
        "/v1/api/torrents/requestdl",
        json!({"success":true,"data":"https://cdn.example/video.mp4"}),
    );
    let token = result["streams"][0]["resolver"]["token"].clone();
    assert_eq!(
        resolve(host.clone(), token).unwrap()["url"],
        "https://cdn.example/video.mp4"
    );
    let calls = host.requests.lock().unwrap();
    assert!(calls.iter().all(|request| request.method == "GET"));
    assert!(
        calls
            .iter()
            .any(|request| request.url.contains("file_id=42"))
    );
}

fn runtime_with_suffix(suffix: &str) -> PluginRuntime {
    let definition = crate::bundled_providers()
        .iter()
        .find(|p| p.url == crate::SUPERNOVA_PROVIDER_URL)
        .unwrap();
    PluginRuntime::new(
        serde_json::from_str::<PluginManifest>(definition.plugin_manifest).unwrap(),
        format!("{}\n{}\n{}", definition.source, CACHE_SHIM, suffix),
    )
}

#[test]
fn torznab_pages_preserve_item_attributes_and_public_metainfo_identity() {
    let host = Arc::new(FixtureHost::default());
    let (bytes, hash) = torrent(&["Example S01E02.mkv"]);
    let metadata = "https://independent.example/source.torrent";
    let xml = format!(
        r#"<rss xmlns:tz="http://torznab.com/schemas/2015/feed" xmlns:nz="http://www.newznab.com/DTD/2010/feeds/attributes/"><channel><nz:response offset="0" total="301"/><item><title>Example S01E02 2020 1080p</title><tz:attr name="infohash" value="{hash}"/><tz:attr name="size" value="4"/><tz:attr name="seeders" value="7"/><enclosure url="{metadata}"/></item></channel></rss>"#
    );
    host.response("feed.animetosho.org/api", 200, xml.into_bytes());
    host.response(metadata, 200, bytes);
    let rows = run_lookup(host.clone(), "animetosho", "direct");
    assert_eq!(rows[0]["info_hash"], hash);
    assert_eq!(rows[0]["file_idx"], 0);
    assert_eq!(rows[0]["seeders"], 7);
    let calls = host.requests.lock().unwrap();
    let pages = calls
        .iter()
        .filter(|request| request.url.contains("feed.animetosho.org/api"))
        .collect::<Vec<_>>();
    assert!(pages.len() >= 3 && pages.len() <= 5);
    assert!(
        pages
            .iter()
            .any(|request| request.url.contains("offset=150"))
    );
    assert!(
        pages
            .iter()
            .any(|request| request.url.contains("offset=300"))
    );
    assert!(
        !pages
            .iter()
            .any(|request| request.url.contains("offset=450"))
    );
    assert!(
        !calls
            .iter()
            .any(|request| request.url.contains("/feed/json"))
    );
    assert!(calls.iter().all(|request| request.method == "GET"));
}

#[test]
fn main_json_fallback_uses_hash_mirror_when_metainfo_url_is_missing() {
    let host = Arc::new(FixtureHost::default());
    let (bytes, hash) = torrent(&["Example S01E02.mkv"]);
    host.response("feed.animetosho.org/api", 503, b"Unavailable".to_vec());
    host.json(
        "/feed/json",
        json!([{"title":"Example S01E02 2020 1080p","info_hash":hash,"total_size":4,"seeders":5}]),
    );
    let mirror = format!(
        "https://itorrents.net/torrent/{}.torrent",
        hash.to_uppercase()
    );
    host.response(&mirror, 200, bytes);
    let result = run_lookup_raw(host.clone(), "animetosho", "direct");
    let rows = result.get("streams").unwrap_or(&result).as_array().unwrap();
    assert_eq!(rows[0]["info_hash"], hash);
    assert_eq!(rows[0]["file_idx"], 0);
    assert!(result.get("warnings").is_none());
    assert!(
        host.requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.url == mirror)
    );
}

#[test]
fn nyaa_follows_advertised_primary_pages_and_caps_episode_variant() {
    let (host, hash) = nyaa_fixture(&["Example S01E02.mkv"], "Example S01E02 2020 1080p");
    host.response("nyaa.si/",200,format!("{}<ul class='pagination'><li><a href='?q=Example&amp;p=2'>2</a></li><li><a href='?q=Example&amp;p=3'>3</a></li></ul>",nyaa("Example S01E02 2020 1080p",&hash)).into_bytes());
    assert_eq!(
        run_lookup(host.clone(), "nyaa", "direct")
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let calls = host.requests.lock().unwrap();
    let pages = calls
        .iter()
        .filter(|request| request.url.starts_with("https://nyaa.si/?"))
        .collect::<Vec<_>>();
    assert!(pages.len() <= 5);
    assert!(pages.iter().any(|request| request.url.contains("p=2")));
    assert!(pages.iter().any(|request| request.url.contains("p=3")));
    assert!(!pages.iter().any(|request| request.url.contains("p=4")
        || request.url.contains("c=1_2")
        || request.url.contains("s=seeders")));
}

#[test]
fn null_file_proof_is_retried_without_renewing_discovery_expiry() {
    let (host, hash) = nyaa_fixture(&["Example S01E02.mkv"], "Example S01E02 2020 1080p");
    host.response("/download/7.torrent", 503, b"Unavailable".to_vec());
    let script = "const native=novaProvider.handle; novaProvider.handle=request=>{Date.now=()=>request.fakeNow;return native(request);};";
    let runtime = runtime_with_suffix(script);
    let result=runtime.invoke(host.clone(),&json!({"op":"lookupStreams","lookup":lookup(),"settings":settings("nyaa","direct"),"fakeNow":100000})).unwrap();
    assert_eq!(result, json!([]));
    let cache_entry = || {
        host.storage
            .lock()
            .unwrap()
            .values()
            .find_map(|raw| {
                serde_json::from_str::<Value>(raw)
                    .ok()
                    .filter(|value| value.get("records").is_some())
            })
            .unwrap()
    };
    let expiry = cache_entry()["discoveryExpiresAt"].as_u64().unwrap();
    let source_calls = host
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request.url.starts_with("https://nyaa.si/?"))
        .count();
    let (bytes, _) = torrent(&["Example S01E02.mkv"]);
    host.response("/download/7.torrent", 200, bytes);
    let result=runtime.invoke(host.clone(),&json!({"op":"lookupStreams","lookup":lookup(),"settings":settings("nyaa","direct"),"fakeNow":110000})).unwrap();
    let rows = result.get("streams").unwrap_or(&result).as_array().unwrap();
    assert_eq!(rows[0]["info_hash"], hash);
    assert_eq!(rows[0]["file_idx"], 0);
    assert_eq!(cache_entry()["discoveryExpiresAt"].as_u64(), Some(expiry));
    assert_eq!(
        host.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.url.starts_with("https://nyaa.si/?"))
            .count(),
        source_calls
    );
}

#[test]
fn empty_failed_discovery_is_not_persisted_and_bad_cache_forces_refresh() {
    let host = Arc::new(FixtureHost::default());
    host.response("nyaa.si/", 503, b"Unavailable".to_vec());
    assert_eq!(
        run_lookup_raw(host.clone(), "nyaa", "direct")["streams"],
        json!([])
    );
    assert!(host.storage.lock().unwrap().is_empty());
    for cache in [
        json!({"records":[null]}),
        json!({"records":[{"infoHash":"0123456789012345678901234567890123456789","title":"Example S01E02 2020","sources":"Nyaa","fileIdx":0,"sizeBytes":4,"seeders":1,"torrentUrl":null}]}),
    ] {
        let (host, hash) = nyaa_fixture(&["Example S01E02.mkv"], "Example S01E02 2020 1080p");
        let suffix = format!("Supernova.cacheGet=()=>({cache});");
        let result=runtime_with_suffix(&suffix).invoke(host.clone(),&json!({"op":"lookupStreams","lookup":lookup(),"settings":settings("nyaa","direct")})).unwrap();
        let rows = result.get("streams").unwrap_or(&result).as_array().unwrap();
        assert_eq!(rows[0]["info_hash"], hash);
        assert!(!host.requests.lock().unwrap().is_empty());
    }
}

#[test]
fn optional_batch_fake_clock_stops_before_reserved_availability_budget() {
    let suffix = r#"let time=0,calls=[]; Date.now=()=>time;
      nova.http.batch=raw=>{const requests=JSON.parse(raw);calls.push(requests.map(r=>r.timeoutMs));time+=Math.max(...requests.map(r=>r.timeoutMs));return JSON.stringify(requests.map(()=>({status:200,body:'[]'})));};
      novaProvider.handle=()=>{const rows=Supernova.batch(Array.from({length:9},()=>({url:'https://example.com/'})),3500);return {time,calls,rows:rows.length,skipped:rows.filter(row=>row.error).length};};"#;
    let result = runtime_with_suffix(suffix)
        .invoke(crate::MemoryProviderHost::new(), &json!({}))
        .unwrap();
    assert_eq!(result["rows"], 9);
    assert_eq!(result["skipped"], 3);
    assert_eq!(result["calls"].as_array().unwrap().len(), 2);
    assert_eq!(result["time"], 3350);
    assert!(
        result["calls"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|wave| wave.as_array().unwrap())
            .all(|value| value
                .as_u64()
                .is_some_and(|timeout| (200..=3000).contains(&timeout)))
    );
}

fn assert_same_cached_targets(first: &Value, repeated: &Value) {
    if repeated.is_object() {
        assert_eq!(repeated["cacheHit"], true);
    }
    let first = first.as_array().unwrap();
    let repeated = repeated
        .get("streams")
        .unwrap_or(repeated)
        .as_array()
        .unwrap();
    assert_eq!(first.len(), repeated.len());
    for (initial, cached) in first.iter().zip(repeated) {
        for field in ["id", "info_hash", "file_idx", "resolver"] {
            assert_eq!(initial.get(field), cached.get(field));
        }
        let annotation_count = cached["description"]
            .as_str()
            .unwrap()
            .matches("Cached discovery; peer availability unverified.")
            .count();
        assert_eq!(
            annotation_count,
            usize::from(cached.get("resolver").is_none())
        );
    }
}

const RESULT_CACHE_CLOCK: &str = r#"
  const handleResultCache=novaProvider.handle;
  const rankResultCache=Supernova.rank;
  let resultCacheMiss=false;
  Supernova.rank=(...args)=>{if(!resultCacheMiss){nova.log('result-cache-miss');resultCacheMiss=true;}return rankResultCache(...args);};
  novaProvider.handle=request=>{resultCacheMiss=false;Date.now=()=>request.fakeNow;return handleResultCache(request);};
"#;

#[test]
fn verified_result_cache_reuses_exact_episode_across_runtime_reload_without_http() {
    for mode in ["direct", "preferred", "cached"] {
        let (host, hash) = nyaa_fixture(&["Example S01E02.mkv"], "Example S01E02 2020 1080p");
        host.account(
            &hash,
            json!([{"id":77,"name":"Example S01E02.mkv","size":4}]),
        );
        let request = json!({"op":"lookupStreams","lookup":lookup(),"settings":settings("nyaa",mode),"fakeNow":100000});
        let first = runtime_with_suffix(RESULT_CACHE_CLOCK)
            .invoke(host.clone(), &request)
            .unwrap();
        assert!(!first.as_array().unwrap().is_empty());
        let calls = host.requests.lock().unwrap().len();
        let repeated = runtime_with_suffix(RESULT_CACHE_CLOCK)
            .invoke(host.clone(), &request)
            .unwrap();
        assert_same_cached_targets(&first, &repeated);
        assert_eq!(host.requests.lock().unwrap().len(), calls, "{mode}");
        assert_eq!(
            host.logs
                .lock()
                .unwrap()
                .iter()
                .filter(|line| line.ends_with("result-cache-miss"))
                .count(),
            1
        );
        let persisted = host.storage.lock().unwrap();
        let result_entries = persisted
            .iter()
            .filter(|(key, _)| key.contains("results:v2:"))
            .collect::<Vec<_>>();
        assert_eq!(result_entries.len(), 1);
        assert!(result_entries[0].0.contains("fixture-cache:"));
        assert!(!result_entries[0].0.contains("fixture-memory:"));
        let payload: Value = serde_json::from_str(result_entries[0].1).unwrap();
        assert_eq!(payload["expiresAt"], 1000);
        assert!(!result_entries[0].1.contains(KEY));
        assert!(!result_entries[0].1.contains("https://"));
        assert!(!result_entries[0].1.contains("Authorization"));
    }
}

#[test]
fn verified_result_cache_identity_expiry_and_clear_require_fresh_operation() {
    let (host, hash) = nyaa_fixture(&["Example S01E02.mkv"], "Example S01E02 2020 1080p");
    host.account(
        &hash,
        json!([{"id":77,"name":"Example S01E02.mkv","size":4}]),
    );
    let base = json!({"op":"lookupStreams","lookup":lookup(),"settings":settings("nyaa","preferred"),"fakeNow":100000});
    runtime_with_suffix(RESULT_CACHE_CLOCK)
        .invoke(host.clone(), &base)
        .unwrap();
    let mut variants = vec![];
    let mut episode = base.clone();
    episode["lookup"]["episode"] = json!(3);
    variants.push(episode);
    let mut mode = base.clone();
    mode["settings"]["playback.mode"] = json!("cached");
    variants.push(mode);
    let mut source = base.clone();
    source["settings"]["sources.animetosho"] = json!(true);
    variants.push(source);
    let mut expiry = base.clone();
    expiry["fakeNow"] = json!(1001000);
    variants.push(expiry);
    for (index, request) in variants.iter().enumerate() {
        runtime_with_suffix(RESULT_CACHE_CLOCK)
            .invoke(host.clone(), request)
            .unwrap();
        assert_eq!(
            host.logs
                .lock()
                .unwrap()
                .iter()
                .filter(|line| line.ends_with("result-cache-miss"))
                .count(),
            index + 2
        );
    }
    host.storage.lock().unwrap().clear();
    runtime_with_suffix(RESULT_CACHE_CLOCK)
        .invoke(host.clone(), &base)
        .unwrap();
    assert_eq!(
        host.logs
            .lock()
            .unwrap()
            .iter()
            .filter(|line| line.ends_with("result-cache-miss"))
            .count(),
        6
    );
}

#[test]
fn verified_result_cache_rejects_malformed_or_sensitive_rows_and_partial_errors() {
    for corruption in ["null", "url", "secret"] {
        let (host, _) = nyaa_fixture(&["Example S01E02.mkv"], "Example S01E02 2020 1080p");
        let request = json!({"op":"lookupStreams","lookup":lookup(),"settings":settings("nyaa","direct"),"fakeNow":100000});
        runtime_with_suffix(RESULT_CACHE_CLOCK)
            .invoke(host.clone(), &request)
            .unwrap();
        for (key, raw) in host.storage.lock().unwrap().iter_mut() {
            if key.contains("results:v2:") {
                let mut payload: Value = serde_json::from_str(raw).unwrap();
                match corruption {
                    "null" => payload["streams"] = json!([null]),
                    "url" => payload["streams"][0]["url"] = json!("https://example.com/secret"),
                    _ => {
                        payload["streams"][0]["description"] =
                            json!("fixture%2Dprivate%2Dapi%2Dkey")
                    }
                }
                *raw = payload.to_string();
            }
        }
        runtime_with_suffix(RESULT_CACHE_CLOCK)
            .invoke(host.clone(), &request)
            .unwrap();
        assert_eq!(
            host.logs
                .lock()
                .unwrap()
                .iter()
                .filter(|line| line.ends_with("result-cache-miss"))
                .count(),
            2
        );
    }
    let (host, _) = nyaa_fixture(&["Example S01E02.mkv"], "Example S01E02 2020 1080p");
    host.response("/v1/api/torrents/checkcached", 503, b"{}".to_vec());
    let request = json!({"op":"lookupStreams","lookup":lookup(),"settings":settings("nyaa","preferred"),"fakeNow":100000});
    for _ in 0..2 {
        let output = runtime_with_suffix(RESULT_CACHE_CLOCK)
            .invoke(host.clone(), &request)
            .unwrap();
        assert!(!output["streams"].as_array().unwrap().is_empty());
        assert!(!output["warnings"].as_array().unwrap().is_empty());
    }
    assert!(
        !host
            .storage
            .lock()
            .unwrap()
            .keys()
            .any(|key| key.contains("results:v2:"))
    );
    assert_eq!(
        host.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.url.contains("checkcached"))
            .count(),
        2
    );
}

#[test]
fn lookup_publishes_verified_whole_lists_and_cached_results_immediately() {
    let (host, hash) = nyaa_fixture(&["Example S01E02.mkv"], "Example S01E02 2020 1080p");
    host.account(
        &hash,
        json!([{"id":77,"name":"Example S01E02.mkv","size":4}]),
    );
    let first = run_lookup(host.clone(), "nyaa", "preferred");
    let snapshots = host.publications.lock().unwrap().clone();
    assert_eq!(snapshots.len(), 2);
    assert_eq!(snapshots[0].as_array().unwrap().len(), 1);
    assert!(snapshots[0][0].get("resolver").is_some());
    assert_eq!(snapshots[1], first);
    assert_eq!(snapshots[1][0]["id"], snapshots[0][0]["id"]);
    let calls = host.requests.lock().unwrap().len();
    let cached = run_lookup(host.clone(), "nyaa", "preferred");
    assert_same_cached_targets(&first, &cached);
    assert_eq!(host.requests.lock().unwrap().len(), calls);
    let repeated = host.publications.lock().unwrap();
    assert_eq!(repeated.len(), 3);
    assert_eq!(repeated[2], cached);
}

#[test]
fn deeper_advertised_pack_is_checked_once_after_first_page_progress() {
    let host = Arc::new(FixtureHost::default());
    let hashes = (1..=6)
        .map(|index| format!("{index:040x}"))
        .collect::<Vec<_>>();
    for (page, hash) in hashes.iter().enumerate() {
        host.response(&format!("/page/{page}"),200,format!("{}<ul class='pagination'><li><a href='?p=6'>6</a></li><li><a href='?p=99'>99</a></li></ul>",nyaa("Example (2020) S01 Complete 1080p",hash)).into_bytes());
    }
    host.account(
        &hashes[5],
        json!([{"id":77,"name":"Example S01E02.mkv","size":4}]),
    );
    let suffix = "Supernova.sources.nyaa.url=(query,lookup,page=0)=>'https://nyaa.si/page/'+page;";
    let output = runtime_with_suffix(suffix)
        .invoke(
            host.clone(),
            &json!({"op":"lookupStreams","lookup":lookup(),"settings":settings("nyaa","cached")}),
        )
        .unwrap();
    let rows = output.get("streams").unwrap_or(&output).as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0]["id"].as_str().unwrap().starts_with(&hashes[5]));
    let calls = host.requests.lock().unwrap();
    let first_check = calls
        .iter()
        .position(|request| request.url.contains("checkcached"))
        .unwrap();
    let deep_page = calls
        .iter()
        .position(|request| request.url.ends_with("/page/5"))
        .unwrap();
    assert!(first_check < deep_page);
    assert!(!calls.iter().any(|request| request.url.ends_with("/page/6")));
    let mut checked = HashMap::<String, usize>::new();
    for request in calls
        .iter()
        .filter(|request| request.url.contains("checkcached"))
    {
        let url = url::Url::parse(&request.url).unwrap();
        for hash in url
            .query_pairs()
            .find(|(key, _)| key == "hash")
            .unwrap()
            .1
            .split(',')
        {
            *checked.entry(hash.into()).or_default() += 1;
        }
    }
    assert_eq!(checked.len(), 6);
    assert!(checked.values().all(|count| *count == 1));
    assert!(calls.len() <= 48);
    assert_eq!(
        host.publications.lock().unwrap().last().unwrap(),
        &json!(rows)
    );
}

#[test]
fn filled_discovery_pool_keeps_crawling_and_admits_better_ranked_later_pack() {
    let host = Arc::new(FixtureHost::default());
    let row = |index: usize, seeders: u64| json!({"infoHash":format!("{index:040x}"),"title":"Example (2020) S01 Complete 1080p","sizeBytes":4,"seeders":seeders,"torrentUrl":null,"sources":["Nyaa"]});
    host.json(
        "/page/0",
        json!((1..=150).map(|index| row(index, 1)).collect::<Vec<_>>()),
    );
    host.json(
        "/page/1",
        json!((151..=300).map(|index| row(index, 1)).collect::<Vec<_>>()),
    );
    host.json("/page/2", json!([row(301, 1000)]));
    host.json("/page/3", json!([row(302, 2000)]));
    let selected = format!("{:040x}", 302);
    host.account(
        &selected,
        json!([{"id":77,"name":"Example S01E02.mkv","size":4}]),
    );
    let suffix = r#"Supernova.sources.nyaa.url=(query,lookup,page=0)=>'https://nyaa.si/page/'+page;Supernova.sources.nyaa.parse=JSON.parse;Supernova.sources.nyaa.pages=()=>4;
      let retained=[];const cacheSet=Supernova.cacheSet;
      Supernova.cacheSet=(key,payload,ttl)=>{if(payload.records)retained=payload.records;cacheSet(key,payload,ttl);};
      const handle=novaProvider.handle;novaProvider.handle=request=>{const result=handle(request);return {result,retainedCount:retained.length,firstHash:retained[0] && retained[0].infoHash};};"#;
    let output = runtime_with_suffix(suffix)
        .invoke(
            host.clone(),
            &json!({"op":"lookupStreams","lookup":lookup(),"settings":settings("nyaa","cached")}),
        )
        .unwrap();
    let result = &output["result"];
    let rows = result.get("streams").unwrap_or(result).as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0]["id"].as_str().unwrap().starts_with(&selected));
    assert!(
        host.requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.url.ends_with("/page/3"))
    );
    // The real cache permits 1 MiB; the fixture's legacy storage shim permits
    // only 16 KiB. Inspect the intended bounded cache payload directly here.
    assert_eq!(output["retainedCount"], 300);
    assert_eq!(output["firstHash"], selected);
}

#[test]
fn knaben_v2_get_pages_use_verified_hash_and_exact_metainfo_file() {
    let host = Arc::new(FixtureHost::default());
    let (bytes, hash) = torrent(&["Example S01E02.mkv"]);
    host.json("api.knaben.org/v2/search",json!({"total":{"value":151,"relation":"eq"},"hits":[
        {"title":"Example S01E02 2020 1080p","hash":hash.to_uppercase(),"magnetUrl":format!("magnet:?xt=urn:btih:{hash}"),"bytes":4,"seeders":3,"liveSeeders":7,
         "link":"https://untrusted.example/download-proxy","cachedOrigin":"https://private-value.invalid/"}
    ]}));
    let mirror = format!(
        "https://itorrents.net/torrent/{}.torrent",
        hash.to_uppercase()
    );
    host.response(&mirror, 200, bytes);
    let rows = run_lookup(host.clone(), "knaben", "direct");
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["info_hash"], hash);
    assert_eq!(rows[0]["file_idx"], 0);
    assert_eq!(rows[0]["seeders"], 7);
    assert_eq!(rows[0]["source"], "Knaben");
    let calls = host.requests.lock().unwrap();
    assert!(calls.iter().all(|request| request.method == "GET"));
    assert!(calls.iter().any(|request| request.url.contains("f=150")));
    assert!(!calls.iter().any(|request| request.url.contains("f=300")
        || request.url.contains("untrusted.example")
        || request.url.contains("xxx")
        || request.url.contains("unsafe")));
    assert!(calls.iter().any(|request| request.url == mirror));
    assert!(
        !host
            .storage
            .lock()
            .unwrap()
            .values()
            .any(|raw| raw.contains("private-value") || raw.contains("download-proxy"))
    );
}

#[test]
fn knaben_valid_empty_and_malformed_feeds_fail_source_locally() {
    for (body, warning) in [
        ("{\"hits\":[],\"total\":{\"value\":0}}", false),
        ("[]", true),
        ("{\"hits\":null}", true),
        ("not JSON", true),
    ] {
        let host = Arc::new(FixtureHost::default());
        host.response("api.knaben.org/v2/search", 200, body.as_bytes().to_vec());
        let output = run_lookup_raw(host.clone(), "knaben", "direct");
        if warning {
            assert_eq!(output["streams"], json!([]));
            assert_eq!(output["warnings"][0]["source"], "Knaben");
        } else {
            assert_eq!(output, json!([]));
        }
        assert!(
            !host
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|request| request.url.contains("itorrents"))
        );
    }
}

#[test]
fn knaben_parser_rejects_hash_conflicts_and_configuration_preserves_pirate_bay_key() {
    let valid = "0123456789012345678901234567890123456789";
    let other = "1123456789012345678901234567890123456789";
    let body=json!({"hits":[null,{"title":"Conflict","hash":valid,"magnetUrl":format!("magnet:?xt=urn:btih:{other}")},
        {"title":"Unsupported","hash":"abc"},{"title":"No hash"},
        {"title":"Valid","hash":valid,"bytes":"12","seeders":"2"},
        {"title":"Magnet only","magnetUrl":format!("magnet:?xt=urn:btih:{other}")}],"total":{"value":999999999}}).to_string();
    let suffix = "novaProvider.handle=request=>({records:Supernova.sources.knaben.parse(request.body),pages:Supernova.sources.knaben.pages(request.body)});";
    let output = runtime_with_suffix(suffix)
        .invoke(crate::MemoryProviderHost::new(), &json!({"body":body}))
        .unwrap();
    assert_eq!(output["records"].as_array().unwrap().len(), 2);
    assert_eq!(output["records"][0]["infoHash"], valid);
    assert_eq!(output["records"][1]["infoHash"], other);
    assert_eq!(output["pages"], 6);
    let provider = crate::bundled_providers()
        .iter()
        .find(|provider| provider.url == crate::SUPERNOVA_PROVIDER_URL)
        .unwrap();
    let manifest: Value = serde_json::from_str(provider.plugin_manifest).unwrap();
    let schema = manifest["configuration"].as_array().unwrap();
    assert!(
        schema
            .iter()
            .any(|field| field["key"] == "sources.knaben" && field["default"] == false)
    );
    assert!(schema.iter().any(|field| field["key"] == "sources.apibay"
        && field["label"].as_str().unwrap().contains("The Pirate Bay")));
}

#[test]
fn knaben_zero_seed_inventory_reaches_verified_torbox_availability() {
    let host = Arc::new(FixtureHost::default());
    let hash = "0123456789012345678901234567890123456789";
    host.json("api.knaben.org/v2/search",json!({"hits":[{"title":"Example S01E02 2020 1080p","hash":hash,"bytes":4,"seeders":0,"liveSeeders":0}],"total":{"value":1}}));
    host.account(
        hash,
        json!([{"id":77,"name":"Example S01E02.mkv","size":4}]),
    );
    let rows = run_lookup(host.clone(), "knaben", "cached");
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert!(rows[0].get("resolver").is_some());
    assert_eq!(rows[0]["seeders"], Value::Null);
    let requests = host.requests.lock().unwrap();
    assert!(
        requests
            .iter()
            .filter(|request| request.url.starts_with("https://api.knaben.org/"))
            .all(|request| url::Url::parse(&request.url)
                .unwrap()
                .query_pairs()
                .any(|(key, value)| key == "dead" && value.is_empty()))
    );
    assert!(
        requests
            .iter()
            .any(|request| request.url.contains("checkcached"))
    );
    assert!(requests.iter().all(|request| request.method == "GET"));
}

fn stremthru_feed(hash: &str, label: &str) -> String {
    format!(
        r#"<rss xmlns:t="http://torznab.com/schemas/2015/feed"><channel><item><title>Example S01E02 2020 1080p</title><t:attr name="infohash" value="{hash}"/><t:attr name="size" value="4"/><t:attr name="indexername" value="{label}"/><t:attr name="seeders" value="99"/></item></channel></rss>"#
    )
}

#[test]
fn stremthru_imdb_inventory_keeps_provenance_and_requires_exact_file_proof() {
    let host = Arc::new(FixtureHost::default());
    let (bytes, hash) = torrent(&["Example S01E02.mkv"]);
    host.response(
        "/v0/torznab/api",
        200,
        stremthru_feed(&hash, "Knaben").into_bytes(),
    );
    host.response(
        &format!(
            "https://itorrents.net/torrent/{}.torrent",
            hash.to_uppercase()
        ),
        200,
        bytes,
    );
    let rows = run_lookup(host.clone(), "stremthru", "direct");
    assert_eq!(rows[0]["info_hash"], hash);
    assert_eq!(rows[0]["file_idx"], 0);
    assert_eq!(rows[0]["source"], "StremThru|Knaben");
    assert_eq!(rows[0]["seeders"], Value::Null);
    let requests = host.requests.lock().unwrap();
    let discovery = requests
        .iter()
        .filter(|request| request.url.contains("/v0/torznab/api"))
        .collect::<Vec<_>>();
    assert_eq!(discovery.len(), 1);
    assert!(discovery[0].url.contains("imdbid=tt123"));
    assert!(requests.iter().all(|request| request.method == "GET"));
}

#[test]
fn stremthru_movie_inventory_keeps_cached_web_release_without_franchise_mismatches() {
    // Public inventory labels and the account's nested filename reproduce the
    // reported missing movie. Remakes remain excluded even when also cached.
    let host = Arc::new(FixtureHost::default());
    let hash = "b65c9fe80d9c428d49d3a33d615eecbdbdf36e6a";
    let title = "Resident Evil 2026.1080p.WEB-DL.AAC.x264-rothebehap.mp4";
    let feed = format!(
        r#"<rss xmlns:t="http://torznab.com/schemas/2015/feed"><channel>
        <item><title>{title}</title><t:attr name="infohash" value="{hash}"/><t:attr name="size" value="3981252034"/><t:attr name="imdb" value="35538033"/></item>
        <item><title>Resident.Evil.Extinction.2007.Blu-ray.Remux.1080p.mkv</title><t:attr name="infohash" value="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"/><t:attr name="size" value="20000000000"/></item>
        <item><title>Resident.Evil.Welcome.to.Raccoon.City.2021.1080p.BluRay.mkv</title><t:attr name="infohash" value="bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"/><t:attr name="size" value="13000000000"/></item>
        </channel></rss>"#
    );
    host.response("/v0/torznab/api", 200, feed.into_bytes());
    host.account(
        hash,
        json!([{"id":0,"name":format!("{title}/{title}"),"size":3981252034_u64}]),
    );
    let movie =
        json!({"mediaId":"tt35538033","mediaType":"movie","title":"Resident Evil","year":"2026"});
    let result = runtime()
        .invoke(
            host.clone(),
            &json!({"op":"lookupStreams","lookup":movie,"settings":settings("stremthru","cached")}),
        )
        .unwrap();
    let rows = result.get("streams").unwrap_or(&result).as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["title"], "[TB⚡] Supernova 1080p");
    let description = rows[0]["description"].as_str().unwrap();
    assert!(description.contains("🎞️ WEB-DL · AVC"));
    assert_eq!(
        description.lines().last(),
        Some(format!("📁 {title}").as_str())
    );
    let token: Value =
        serde_json::from_str(rows[0]["resolver"]["token"].as_str().unwrap()).unwrap();
    assert_eq!(token["infoHash"], hash);
    assert_eq!(token["fileName"], format!("{title}/{title}"));
    let requests = host.requests.lock().unwrap();
    assert!(
        requests
            .iter()
            .any(|request| request.url.contains("imdbid=tt35538033"))
    );
    assert!(requests.iter().all(|request| request.method == "GET"));
    let availability = requests
        .iter()
        .find(|request| request.url.contains("checkcached"))
        .unwrap();
    assert!(
        !availability
            .url
            .contains("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
    );
    assert!(
        !availability
            .url
            .contains("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
    );
}

#[test]
fn stremthru_non_imdb_title_search_and_configured_url_change_do_not_reuse_old_results() {
    let host = Arc::new(FixtureHost::default());
    let (bytes, hash) = torrent(&["Example S01E02.mkv"]);
    host.response(
        "/v0/torznab/api",
        200,
        stremthru_feed(&hash, "Knaben").into_bytes(),
    );
    host.response(
        &format!(
            "https://itorrents.net/torrent/{}.torrent",
            hash.to_uppercase()
        ),
        200,
        bytes,
    );
    let mut native_lookup = lookup();
    native_lookup["mediaId"] = json!("kitsu:42");
    let mut preferences = settings("stremthru", "direct");
    let request = json!({"op":"lookupStreams","lookup":native_lookup,"settings":preferences});
    let first = runtime().invoke(host.clone(), &request).unwrap();
    assert_eq!(first.as_array().unwrap().len(), 1);
    let calls = host.requests.lock().unwrap().len();
    runtime().invoke(host.clone(), &request).unwrap();
    assert_eq!(host.requests.lock().unwrap().len(), calls);
    preferences["stremthru.url"] = json!("https://alternate.stremthru.example/");
    let changed = runtime()
        .invoke(
            host.clone(),
            &json!({"op":"lookupStreams","lookup":native_lookup,"settings":preferences}),
        )
        .unwrap();
    assert_eq!(changed[0]["info_hash"], hash);
    let calls = host.requests.lock().unwrap();
    let endpoints = calls
        .iter()
        .filter(|request| request.url.contains("/v0/torznab/api"))
        .collect::<Vec<_>>();
    assert_eq!(endpoints.len(), 2);
    assert!(
        endpoints[1]
            .url
            .starts_with("https://alternate.stremthru.example/")
    );
    assert!(
        endpoints
            .iter()
            .all(|request| request.url.contains("q=Example")
                && request.url.contains("year=2020")
                && !request.url.contains("imdbid")
                && !request.url.contains("kitsu"))
    );
    assert!(
        !host
            .storage
            .lock()
            .unwrap()
            .values()
            .any(|value| value.contains("alternate.stremthru.example"))
    );
}

#[test]
fn stremthru_xml_rejects_invalid_hashes_magnets_and_untrusted_indexer_labels() {
    let hash = "0123456789012345678901234567890123456789";
    for label in [
        "https://key.example",
        "Bearer credential",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "Knaben",
    ] {
        let body = stremthru_feed(hash, label);
        let script =
            "novaProvider.handle=request=>Supernova.sources.stremthru.parse(request.body);";
        let rows = runtime_with_suffix(script)
            .invoke(crate::MemoryProviderHost::new(), &json!({"body":body}))
            .unwrap();
        assert_eq!(
            rows[0]["sources"][0],
            if label == "Knaben" {
                "StremThru|Knaben"
            } else {
                "StremThru"
            }
        );
    }
    let script = "novaProvider.handle=request=>Supernova.sources.stremthru.parse(request.body);";
    for body in [stremthru_feed("abc","Knaben"),stremthru_feed(hash,"Knaben").replace("Example S01E02 2020 1080p","Invalid Magnet"),stremthru_feed(hash,"Knaben").replace("</item>","<t:attr name='magneturl' value='magnet:?xt=urn:btih:1123456789012345678901234567890123456789'/></item>")] {
        assert_eq!(runtime_with_suffix(script).invoke(crate::MemoryProviderHost::new(),&json!({"body":body})).unwrap(),json!([]));
    }
    let host = Arc::new(FixtureHost::default());
    let mut preferences = settings("stremthru", "direct");
    preferences["stremthru.url"] = json!("http://localhost:8080");
    let output = runtime()
        .invoke(
            host.clone(),
            &json!({"op":"lookupStreams","lookup":lookup(),"settings":preferences}),
        )
        .unwrap();
    assert_eq!(output["streams"], json!([]));
    assert!(host.requests.lock().unwrap().is_empty());
    let manifest: Value = serde_json::from_str(
        crate::bundled_providers()
            .iter()
            .find(|provider| provider.url == crate::SUPERNOVA_PROVIDER_URL)
            .unwrap()
            .plugin_manifest,
    )
    .unwrap();
    assert!(
        manifest["configuration"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field["key"] == "sources.stremthru" && field["default"] == false)
    );
    assert!(
        manifest["configuration"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field["key"] == "stremthru.url"
                && field["default"] == "https://stremthru.13377001.xyz")
    );
}

#[test]
fn configurable_sort_criteria_directions_unknowns_and_stable_ties() {
    let script = r#"novaProvider.handle=request=>{
      const order=[{key:request.criterion,direction:request.direction},...Supernova.SORT_DEFAULT.filter(item=>item.key!==request.criterion)];
      return Supernova.sortRows(request.rows,{"sort.order":order}).map(row=>row.id);
    };"#;
    let row = |id: &str, title: &str, size: Value, seeders: Value, cached: bool| {
        let mut value = json!({"id":id,"description":title,"size":size,"seeders":seeders});
        if cached {
            value["resolver"] = json!({"provider_id":"supernova"});
        }
        value
    };
    for (criterion, low, high, unknown) in [
        (
            "cached",
            row("low", "Example 1080p", json!(10), json!(10), false),
            row("high", "Example 1080p", json!(10), json!(10), true),
            None,
        ),
        (
            "resolution",
            row("low", "Example 720p", json!(10), json!(10), false),
            row("high", "Example 4K", json!(10), json!(10), false),
            Some(row("unknown", "Example", json!(10), json!(10), false)),
        ),
        (
            "size",
            row("low", "Example 1080p", json!(0), json!(10), false),
            row("high", "Example 1080p", json!(100), json!(10), false),
            Some(row(
                "unknown",
                "Example 1080p",
                Value::Null,
                json!(10),
                false,
            )),
        ),
        (
            "pack",
            row("low", "Example S01E02 1080p", json!(10), json!(10), false),
            row(
                "high",
                "Example S01 Complete 1080p",
                json!(10),
                json!(10),
                false,
            ),
            Some(row("unknown", "", json!(10), json!(10), false)),
        ),
        (
            "seeders",
            row("low", "Example 1080p", json!(10), json!(0), false),
            row("high", "Example 1080p", json!(10), json!(100), false),
            Some(row(
                "unknown",
                "Example 1080p",
                json!(10),
                Value::Null,
                false,
            )),
        ),
    ] {
        for direction in ["asc", "desc"] {
            let mut rows = vec![high.clone(), low.clone()];
            if let Some(unknown) = &unknown {
                rows.push(unknown.clone());
            }
            let output = runtime_with_suffix(script)
                .invoke(
                    crate::MemoryProviderHost::new(),
                    &json!({"criterion":criterion,"direction":direction,"rows":rows}),
                )
                .unwrap();
            let mut expected = if direction == "asc" {
                vec!["low", "high"]
            } else {
                vec!["high", "low"]
            };
            if unknown.is_some() {
                expected.push("unknown");
            }
            assert_eq!(output, json!(expected), "{criterion}/{direction}");
        }
    }
    let a = row("a:1:p2p", "Example 1080p", json!(10), json!(10), false);
    let b = row("a:2:p2p", "Example 1080p", json!(10), json!(10), false);
    for rows in [vec![a.clone(), b.clone()], vec![b, a]] {
        let output = runtime_with_suffix(script)
            .invoke(
                crate::MemoryProviderHost::new(),
                &json!({"criterion":"seeders","direction":"desc","rows":rows}),
            )
            .unwrap();
        assert_eq!(output, json!(["a:1:p2p", "a:2:p2p"]));
    }
}

#[test]
fn explicit_sort_merges_cached_and_direct_before_cap_and_preserves_quality_retention() {
    let script = r#"novaProvider.handle=()=>{
      const settings={"quality.preferred":"720p","sort.order":[{key:"resolution",direction:"desc"},...Supernova.SORT_DEFAULT.filter(item=>item.key!=="resolution")]};
      const direct={id:"high:0:p2p",description:"Example S01E02 2160p",size:10,seeders:null};
      const cached=Array.from({length:55},(_,index)=>({id:`low:${index}:torbox`,description:"Example S01E02 720p",size:10,seeders:null,resolver:{provider_id:"supernova"}}));
      const records=[{infoHash:"high",title:"Example 2160p",sizeBytes:10,seeders:100},{infoHash:"low",title:"Example 720p",sizeBytes:10,seeders:0}];
      return {rows:Supernova.sortRows([...cached,direct],settings).slice(0,Supernova.MAX).map(row=>row.id),retained:Supernova.rank(records,settings).map(record=>record.infoHash)};
    };"#;
    let output = runtime_with_suffix(script)
        .invoke(crate::MemoryProviderHost::new(), &json!({}))
        .unwrap();
    assert_eq!(output["rows"][0], "high:0:p2p");
    assert_eq!(output["rows"].as_array().unwrap().len(), 50);
    assert_eq!(output["retained"], json!(["low", "high"]));
}

#[test]
fn progressive_sort_and_cache_follow_changed_whole_field_order() {
    let host = Arc::new(FixtureHost::default());
    let (bytes, hash) = torrent(&["Example S01E02.mkv"]);
    let cached_hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    host.json("/page/0",json!([
        {"infoHash":hash,"title":"Example 2020 S01E02 2160p","sizeBytes":10,"seeders":10,"torrentUrl":"https://example.com/direct.torrent","sources":["Nyaa"]},
        {"infoHash":cached_hash,"title":"Example 2020 S01E02 720p","sizeBytes":10,"seeders":10,"torrentUrl":null,"sources":["Nyaa"]}
    ]));
    host.response("/direct.torrent", 200, bytes);
    host.account(
        cached_hash,
        json!([{"id":4,"name":"Example S01E02.mkv","size":10}]),
    );
    let script = r#"Supernova.sources.nyaa.url=()=>"https://nyaa.si/page/0";Supernova.sources.nyaa.parse=JSON.parse;Supernova.sources.nyaa.pages=()=>1;"#;
    let mut preferences = settings("nyaa", "preferred");
    let mut request = json!({"op":"lookupStreams","lookup":lookup(),"settings":preferences});
    let first = runtime_with_suffix(script)
        .invoke(host.clone(), &request)
        .unwrap();
    let rows = first.get("streams").unwrap_or(&first).as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows[0]["resolver"].is_object());
    let requests = host.requests.lock().unwrap().len();
    let cached = runtime_with_suffix(script)
        .invoke(host.clone(), &request)
        .unwrap();
    assert_eq!(cached["cacheHit"], true);
    assert_eq!(cached["streams"].as_array().unwrap().len(), 2);
    assert_eq!(host.requests.lock().unwrap().len(), requests);
    preferences["sort.order"] = json!([
        {"key":"resolution","direction":"desc"},{"key":"cached","direction":"desc"},{"key":"size","direction":"desc"},{"key":"pack","direction":"desc"},{"key":"seeders","direction":"desc"}
    ]);
    request["settings"] = preferences;
    let second = runtime_with_suffix(script)
        .invoke(host.clone(), &request)
        .unwrap();
    let rows = second.get("streams").unwrap_or(&second).as_array().unwrap();
    assert_eq!(rows[0]["info_hash"], hash);
    assert!(host.requests.lock().unwrap().len() > requests);
    let publications = host.publications.lock().unwrap();
    assert_eq!(publications.last().unwrap(), &json!(rows));
    // Reordering preserves the exact stream identities between snapshots.
    let mut before = first
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].clone())
        .collect::<Vec<_>>();
    let mut after = rows.iter().map(|row| row["id"].clone()).collect::<Vec<_>>();
    before.sort_by_key(Value::to_string);
    after.sort_by_key(Value::to_string);
    assert_eq!(before, after);
}

#[test]
fn preferred_release_languages_reorder_explicit_labels_and_keep_unknown_last() {
    let script = r#"novaProvider.handle=request=>{
      const settings={"language.prefs":request.preferences,"quality.preferred":"any","sort.order":[{key:"language",direction:request.direction || "desc"},...Supernova.SORT_DEFAULT.filter(item=>item.key!=="language")]};
      const rows=[{id:"english",description:"Example 1080p [ENG]"},{id:"japanese",description:"Example 1080p Japanese"},{id:"multi",description:"Example 1080p [ENG JPN Dual Audio]"},{id:"unknown",description:"Example 1080p Multi Audio"},{id:"subtitle",description:"Example 1080p English Subs"},{id:"filename",description:"Example 1080p",resolver:{token:JSON.stringify({fileName:"English/Example [JPN].mkv"})}}];
      const records=rows.map((row,index)=>({title:row.description,infoHash:String(index)}));
      const legacy=[{key:"seeders",direction:"asc"},{key:"size",direction:"asc"},{key:"cached",direction:"desc"},{key:"pack",direction:"desc"},{key:"resolution",direction:"asc"}];
      return {rows:Supernova.sortRows(rows,settings).map(row=>row.id),records:Supernova.rank(records,settings).map(record=>record.infoHash),legacy:Supernova.sortOrder({"sort.order":legacy}),labels:request.labels.map(Supernova.releaseLanguages)};
    };"#;
    let invoke = |preferences: Value, direction: &str| {
        runtime_with_suffix(script).invoke(crate::MemoryProviderHost::new(), &json!({"preferences":preferences,"direction":direction,"labels":["ENG","English","JPN","Japanese","Dual Audio","Multi","English Subtitles","Subtitles: English","ENGlishman","English/Japanese"]})).unwrap()
    };
    let english = invoke(json!(["en", "ja"]), "desc");
    assert_eq!(
        english["rows"],
        json!([
            "english", "multi", "filename", "japanese", "subtitle", "unknown"
        ])
    );
    assert_eq!(english["records"], json!(["0", "2", "1", "3", "4", "5"]));
    assert_eq!(
        english["labels"],
        json!([
            ["en"],
            ["en"],
            ["ja"],
            ["ja"],
            [],
            [],
            [],
            [],
            [],
            ["en", "ja"]
        ])
    );
    assert_eq!(
        english["legacy"][0],
        json!({"key":"seeders","direction":"asc"})
    );
    assert_eq!(
        english["legacy"][5],
        json!({"key":"language","direction":"desc"})
    );
    assert_eq!(
        invoke(json!(["ja", "en"]), "desc")["rows"],
        json!([
            "filename", "japanese", "multi", "english", "subtitle", "unknown"
        ])
    );
    assert_eq!(
        invoke(json!(["en", "ja"]), "asc")["rows"],
        json!([
            "filename", "japanese", "english", "multi", "subtitle", "unknown"
        ])
    );
    assert_eq!(
        invoke(json!([]), "desc")["rows"],
        json!([
            "filename", "english", "japanese", "multi", "subtitle", "unknown"
        ])
    );
}

#[test]
fn preferred_video_codecs_rank_explicit_aliases_and_preserve_old_sort_orders() {
    let script = r#"novaProvider.handle=request=>{
      const settings={"codec.prefs":request.preferences,"quality.preferred":"any","sort.order":[{key:"codec",direction:request.direction},...Supernova.SORT_DEFAULT.filter(item=>item.key!=="codec")]};
      const rows=[{id:"avc",description:"Example 1080p x264"},{id:"hevc",description:"Example 1080p H.265"},{id:"av1",description:"Example 1080p AV1"},{id:"unknown",description:"Example 1080p"},{id:"conflict",description:"Example 1080p x264 HEVC"},{id:"filename",description:"Example 1080p",resolver:{token:JSON.stringify({fileName:"AV1/Example [HEVC].mkv"})}}];
      const records=rows.map((row,index)=>({title:row.description,infoHash:String(index)}));
      const old=[{key:"seeders",direction:"asc"},{key:"size",direction:"asc"},{key:"cached",direction:"desc"},{key:"pack",direction:"desc"},{key:"resolution",direction:"asc"}];
      const six=[{key:"language",direction:"asc"},...old];
      return {rows:Supernova.sortRows(rows,settings).map(row=>row.id),records:Supernova.rank(records,settings).map(record=>record.infoHash),old:Supernova.sortOrder({"sort.order":old}),six:Supernova.sortOrder({"sort.order":six}),labels:request.labels.map(Supernova.releaseVideoCodec),contradictoryFile:Supernova.codecRank(["Example AVC","Example HEVC.mkv"],settings)};
    };"#;
    let invoke = |preferences: Value, direction: &str| {
        runtime_with_suffix(script).invoke(crate::MemoryProviderHost::new(), &json!({"preferences":preferences,"direction":direction,"labels":["H264","h.264","x264","AVC","H265","h.265","x265","HEVC","AV1","HEVC AVC","[AV1Fans] Example 1080p","[AV1] Example 1080p","Example.S01EAV1","Example 1080p-AV1","Example [AV1]"]})).unwrap()
    };
    let avc = invoke(json!(["avc", "hevc", "av1"]), "desc");
    assert_eq!(
        avc["rows"],
        json!(["avc", "filename", "hevc", "av1", "conflict", "unknown"])
    );
    assert_eq!(avc["records"], json!(["0", "1", "2", "3", "4", "5"]));
    assert_eq!(
        avc["labels"],
        json!([
            "avc", "avc", "avc", "avc", "hevc", "hevc", "hevc", "hevc", "av1", null, null, null,
            null, null, "av1"
        ])
    );
    assert_eq!(avc["contradictoryFile"], Value::Null);
    assert_eq!(avc["old"][0], json!({"key":"seeders","direction":"asc"}));
    assert_eq!(avc["old"][5], json!({"key":"language","direction":"desc"}));
    assert_eq!(avc["old"][6], json!({"key":"codec","direction":"desc"}));
    assert_eq!(avc["six"][0], json!({"key":"language","direction":"asc"}));
    assert_eq!(avc["six"][6], json!({"key":"codec","direction":"desc"}));
    assert_eq!(
        invoke(json!(["av1", "hevc", "avc"]), "desc")["rows"],
        json!(["av1", "filename", "hevc", "avc", "conflict", "unknown"])
    );
    assert_eq!(
        invoke(json!(["avc", "hevc", "av1"]), "asc")["rows"],
        json!(["av1", "filename", "hevc", "avc", "conflict", "unknown"])
    );
    assert_eq!(
        invoke(json!([]), "desc")["rows"],
        json!(["filename", "av1", "avc", "conflict", "hevc", "unknown"])
    );
}

#[test]
fn rich_stream_labels_keep_explicit_metadata_and_hide_cached_peer_counts() {
    let script = r#"novaProvider.handle=()=>{
      const S=Supernova;
      const record={infoHash:'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',title:'Example.2026.1080p.WEB-DL.DDP5.1.H.265-GROUP.mkv',fileIdx:0,sizeBytes:4000000000,seeders:23,sources:['StremThru|Public']};
      const token=JSON.stringify({title:record.title,fileName:'folder/Example.2026.1080p.WEB-DL.DDP5.1.H.265-GROUP.[ENG].mkv'});
      const cached=S.row(record,true,token),direct=S.row({...record,fileName:'folder/Example.2026.1080p.mkv'},false);
      const unknown=S.row({...record,title:'Example.2026.1080p.Multi.Audio.mkv',fileName:undefined,seeders:null},false);
      const conflicting=S.row(record,true,JSON.stringify({title:record.title,fileName:'Example.2026.1080p.x264.mkv'}));
      const links=S.row({...record,title:'Example 1080p https://example.test/private?token=secret',fileName:undefined,sources:['https://example.test/private']},false);
      return {cached,direct,unknown,conflicting,links,annotated:S.cachedDiscoveryDescription(direct.description)};
    };"#;
    let output = runtime_with_suffix(script)
        .invoke(crate::MemoryProviderHost::new(), &json!({}))
        .unwrap();
    assert_eq!(output["cached"]["title"], "[TB⚡] Supernova 1080p");
    assert_eq!(output["cached"]["seeders"], Value::Null);
    let description = output["cached"]["description"].as_str().unwrap();
    assert!(description.contains("🎞️ WEB-DL · HEVC"));
    assert!(!description.lines().next().unwrap().contains("GROUP"));
    assert!(description.contains("🔊 E-AC-3 · 5.1"));
    assert!(description.contains("🔎 StremThru|Public"));
    assert!(description.contains("🌐 Audio: English"));
    assert!(
        description
            .lines()
            .last()
            .unwrap()
            .starts_with("📁 Example.")
    );
    assert!(!description.contains("seeders") && !description.contains("peers"));
    assert_eq!(output["direct"]["title"], "Supernova P2P 1080p");
    assert_eq!(output["direct"]["seeders"], 23);
    assert!(
        output["direct"]["description"]
            .as_str()
            .unwrap()
            .contains("23 reported seeders")
    );
    let unknown = output["unknown"]["description"].as_str().unwrap();
    assert!(!unknown.contains("🌐") && !unknown.contains("🔊"));
    assert!(unknown.contains("peers unverified"));
    assert!(
        !output["conflicting"]["description"]
            .as_str()
            .unwrap()
            .contains("HEVC")
    );
    assert!(
        !output["links"]["description"]
            .as_str()
            .unwrap()
            .contains("https://")
    );
    assert!(
        !output["links"]["description"]
            .as_str()
            .unwrap()
            .contains("secret")
    );
    let annotated = output["annotated"].as_str().unwrap();
    assert!(annotated.contains("Cached discovery; peer availability unverified."));
    assert!(annotated.lines().last().unwrap().starts_with("📁 "));
}

#[test]
fn rich_stream_metadata_does_not_replace_release_evidence_for_ranking() {
    let script = r#"novaProvider.handle=()=>{
      const S=Supernova,base={fileIdx:0,sizeBytes:10,seeders:1,sources:['Public']};
      const record=(hash,title)=>({...base,infoHash:hash.repeat(40),title});
      const cached=S.row(record('a','Example S01 complete 2160p HEVC [ENG]'),true,JSON.stringify({title:'Example S01 complete 2160p HEVC [ENG]',fileName:'folder/Episode.1080p.mkv'}));
      const direct=S.row({...record('b','Example S01E01 1080p'),fileName:'Episode.mkv'},false);
      const filename=S.row(record('c','Example S01E01 720p'),false);
      const settings={"sort.order":[{key:'resolution',direction:'desc'},...S.SORT_DEFAULT.filter(item=>item.key!=='resolution')]};
      return S.sortRows([filename,direct,cached],settings).map(row=>row.id);
    };"#;
    let output = runtime_with_suffix(script)
        .invoke(crate::MemoryProviderHost::new(), &json!({}))
        .unwrap();
    assert_eq!(
        output,
        json!([
            format!("{}:0:torbox", "a".repeat(40)),
            format!("{}:0:p2p", "b".repeat(40)),
            format!("{}:0:p2p", "c".repeat(40))
        ])
    );
}

#[test]
fn audio_and_subtitle_preferences_use_distinct_file_track_metadata() {
    let script = r#"novaProvider.handle=request=>{
      const settings={"language.prefs":["en","ja"],"subtitle.prefs":["en","ja"],"quality.preferred":"any","sort.order":[{key:request.key,direction:request.direction},...Supernova.SORT_DEFAULT.filter(item=>item.key!==request.key)]};
      const records=[{infoHash:"japanese",title:"Example 1080p",trackMetadata:{audioLanguages:["ja"],subtitleLanguages:["en"]}},{infoHash:"english",title:"Example 1080p",trackMetadata:{audioLanguages:["en"],subtitleLanguages:["ja"]}},{infoHash:"empty",title:"Example English Japanese Subs 1080p",trackMetadata:{audioLanguages:[],subtitleLanguages:[]}},{infoHash:"unknown",title:"Example Multi Audio Multi-Subs 1080p"}];
      const rows=[{id:"japanese",description:"📁 Example 1080p\n🌐 ja\n💬 eng"},{id:"english",description:"📁 Example 1080p\n🌐 English\n💬 Japanese"},{id:"empty",description:"📁 Example English Japanese Subs 1080p\n🌐 Unknown\n💬 Unknown"},{id:"unknown",description:"Example Multi Audio Multi-Subs 1080p"}];
      return {records:Supernova.rank(records,settings).map(record=>record.infoHash),rows:Supernova.sortRows(rows,settings).map(row=>row.id),audio:request.labels.map(Supernova.releaseLanguages),subtitles:request.labels.map(Supernova.releaseSubtitleLanguages)};
    };"#;
    for (key, direction, expected) in [
        (
            "language",
            "desc",
            json!(["english", "japanese", "empty", "unknown"]),
        ),
        (
            "language",
            "asc",
            json!(["japanese", "english", "empty", "unknown"]),
        ),
        (
            "subtitle",
            "desc",
            json!(["japanese", "english", "empty", "unknown"]),
        ),
        (
            "subtitle",
            "asc",
            json!(["english", "japanese", "empty", "unknown"]),
        ),
    ] {
        let output = runtime_with_suffix(script).invoke(crate::MemoryProviderHost::new(), &json!({"key":key,"direction":direction,"labels":["Japanese English Subs","English/Japanese Subs","Subtitles: English, Japanese","English audio Japanese subtitles","Multi-Subs","English"]})).unwrap();
        assert_eq!(output["records"], expected, "records {key}/{direction}");
        assert_eq!(output["rows"], expected, "rows {key}/{direction}");
        assert_eq!(output["audio"], json!([["ja"], [], [], ["en"], [], ["en"]]));
        assert_eq!(
            output["subtitles"],
            json!([["en"], ["en", "ja"], ["en", "ja"], ["ja"], [], []])
        );
    }
}

#[test]
fn inspected_unknown_metadata_suppresses_contradictory_release_hints() {
    let script = r#"novaProvider.handle=()=>{
      const settings={"language.prefs":["en"],"subtitle.prefs":["en"],"codec.prefs":["avc"]};
      const records=[{infoHash:"a",title:"English English Subs AVC",trackMetadata:{audioLanguages:[],subtitleLanguages:[],videoCodec:null}},{infoHash:"b",title:"Example",trackMetadata:{audioLanguages:["en"],subtitleLanguages:["en"],videoCodec:"avc"}}];
      const rows=[{id:"a",description:"🎞️ 1080p · Codec unknown\n📁 Example English English Subs AVC.mkv\n🌐 Audio: Unspecified\n💬 Subtitles: Unspecified"},{id:"b",description:"🎞️ 1080p · AVC\n📁 Example.mkv\n🌐 Audio: English\n💬 Subtitles: English"}];
      return ["language","subtitle","codec"].map(key=>{
        const sort={...settings,"quality.preferred":"any","sort.order":[{key,direction:"asc"},...Supernova.SORT_DEFAULT.filter(item=>item.key!==key)]};
        return {key,records:Supernova.rank([...records],sort).map(record=>record.infoHash),rows:Supernova.sortRows([...rows],sort).map(row=>row.id)};
      });
    };"#;
    let output = runtime_with_suffix(script)
        .invoke(crate::MemoryProviderHost::new(), &json!({}))
        .unwrap();
    for (index, key) in ["language", "subtitle", "codec"].iter().enumerate() {
        assert_eq!(output[index]["key"], json!(key));
        assert_eq!(output[index]["records"], json!(["b", "a"]));
        assert_eq!(output[index]["rows"], json!(["b", "a"]));
    }
}

#[test]
fn torbox_progress_proves_each_available_candidate_once() {
    let script = r#"
      novaProvider.handle=request=>{
        const S=Supernova,choose=S.chooseFile,render=S.row;
        let proofs=0,renders=0;const snapshots=[];
        S.chooseFile=(...args)=>{proofs++;return choose(...args);};
        S.row=(...args)=>{renders++;return render(...args);};
        const records=Array.from({length:150},(_,index)=>({
          infoHash:(index+1).toString(16).padStart(40,'0'),title:'Example (2020) S01 Complete 1080p',
          sources:['Fixture'],seeders:null,fileIdx:null,sizeBytes:4
        }));
        const warnings=[];
        const rows=S.torboxOptions(records,request.lookup,request.settings,warnings,3000,
          rows=>snapshots.push(rows.map(row=>row.id)));
        return {proofs,renders,snapshots,ids:rows.map(row=>row.id),warnings};
      };
    "#;
    let host = Arc::new(FixtureHost::default());
    let entries = (1..=150)
        .map(|index| {
            let hash = format!("{index:040x}");
            let files = if index == 1 {
                // A rejected ambiguous proof must not be retried for each later batch.
                json!([{"id":1,"name":"Example S01E02.mkv","size":4},
                       {"id":2,"name":"Example S01E02.mp4","size":4}])
            } else {
                json!([{"id":1,"name":"Example S01E02.mkv","size":4}])
            };
            (hash.clone(), json!({"hash":hash,"files":files}))
        })
        .collect::<serde_json::Map<_, _>>();
    host.json(
        "/v1/api/torrents/checkcached",
        json!({"success":true,"data":entries}),
    );
    let output = runtime_with_suffix(script)
        .invoke(
            host.clone(),
            &json!({"op":"fixture","lookup":lookup(),"settings":settings("nyaa","cached")}),
        )
        .unwrap();
    assert_eq!(output["proofs"], 150, "{output}");
    assert_eq!(output["renders"], 149);
    assert_eq!(output["warnings"], json!([]));
    let expected = (2..=150)
        .map(|index| format!("{index:040x}:unknown:torbox"))
        .collect::<Vec<_>>();
    assert_eq!(output["ids"], json!(expected));
    let snapshots = output["snapshots"].as_array().unwrap();
    assert_eq!(snapshots.len(), 3);
    assert_eq!(snapshots.last().unwrap(), &output["ids"]);
    assert!(
        snapshots
            .windows(2)
            .all(|pair| pair[0].as_array().unwrap().len() < pair[1].as_array().unwrap().len())
    );
    assert_eq!(host.requests.lock().unwrap().len(), 3);
}

#[test]
fn partial_proved_list_retries_missing_files_before_result_cache_hit() {
    for fail_second in [false, true] {
        let host = Arc::new(FixtureHost::default());
        let count = if fail_second { 2 } else { 7 };
        let records = (1..=count)
            .map(|index| {
                let (bytes, hash) = torrent(&[&format!("Example S01E02 release{index}.mkv")]);
                let route = format!("/fixture-{index}.torrent");
                if fail_second && index == 2 {
                    host.response(&route, 503, b"Unavailable".to_vec());
                } else {
                    host.response(&route, 200, bytes);
                }
                json!({"infoHash":hash,"title":"Example S01E02 2020 1080p","sources":["Nyaa"],"seeders":1,"sizeBytes":4,"torrentUrl":format!("https://nyaa.si{route}")})
            })
            .collect::<Vec<_>>();
        host.json("nyaa.si/", json!(records));
        let script = "Supernova.sources.nyaa.parse=JSON.parse;Supernova.sources.nyaa.pages=()=>1;";
        let first = runtime_with_suffix(script)
            .invoke(host.clone(), &json!({"op":"lookupStreams","lookup":lookup(),"settings":settings("nyaa","direct")}))
            .unwrap();
        assert_eq!(first.as_array().unwrap().len(), count - 1);
        if fail_second {
            let (bytes, _) = torrent(&["Example S01E02 release2.mkv"]);
            host.response("/fixture-2.torrent", 200, bytes);
        }
        let before = host.requests.lock().unwrap().len();
        let second = runtime_with_suffix(script)
            .invoke(host.clone(), &json!({"op":"lookupStreams","lookup":lookup(),"settings":settings("nyaa","direct")}))
            .unwrap();
        assert_ne!(second["cacheHit"], true, "{second}");
        assert_eq!(second.as_array().unwrap().len(), count);
        assert_eq!(host.requests.lock().unwrap().len(), before + 1);
        let third = runtime_with_suffix(script)
            .invoke(host.clone(), &json!({"op":"lookupStreams","lookup":lookup(),"settings":settings("nyaa","direct")}))
            .unwrap();
        assert_eq!(third["cacheHit"], true);
        assert_eq!(host.requests.lock().unwrap().len(), before + 1);
    }
}

#[test]
fn partial_discovery_result_cache_keeps_short_retry_expiry() {
    let (host, hash) = nyaa_fixture(&["Example S01E02.mkv"], "Example S01E02 2020 1080p");
    host.json("nyaa.si/", json!([
        {"infoHash":hash,"title":"Example S01E02 2020 1080p","sources":["Nyaa"],"seeders":1,"sizeBytes":4,"torrentUrl":"https://nyaa.si/download/7.torrent"},
        {"infoHash":"ffffffffffffffffffffffffffffffffffffffff","title":"Example S01E02 2020 720p","sources":["Nyaa"],"seeders":1,"sizeBytes":4,"torrentUrl":null}
    ]));
    let script = format!(
        "{RESULT_CACHE_CLOCK}\nSupernova.sources.nyaa.parse=JSON.parse;Supernova.sources.nyaa.pages=()=>1;Supernova.CANDIDATE_MAX=1;"
    );
    let mut request = json!({"op":"lookupStreams","lookup":lookup(),"settings":settings("nyaa","direct"),"fakeNow":100000});
    let first = runtime_with_suffix(&script)
        .invoke(host.clone(), &request)
        .unwrap();
    assert_eq!(first.as_array().unwrap().len(), 1);
    let expires_at = host
        .storage
        .lock()
        .unwrap()
        .iter()
        .find(|(key, _)| key.contains("results:v2:"))
        .map(|(_, raw)| serde_json::from_str::<Value>(raw).unwrap()["expiresAt"].clone())
        .unwrap();
    assert_eq!(expires_at, 160);
    let calls = host.requests.lock().unwrap().len();
    request["fakeNow"] = json!(110000);
    let repeated = runtime_with_suffix(&script)
        .invoke(host.clone(), &request)
        .unwrap();
    assert_eq!(repeated["cacheHit"], true);
    assert_eq!(host.requests.lock().unwrap().len(), calls);
    request["fakeNow"] = json!(161000);
    let refreshed = runtime_with_suffix(&script)
        .invoke(host.clone(), &request)
        .unwrap();
    assert_ne!(refreshed["cacheHit"], true);
    assert_eq!(refreshed.as_array().unwrap().len(), 1);
    assert!(host.requests.lock().unwrap().len() > calls);
}
