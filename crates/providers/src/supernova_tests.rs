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
"#;

#[derive(Default)]
struct FixtureHost {
    responses: Mutex<HashMap<String, HttpResponse>>,
    requests: Mutex<Vec<HttpRequest>>,
    storage: Mutex<HashMap<String, String>>,
    logs: Mutex<Vec<String>>,
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
        self.storage
            .lock()
            .unwrap()
            .insert(key.into(), value.into());
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
    assert!(pages.len() >= 2 && pages.len() <= 4);
    assert!(
        pages
            .iter()
            .any(|request| request.url.contains("offset=150"))
    );
    assert!(
        !pages
            .iter()
            .any(|request| request.url.contains("offset=300"))
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
fn nyaa_only_follows_one_advertised_second_page_without_category_filter() {
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
    assert!(pages.len() <= 4);
    assert!(pages.iter().any(|request| request.url.contains("p=2")));
    assert!(!pages.iter().any(|request| request.url.contains("p=3")
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
    assert_eq!(result["streams"], json!([]));
    assert!(!result["warnings"].as_array().unwrap().is_empty());
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
