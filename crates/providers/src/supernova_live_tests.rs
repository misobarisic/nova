//! Opt-in network smoke test. Credentials enter only through an ephemeral file;
//! every host mutation is denied, including cached-torrent creation.
use crate::{
    HttpRequest, HttpResponse, MemoryProviderHost, PluginManifest, PluginRuntime, ProviderHost,
    ProviderHostError, ScopedHttpHost,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

struct ReadOnlyLiveHost {
    network: ScopedHttpHost,
    nyaa: Option<String>,
    denied_posts: AtomicUsize,
    gets: AtomicUsize,
    public_diagnostics: Mutex<Vec<(String, u16, usize, bool, bool, &'static str)>>,
}

impl ProviderHost for ReadOnlyLiveHost {
    fn get(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
    ) -> Result<HttpResponse, ProviderHostError> {
        self.request(&HttpRequest::get(url, headers))
    }
    fn request(&self, request: &HttpRequest) -> Result<HttpResponse, ProviderHostError> {
        if request.method != "GET" {
            self.denied_posts.fetch_add(1, Ordering::Relaxed);
            return Err(ProviderHostError("live test rejects all mutations".into()));
        }
        self.gets.fetch_add(1, Ordering::Relaxed);
        let url = url::Url::parse(&request.url)
            .map_err(|_| ProviderHostError("invalid live request".into()))?;
        if url.host_str() == Some("nyaa.si")
            && let Some(body) = &self.nyaa
        {
            return Ok(HttpResponse {
                status: 200,
                body: body.as_bytes().to_vec(),
                ..Default::default()
            });
        }
        let response = self.network.request(request);
        let domain = url.host_str().unwrap_or_default();
        if matches!(
            domain,
            "apibay.org"
                | "itorrents.org"
                | "itorrents.net"
                | "nyaa.si"
                | "animetosho.org"
                | "feed.animetosho.org"
        ) {
            let (status, bytes, inspected, valid) = match &response {
                Ok(response) => {
                    let inspected = url.path().ends_with(".torrent") && response.status == 200;
                    (
                        response.status,
                        response.body.len(),
                        inspected,
                        !inspected || crate::inspect_torrent_metadata(&response.body).is_ok(),
                    )
                }
                Err(_) => (0, 0, false, false),
            };
            let error = response
                .as_ref()
                .err()
                .map(|error| safe_error_marker(&error.0))
                .unwrap_or("none");
            self.public_diagnostics.lock().unwrap().push((
                domain.into(),
                status,
                bytes,
                inspected,
                valid,
                error,
            ));
        }
        if domain == "api.torbox.app" {
            let route = match url.path() {
                "/v1/api/torrents/mylist" => "mylist",
                "/v1/api/torrents/checkcached" => "checkcached",
                "/v1/api/torrents/requestdl" => "requestdl",
                _ => "other",
            };
            let (status, bytes, marker) = match &response {
                Ok(response) => {
                    let marker = serde_json::from_slice::<Value>(&response.body)
                        .ok()
                        .map(|body| {
                            if body.get("success") == Some(&json!(true)) {
                                "api_success"
                            } else if body.get("success") == Some(&json!(false)) {
                                safe_api_error_marker(&body)
                            } else {
                                "api_shape_unknown"
                            }
                        })
                        .unwrap_or("non_json_response");
                    (response.status, response.body.len(), marker)
                }
                Err(error) => (0, 0, safe_error_marker(&error.0)),
            };
            eprintln!(
                "live TorBox HTTP: route={route} status={status} bytes={bytes} marker={marker}"
            );
        }
        response
    }
    fn storage_get(&self, _key: &str) -> Option<String> {
        None
    }
    fn storage_set(&self, _key: &str, _value: &str) -> Result<(), ProviderHostError> {
        Err(ProviderHostError(
            "live test does not persist provider state".into(),
        ))
    }
    fn log(&self, _message: &str) { /* Account/source details stay private. */
    }
}

fn safe_api_error_marker(body: &Value) -> &'static str {
    let error = body
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if error.contains("auth") || error.contains("token") || error.contains("key") {
        "api_auth_failure"
    } else if error.contains("already") || error.contains("duplicate") {
        "api_duplicate"
    } else if error.contains("cache") || error.contains("not_found") {
        "api_cache_failure"
    } else if error.contains("limit") || error.contains("slot") || error.contains("plan") {
        "api_account_limit"
    } else if error.contains("database") {
        "api_database_failure"
    } else if error.contains("link") || error.contains("requestdl") {
        "api_link_failure"
    } else {
        "api_failure"
    }
}

fn safe_error_marker(error: &str) -> &'static str {
    let error = error.to_ascii_lowercase();
    if error.contains("only https") {
        "https_policy"
    } else if error.contains("permission") {
        "domain_permission"
    } else if error.contains("non-public") {
        "non_public_address"
    } else if error.contains("across origins") {
        "cross_origin_redirect"
    } else if error.contains("redirect limit") {
        "redirect_limit"
    } else if error.contains("dns") {
        "dns_failure"
    } else if error.contains("timed out")
        || error.contains("timeout")
        || error.contains("time limit")
    {
        "timeout"
    } else if error.contains("byte limit") || error.contains("size limit") {
        "byte_limit"
    } else if error.contains("request limit") {
        "request_limit"
    } else if error.contains("cancel") {
        "cancelled"
    } else if error.contains("tls") || error.contains("certificate") {
        "tls_failure"
    } else if error.contains("transport") {
        "transport_failure"
    } else {
        "other_host_error"
    }
}

fn host(nyaa: Option<String>) -> Arc<ReadOnlyLiveHost> {
    let manifest = manifest();
    Arc::new(ReadOnlyLiveHost {
        network: ScopedHttpHost::new(
            MemoryProviderHost::new(),
            manifest.permissions.domains,
            Duration::from_secs(60),
            2 * 1024 * 1024,
            3,
            64,
        ),
        nyaa,
        denied_posts: AtomicUsize::new(0),
        gets: AtomicUsize::new(0),
        public_diagnostics: Mutex::new(Vec::new()),
    })
}

fn manifest() -> PluginManifest {
    serde_json::from_str(
        crate::bundled_providers()
            .iter()
            .find(|provider| provider.url == crate::SUPERNOVA_PROVIDER_URL)
            .expect("bundled source exists")
            .plugin_manifest,
    )
    .expect("bundled manifest is valid")
}

fn runtime() -> PluginRuntime {
    let definition = crate::bundled_providers()
        .iter()
        .find(|provider| provider.url == crate::SUPERNOVA_PROVIDER_URL)
        .expect("bundled source exists");
    let mut manifest = manifest();
    manifest.limits.request_timeout_ms = 30_000;
    // Exercise the bundled parser/resolver while ensuring no app-backed cache,
    // credential settings, or account-dependent stream URL is written to disk.
    PluginRuntime::new(
        manifest,
        format!(
            "{}\nnova.cache.get=()=>''; nova.cache.set=()=>''; nova.cache.remove=()=>'';",
            definition.source
        ),
    )
}

fn key_from_file() -> String {
    let path =
        std::env::var_os("NOVA_SUPERNOVA_TEST_KEY_FILE").expect("live test requires a key file");
    let metadata = std::fs::symlink_metadata(&path).expect("live key file is readable");
    assert!(
        metadata.file_type().is_file() && metadata.len() <= 1024,
        "live key file must be a bounded regular file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            metadata.permissions().mode() & 0o077,
            0,
            "live key file permissions must exclude other users"
        );
    }
    let key = std::fs::read_to_string(path).expect("live key file contains UTF-8");
    let key = key.trim().to_owned();
    assert!(
        !key.is_empty() && key.len() <= 512 && !key.chars().any(char::is_whitespace),
        "live key format is invalid"
    );
    key
}

fn invoke(host: Arc<ReadOnlyLiveHost>, request: Value) -> Value {
    match runtime().invoke(host, &request) {
        Ok(value) => value,
        Err(_) => panic!("live provider operation failed; upstream details intentionally omitted"),
    }
}

fn settings(source: &str, mode: &str, key: &str) -> Value {
    json!({format!("sources.{source}"):true,"quality.preferred":"any","seeders.minimum":0,
        "playback.mode":mode,"torbox.enabled":mode!="direct","torbox.apiKey":key})
}

fn candidate(file: &Value) -> Option<(String, Value)> {
    let name = file
        .get("name")
        .or_else(|| file.get("path"))?
        .as_str()?
        .rsplit('/')
        .next()?;
    if name.len() > 1024
        || !regex::Regex::new(r"(?i)\.(mkv|mp4|avi|mov|m4v|webm|ts)$")
            .ok()?
            .is_match(name)
    {
        return None;
    }
    let group = regex::Regex::new(r"^(?:\s*\[[^\]]+\]\s*)+").ok()?;
    let clean = group.replace(name, "");
    let series =
        regex::Regex::new(r"(?i)^(.+?)[ ._-]+S([0-9]{1,2})[ ._-]*E([0-9]{1,4})(?:\D|$)").ok()?;
    let movie = regex::Regex::new(r"^(.+?)[ ._(-]+((?:19|20)[0-9]{2})\b").ok()?;
    let (title, mut lookup) = if let Some(captures) = series.captures(&clean) {
        let season = captures[2].parse::<u32>().ok()?;
        let episode = captures[3].parse::<u32>().ok()?;
        if season == 0 || episode == 0 {
            return None;
        }
        (
            captures[1].to_owned(),
            json!({"mediaType":"series","season":season,"episode":episode}),
        )
    } else if let Some(captures) = movie.captures(&clean) {
        (
            captures[1].to_owned(),
            json!({"mediaType":"movie","year":captures[2].to_owned()}),
        )
    } else {
        return None;
    };
    let title = title
        .replace(['.', '_'], " ")
        .trim_matches([' ', '-', '(', ')'])
        .to_owned();
    if title.len() < 2 {
        return None;
    }
    lookup["title"] = json!(title);
    lookup["mediaId"] = json!("supernova-live-owned-account-video");
    Some((name.to_owned(), lookup))
}

fn fake_nyaa(title: &str, hash: &str) -> String {
    let title = title
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!(
        "<table class='torrent-list'><tbody><tr><td></td><td><a href='/view/1'>{title}</a></td><td><a href='magnet:?xt=urn:btih:{hash}'>magnet</a></td><td></td><td></td><td>1</td></tr></tbody></table>"
    )
}

#[test]
#[ignore = "requires explicitly authorized live network access and an ephemeral TorBox key file"]
fn live_public_sintel_source() {
    let public = host(None);
    let streams = invoke(
        public.clone(),
        json!({"op":"lookupStreams","lookup":{"mediaId":"tt1727587","mediaType":"movie","title":"Sintel","year":"2010"},"settings":settings("apibay","direct","")}),
    );
    for (domain, status, bytes, inspected, valid, error) in
        public.public_diagnostics.lock().unwrap().iter()
    {
        eprintln!(
            "live public HTTP: domain={domain} status={status} bytes={bytes} torrent_inspected={inspected} torrent_valid={valid} error={error}"
        );
    }
    let rows = streams
        .get("streams")
        .unwrap_or(&streams)
        .as_array()
        .expect("live source returns stream selections");
    assert!(
        !rows.is_empty(),
        "live Sintel discovery returned no verified direct file selections"
    );
    assert!(
        rows.iter().all(
            |row| row.get("info_hash").and_then(Value::as_str).is_some_and(
                |hash| hash.len() == 40 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
            ) && row.get("file_idx").and_then(Value::as_u64).is_some()
        ),
        "live direct rows must identify a verified torrent and file index"
    );
    assert_eq!(
        public.denied_posts.load(Ordering::Relaxed),
        0,
        "public lookup attempted a mutation"
    );
    eprintln!(
        "live public source: {} verified direct selections",
        rows.len()
    );
}

#[test]
#[ignore = "requires explicitly authorized live network access and an ephemeral TorBox key file"]
fn live_existing_torbox_cached_resolver() {
    let key = key_from_file();
    let account = host(None);
    let response = match account.request(&HttpRequest::get(
        "https://api.torbox.app/v1/api/torrents/mylist?limit=100",
        &BTreeMap::from([("Authorization".into(), format!("Bearer {key}"))]),
    )) {
        Ok(response) => response,
        Err(_) => panic!("live TorBox authentication transport failed"),
    };
    assert!(
        !matches!(response.status, 401 | 403),
        "live TorBox authentication rejected the supplied credential"
    );
    assert_eq!(
        response.status, 200,
        "live TorBox authentication endpoint was unavailable"
    );
    let response: Value = serde_json::from_slice(&response.body)
        .expect("live TorBox authentication returned valid JSON");
    assert_eq!(
        response.get("success").and_then(Value::as_bool),
        Some(true),
        "live TorBox account authentication failed"
    );
    eprintln!("live TorBox authentication: success");
    let Some(items) = response.get("data").and_then(Value::as_array) else {
        panic!("live TorBox account response was malformed");
    };
    let mut attempts = 0;
    for item in items {
        if item.get("download_finished") != Some(&json!(true))
            || item.get("download_present") == Some(&json!(false))
        {
            continue;
        }
        let Some(hash) = item
            .get("hash")
            .and_then(Value::as_str)
            .filter(|hash| hash.len() == 40 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
        else {
            continue;
        };
        let Some(files) = item.get("files").and_then(Value::as_array) else {
            continue;
        };
        for file in files {
            let Some((title, lookup)) = candidate(file) else {
                continue;
            };
            if attempts >= 4 {
                break;
            }
            attempts += 1;
            let cached = host(Some(fake_nyaa(&title, hash)));
            let rows = invoke(
                cached.clone(),
                json!({"op":"lookupStreams","lookup":lookup,"settings":settings("nyaa","cached",&key)}),
            );
            let Some(row) = rows
                .get("streams")
                .unwrap_or(&rows)
                .as_array()
                .and_then(|rows| rows.first())
            else {
                continue;
            };
            let Some(token) = row
                .get("resolver")
                .and_then(|resolver| resolver.get("token"))
                .and_then(Value::as_str)
            else {
                panic!("live cached row omitted its deferred resolver");
            };
            let snapshot = settings("nyaa", "cached", &key)
                .as_object()
                .unwrap()
                .clone();
            let resolved = match crate::registry::resolve_with_snapshot_for_test(
                cached.clone(),
                token,
                snapshot,
            ) {
                Ok(value) => value,
                Err(error) => {
                    let safe_code = match error.code.as_str() {
                        "missing_key" | "invalid_key" | "invalid_token" | "settings_changed"
                        | "not_cached" | "ambiguous_file" | "invalid_response" | "invalid_url"
                        | "unavailable" | "account_limit" | "rate_limit" | "provider_error" => {
                            error.code.as_str()
                        }
                        _ => "unknown_error",
                    };
                    panic!("live registry resolver returned sanitized error code: {safe_code}");
                }
            };
            assert_eq!(
                resolved
                    .get("novaSensitivePlayback")
                    .and_then(Value::as_bool),
                Some(true),
                "live registry must mark credential playback as sensitive"
            );
            assert_eq!(
                cached.denied_posts.load(Ordering::Relaxed),
                0,
                "live resolver attempted a forbidden mutation"
            );
            let url = resolved
                .get("url")
                .and_then(Value::as_str)
                .expect("live cached resolver omitted its playback URL");
            assert!(
                url::Url::parse(url).is_ok_and(|url| {
                    url.scheme() == "https"
                        && url.port().is_none_or(|port| port == 443)
                        && url.username().is_empty()
                        && url.password().is_none()
                        && url.fragment().is_none()
                        && manifest()
                            .permissions
                            .ephemeral_playback_credentials
                            .iter()
                            .any(|policy| {
                                url.host_str().is_some_and(|host| {
                                    policy.domains.iter().any(|domain| {
                                        domain == "*"
                                            || host == domain
                                            || host.ends_with(&format!(".{domain}"))
                                    })
                                }) && url.path().starts_with(&policy.path_prefix)
                                    && url
                                        .query_pairs()
                                        .filter(|(name, _)| {
                                            policy
                                                .query_parameters
                                                .iter()
                                                .any(|parameter| parameter == name)
                                        })
                                        .map(|(_, value)| value == key)
                                        .collect::<Vec<_>>()
                                        == vec![true]
                            })
                }),
                "live cached resolver returned an unsafe URL"
            );
            assert!(
                cached.gets.load(Ordering::Relaxed) >= 5,
                "live resolver did not exercise account and download-link checks"
            );
            eprintln!("live cached lookup and existing-torrent resolver: success (GET only)");
            return;
        }
        if attempts >= 4 {
            break;
        }
    }
    eprintln!("live cached resolver: skipped (no confidently matched cached account video)");
}

/// Print requested public discovery evidence without account credentials, links,
/// resolver tokens, or upstream response bodies.
fn safe_public_release_text(value: &str, key: Option<&str>) -> String {
    let mut value = value
        .chars()
        .take(1024)
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    if let Some(key) = key.filter(|key| !key.is_empty()) {
        value = value.replace(key, "[redacted]");
    }
    static URL: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    URL.get_or_init(|| regex::Regex::new(r"(?i)https?://[^\s]+").unwrap())
        .replace_all(&value, "[URL omitted]")
        .into_owned()
}

const ANIME_PIPELINE_DIAGNOSTICS: &str = r#"
(() => {
  const S = globalThis.Supernova;
  const stats = {};
  const matched = {}, verified = {};
  for (const [id, source] of Object.entries(S.sources)) {
    stats[id] = {parsedRows:0,parseFailures:0,matchedRecords:0,verifiedFiles:0};
    matched[id] = new Set(); verified[id] = new Set();
    const parse = source.parse;
    source.parse = body => {
      try { const rows = parse(body); stats[id].parsedRows += rows.length; return rows; }
      catch (error) { stats[id].parseFailures++; throw error; }
    };
  }
  const match = S.candidateMatches;
  S.candidateMatches = (record, lookup) => {
    const accepted = match(record, lookup);
    if (accepted) for (const [id, source] of Object.entries(S.sources)) {
      if (record.sources.includes(source.name)) matched[id].add(record.infoHash);
    }
    return accepted;
  };
  const choose = S.chooseFile;
  S.chooseFile = (...args) => {
    const [files,record,lookup,torbox] = args;
    const file = choose(...args);
    if (file && !torbox) for (const [id, source] of Object.entries(S.sources)) {
      if (record.sources.includes(source.name)) verified[id].add(record.infoHash);
    }
    return file;
  };
  const candidates = new Map();
  let preCapCandidates = 0;
  const rank = S.rank;
  S.rank = (records, settings) => {
    preCapCandidates = Math.max(preCapCandidates,records.length);
    const sorted = rank(records,settings);
    for (const record of sorted.slice(0,S.CANDIDATE_MAX)) candidates.set(record.infoHash,record);
    return sorted;
  };
  const cacheStatus = {};
  const inspectAvailability = (request,response) => {
    if (!request || !String(request.url).startsWith('https://api.torbox.app/v1/api/torrents/checkcached?')) return;
    const match=request.url.match(/[?&]hash=([^&]+)/);
    const hashes=match ? match[1].split(',').map(S.hash).filter(Boolean) : [];
    const body=response && S.json(response.body);
    const okay=response && response.status===200 && body && body.success===true && body.data && typeof body.data==='object';
    for(const hash of hashes)cacheStatus[hash]=okay ? 'not_cached' : 'check_failed';
    if(okay)for(const hash of hashes){const entry=body.data[hash] || body.data[hash.toUpperCase()];if(entry && S.hash(entry.hash)===hash)cacheStatus[hash]='advertised_cached';}
  };
  const httpBatch=nova.http.batch;
  nova.http.batch=raw=>{const requests=S.json(raw),result=httpBatch(raw),responses=S.json(result);if(Array.isArray(requests))requests.forEach((request,index)=>inspectAvailability(request,Array.isArray(responses) && responses[index]));return result;};
  const httpBatchEach=nova.http.batchEach;
  nova.http.batchEach=(raw,callback)=>{const requests=S.json(raw);return httpBatchEach(raw,(index,response)=>{inspectAvailability(requests && requests[index],S.json(response));callback(index,response);});};
  const publications=[];
  const publish=nova.streams.publish;
  let publishStarted=0;
  nova.streams.publish=raw=>{const rows=S.json(raw);publications.push({elapsedMs:Date.now()-publishStarted,rows:Array.isArray(rows)?rows.length:0});return publish(raw);};
  let cache = {};
  let cacheHits = 0;
  nova.cache.get = key => { if (cache[key]) cacheHits++; return cache[key] || ''; };
  nova.cache.set = (key, value) => { cache[key] = value; return ''; };
  nova.cache.remove = key => { delete cache[key]; return ''; };
  nova.cache.getMemory = key => { if (cache['memory:' + key]) cacheHits++; return cache['memory:' + key] || ''; };
  nova.cache.setMemory = (key,value) => { cache['memory:' + key] = value; return ''; };
  const handle = globalThis.novaProvider.handle;
  globalThis.novaProvider.handle = request => {
    cache = request.diagnosticCache || {};
    publishStarted=Date.now();
    const result = handle(request);
    for (const id of Object.keys(stats)) {
      stats[id].matchedRecords = matched[id].size;
      stats[id].verifiedFiles = verified[id].size;
    }
    const rows = Array.isArray(result) ? result : result.streams;
    const cachedRows = new Set(rows.filter(row=>row.resolver).map(row=>String(row.id).split(':')[0]));
    const publicCandidates = [...candidates.values()].map(record => ({title:record.title,infoHash:record.infoHash,fileIdx:record.fileIdx,sources:record.sources,
      kind:S.matches(record,request.lookup) ? 'individual' : 'pack_or_title_only',
      torboxStatus:cachedRows.has(record.infoHash) ? 'cached_playable' : cacheStatus[record.infoHash] || 'not_checked'}));
    return {result,stats,cacheHits,publications,directRows:rows.filter(row=>row.info_hash).length,cachedRows:cachedRows.size,preCapCandidates,publicCandidates,diagnosticCache:cache};
  };
})();
"#;

#[test]
#[ignore = "requires explicitly authorized public anime source requests; no account key is needed"]
fn live_public_anime_discovery_pipeline() {
    let definition = crate::bundled_providers()
        .iter()
        .find(|provider| provider.url == crate::SUPERNOVA_PROVIDER_URL)
        .expect("bundled source exists");
    let account_key = std::env::var_os("NOVA_SUPERNOVA_TEST_KEY_FILE").map(|_| key_from_file());
    for (case, title, year) in [
        ("original", "Steins;Gate", "2011"),
        ("sequel", "Steins;Gate 0", "2018"),
        ("bluebox", "Blue Box", "2024"),
    ] {
        let mut local_cache = json!({});
        for repeat in 1..=2 {
            let public = host(None);
            let runtime = PluginRuntime::new(
                manifest(),
                format!("{}\n{}", definition.source, ANIME_PIPELINE_DIAGNOSTICS),
            );
            let request = json!({"op":"lookupStreams","lookup":{"mediaId":format!("live-anime-{case}"),"mediaType":"series","title":title,"year":year,"season":1,"episode":1,"absoluteEpisode":1},"settings":{"sources.nyaa":true,"sources.animetosho":true,"quality.preferred":"any","seeders.minimum":0,"playback.mode":if account_key.is_some(){"preferred"}else{"direct"},"torbox.enabled":account_key.is_some(),"torbox.apiKey":account_key.as_deref().unwrap_or("")},"diagnosticCache":local_cache});
            let output = match runtime.invoke(public.clone(), &request) {
                Ok(value) => value,
                Err(error) => {
                    let message =
                        safe_public_release_text(&error.to_string(), account_key.as_deref());
                    eprintln!(
                        "live anime pipeline: case={case} repeat={repeat} operation_failed=true error={message}"
                    );
                    for (domain, status, bytes, inspected, valid, error) in
                        public.public_diagnostics.lock().unwrap().iter()
                    {
                        eprintln!(
                            "live anime HTTP: domain={domain} status={status} bytes={bytes} inspected={inspected} valid={valid} error={error}"
                        );
                    }
                    panic!("live anime operation failed for {case}, repeat {repeat}");
                }
            };
            local_cache = output
                .get("diagnosticCache")
                .cloned()
                .unwrap_or_else(|| json!({}));
            for id in ["nyaa", "animetosho"] {
                let stats = &output["stats"][id];
                let parsed = stats["parsedRows"].as_u64().unwrap_or_default();
                let matched = stats["matchedRecords"].as_u64().unwrap_or_default();
                let verified = stats["verifiedFiles"].as_u64().unwrap_or_default();
                let parse_failures = stats["parseFailures"].as_u64().unwrap_or_default();
                eprintln!(
                    "live anime stages: case={case} repeat={repeat} source={id} parsed={parsed} matched={matched} verified={verified} parse_failures={parse_failures}"
                );
            }
            let cache_hits = output["cacheHits"].as_u64().unwrap_or_default();
            let direct_rows = output["directRows"].as_u64().unwrap_or_default();
            let warnings = output["result"]
                .get("warnings")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            eprintln!(
                "live anime result: case={case} repeat={repeat} cache_hits={cache_hits} direct_rows={direct_rows} warnings={warnings}"
            );
            for snapshot in output["publications"].as_array().into_iter().flatten() {
                let elapsed = snapshot["elapsedMs"].as_u64().unwrap_or_default();
                let count = snapshot["rows"].as_u64().unwrap_or_default();
                eprintln!(
                    "live anime snapshot: case={case} repeat={repeat} elapsed_ms={elapsed} rows={count}"
                );
            }
            let cached_rows = output["cachedRows"].as_u64().unwrap_or_default();
            let pre_cap = output["preCapCandidates"].as_u64().unwrap_or_default();
            let candidates = output["publicCandidates"]
                .as_array()
                .expect("diagnostic candidate array exists");
            let individuals = candidates
                .iter()
                .filter(|row| row["kind"] == "individual")
                .count();
            eprintln!(
                "live anime inventory: case={case} repeat={repeat} matched_before_cap={pre_cap} checked_candidates={} individuals={individuals} pack_or_title_only={} cached_playable={cached_rows}",
                candidates.len(),
                candidates.len() - individuals
            );
            for candidate in candidates {
                let title = safe_public_release_text(
                    candidate["title"].as_str().unwrap_or_default(),
                    account_key.as_deref(),
                );
                let hash = safe_public_release_text(
                    candidate["infoHash"].as_str().unwrap_or_default(),
                    account_key.as_deref(),
                );
                let sources = safe_public_release_text(
                    &candidate["sources"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" + "),
                    account_key.as_deref(),
                );
                let index = candidate["fileIdx"]
                    .as_u64()
                    .map_or_else(|| "unverified".into(), |index| index.to_string());
                let kind = candidate["kind"].as_str().unwrap_or("unknown");
                let status = candidate["torboxStatus"].as_str().unwrap_or("unknown");
                eprintln!(
                    "live public release: case={case} repeat={repeat} title={title} hash={hash} file_idx={index} sources={sources} kind={kind} torbox={status}"
                );
            }
            for (domain, status, bytes, inspected, valid, error) in
                public.public_diagnostics.lock().unwrap().iter()
            {
                eprintln!(
                    "live anime HTTP: domain={domain} status={status} bytes={bytes} inspected={inspected} valid={valid} error={error}"
                );
            }
            assert_eq!(
                public.denied_posts.load(Ordering::Relaxed),
                0,
                "public anime lookup attempted a mutation"
            );
            let rows = output["result"]
                .get("streams")
                .unwrap_or(&output["result"])
                .as_array()
                .expect("live anime returns stream rows");
            assert!(
                rows.iter().all(|row| row.get("resolver").is_some()
                    || (row.get("info_hash").is_some()
                        && row.get("file_idx").and_then(Value::as_u64).is_some())),
                "live anime direct selections must identify verified files"
            );
        }
    }
}
