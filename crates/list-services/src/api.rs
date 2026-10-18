use crate::*;
use serde_json::{Value, json};
use std::{
    io::Read,
    sync::{Arc, Mutex},
    time::Duration,
};

const MAX_BODY: u64 = 16 * 1024 * 1024;
const MAX_TITLES: usize = 50_000;
const MAX_PAGES: usize = 100;

/// Deliberately has no Debug implementation: MDBList authenticates via query key.
pub struct Request {
    pub method: &'static str,
    pub url: String,
    pub bearer: Option<Secret>,
    pub body: Option<Value>,
}
impl Drop for Request {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.url.zeroize();
        if let Some(body) = self.body.as_mut() {
            wipe_json(body);
        }
    }
}
fn wipe_json(value: &mut Value) {
    use zeroize::Zeroize;
    match value {
        Value::String(s) => s.zeroize(),
        Value::Array(a) => a.iter_mut().for_each(wipe_json),
        Value::Object(m) => m.values_mut().for_each(wipe_json),
        _ => {}
    }
}
pub struct Response {
    pub status: u16,
    pub retry_after: Option<u64>,
    pub has_more: bool,
    pub body: Value,
}
pub trait Transport: Send + Sync {
    fn send(&self, request: Request) -> Result<Response, ApiError>;
}
#[derive(Clone)]
pub struct HttpsTransport {
    client: reqwest::blocking::Client,
}
impl HttpsTransport {
    pub fn new() -> Result<Self, ApiError> {
        let builder = reqwest::blocking::Client::builder()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .user_agent(concat!(
                "Nova/",
                env!("CARGO_PKG_VERSION"),
                " list-services"
            ));
        // Android's platform verifier needs JNI/Kotlin components cargo-apk2
        // does not package. Match Nova's other transports with bundled roots.
        #[cfg(target_os = "android")]
        let builder = {
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::aws_lc_rs::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .map_err(|_| ApiError::Offline)?
            .with_root_certificates(roots)
            .with_no_client_auth();
            builder.tls_backend_preconfigured(tls)
        };
        Ok(Self {
            client: builder.build().map_err(|_| ApiError::Offline)?,
        })
    }
}
impl Transport for HttpsTransport {
    fn send(&self, request: Request) -> Result<Response, ApiError> {
        let url = url::Url::parse(&request.url).map_err(|_| ApiError::InvalidInput)?;
        if url.scheme() != "https"
            || !matches!(url.host_str(), Some("api.simkl.com" | "api.mdblist.com"))
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some()
            || url.fragment().is_some()
        {
            return Err(ApiError::InvalidInput);
        }
        let method = reqwest::Method::from_bytes(request.method.as_bytes())
            .map_err(|_| ApiError::InvalidInput)?;
        let mut builder = self.client.request(method, url);
        if let Some(token) = &request.bearer {
            builder = builder.bearer_auth(token.expose());
        }
        if let Some(body) = &request.body {
            builder = builder.json(body);
        }
        let mut response = builder.send().map_err(|_| ApiError::Offline)?;
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.parse().ok());
        let has_more = response
            .headers()
            .get("x-has-more")
            .is_some_and(|h| h == "true");
        let mut bytes = zeroize::Zeroizing::new(Vec::new());
        response
            .by_ref()
            .take(MAX_BODY + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ApiError::Offline)?;
        if bytes.len() as u64 > MAX_BODY {
            return Err(ApiError::InvalidResponse);
        }
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            match serde_json::from_slice(&bytes) {
                Ok(value) => value,
                Err(_) if !(200..300).contains(&status) => Value::Null,
                Err(_) => return Err(ApiError::InvalidResponse),
            }
        };
        Ok(Response {
            status,
            retry_after,
            has_more,
            body,
        })
    }
}
fn checked(response: Response) -> Result<Response, ApiError> {
    match response.body.get("error").and_then(Value::as_str) {
        Some("authorization_pending") => return Err(ApiError::Pending),
        Some("slow_down") => return Err(ApiError::SlowDown),
        Some("expired_token" | "access_denied") => return Err(ApiError::Expired),
        _ => {}
    }
    match response.status {
        200..=299 if response.body.get("error").is_some_and(contains_failure) => {
            Err(ApiError::Rejected)
        }
        200..=299 => Ok(response),
        401 | 403 | 412 => Err(ApiError::Authentication),
        429 => Err(ApiError::RateLimited {
            retry_after: response.retry_after.unwrap_or(60).clamp(1, 86400),
        }),
        500..=599 => Err(ApiError::Offline),
        _ => Err(ApiError::Rejected),
    }
}
struct SimklCache {
    credentials: Credentials,
    activities: Value,
    titles: Vec<RemoteTitle>,
}
pub struct Client {
    transport: Arc<dyn Transport>,
    simkl: Mutex<Option<SimklCache>>,
}
impl Client {
    pub fn new(transport: Arc<dyn Transport>) -> Self {
        Self {
            transport,
            simkl: Mutex::new(None),
        }
    }
    fn request(
        &self,
        credentials: &Credentials,
        method: &'static str,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
    ) -> Result<Response, ApiError> {
        validate_credentials(credentials)?;
        let base = match credentials.service {
            Service::Simkl => "https://api.simkl.com",
            Service::MdbList => "https://api.mdblist.com",
        };
        let mut url =
            url::Url::parse(&format!("{base}{path}")).map_err(|_| ApiError::InvalidInput)?;
        let bearer = match credentials.service {
            Service::Simkl => {
                url.query_pairs_mut()
                    .append_pair("client_id", &credentials.client_id)
                    .append_pair("app-name", "nova")
                    .append_pair("app-version", env!("CARGO_PKG_VERSION"));
                Some(credentials.token.clone())
            }
            Service::MdbList => {
                url.query_pairs_mut()
                    .append_pair("apikey", credentials.token.expose());
                None
            }
        };
        for (k, v) in query {
            url.query_pairs_mut().append_pair(k, v);
        }
        checked(self.transport.send(Request {
            method,
            url: url.into(),
            bearer,
            body,
        })?)
    }
    pub fn verify(&self, credentials: &Credentials) -> Result<Account, ApiError> {
        let value = self
            .request(
                credentials,
                "GET",
                match credentials.service {
                    Service::Simkl => "/users/settings",
                    Service::MdbList => "/user",
                },
                &[],
                None,
            )?
            .body;
        let (id, name) = match credentials.service {
            Service::Simkl => (&value["account"]["id"], &value["user"]["name"]),
            Service::MdbList => (
                &value["user_id"],
                value
                    .get("username")
                    .or_else(|| value.get("name"))
                    .unwrap_or(&Value::Null),
            ),
        };
        let id = if let Some(id) = id.as_u64().filter(|n| *n > 0) {
            id.to_string()
        } else {
            string(id)?
        };
        Ok(Account {
            id,
            name: string(name)?,
        })
    }
    /// Only restore after verify confirms the same account that owns this baseline.
    /// Keep this snapshot device-local, separate from cross-device preferences.
    pub fn restore_simkl_checkpoint(
        &self,
        credentials: &Credentials,
        checkpoint: SimklCheckpoint,
    ) -> Result<(), ApiError> {
        if credentials.service != Service::Simkl {
            return Err(ApiError::Unsupported);
        }
        validate_credentials(credentials)?;
        string(&checkpoint.activities["all"])?;
        if checkpoint.titles.len() > MAX_TITLES
            || checkpoint.titles.iter().any(|t| {
                !t.ids.valid()
                    || t.episodes.len() > MAX_TITLES
                    || t.progress
                        .is_some_and(|p| !p.is_finite() || !(0.0..=100.0).contains(&p))
            })
        {
            return Err(ApiError::InvalidInput);
        }
        *self.simkl.lock().map_err(|_| ApiError::Offline)? = Some(SimklCache {
            credentials: credentials.clone(),
            activities: checkpoint.activities,
            titles: checkpoint.titles,
        });
        Ok(())
    }
    pub fn simkl_checkpoint(&self, credentials: &Credentials) -> Option<SimklCheckpoint> {
        let cache = self.simkl.lock().ok()?;
        let c = cache.as_ref()?;
        if credentials.service != Service::Simkl
            || c.credentials.client_id != credentials.client_id
            || c.credentials.token != credentials.token
        {
            return None;
        }
        Some(SimklCheckpoint {
            activities: c.activities.clone(),
            titles: c.titles.clone(),
        })
    }
    /// Simkl checks activities before deltas and reconciles removal timestamps.
    /// Retain the Client for the signed-in session so repeated calls are incremental.
    pub fn library(&self, credentials: &Credentials) -> Result<Vec<RemoteTitle>, ApiError> {
        match credentials.service {
            Service::Simkl => self.simkl_library(credentials),
            Service::MdbList => {
                let mut titles = Vec::new();
                for (path, status) in [
                    ("/watchlist/items", ListStatus::Planning),
                    ("/sync/collection", ListStatus::Planning),
                    ("/sync/watched", ListStatus::Completed),
                ] {
                    let rows = self.mdb_pages(credentials, path, status)?;
                    merge_titles(&mut titles, rows);
                }
                let playback = self
                    .request(credentials, "GET", "/sync/playback", &[], None)?
                    .body;
                apply_playback(&mut titles, &playback)?;
                if titles.len() > MAX_TITLES {
                    return Err(ApiError::InvalidResponse);
                }
                Ok(titles)
            }
        }
    }
    fn simkl_library(&self, credentials: &Credentials) -> Result<Vec<RemoteTitle>, ApiError> {
        let mut cache = self.simkl.lock().map_err(|_| ApiError::Offline)?;
        if cache.as_ref().is_some_and(|c| {
            c.credentials.client_id != credentials.client_id
                || c.credentials.token != credentials.token
        }) {
            *cache = None;
        }
        let activities = self
            .request(credentials, "GET", "/sync/activities", &[], None)?
            .body;
        string(&activities["all"])?;
        if let Some(c) = cache.as_ref()
            && c.activities["all"] == activities["all"]
        {
            return Ok(c.titles.clone());
        }
        let mut titles = cache.as_ref().map(|c| c.titles.clone()).unwrap_or_default();
        let mut query = vec![
            ("extended", "full".to_owned()),
            ("include_all_episodes", "original".to_owned()),
        ];
        if let Some(c) = cache.as_ref() {
            query.push(("date_from", string(&c.activities["all"])?));
            let value = self
                .request(credentials, "GET", "/sync/all-items", &query, None)?
                .body;
            let changed = parse_titles(&value, ListStatus::Planning, true)?;
            // Each Simkl delta row is the authoritative complete title state,
            // including unwatching episodes. Do not union stale episode rows.
            titles.retain(|old| !changed.iter().any(|row| same_title(old, row)));
            merge_titles(&mut titles, changed);
            let removed = ["tv_shows", "movies", "anime"].iter().any(|kind| {
                c.activities[kind]["removed_from_list"] != activities[kind]["removed_from_list"]
            });
            if removed {
                let value = self
                    .request(
                        credentials,
                        "GET",
                        "/sync/all-items",
                        &[("extended", "ids_only".to_owned())],
                        None,
                    )?
                    .body;
                let current = parse_titles(&value, ListStatus::Planning, true)?;
                titles.retain(|t| current.iter().any(|r| same_title(t, r)));
            }
        } else {
            // Simkl explicitly requests sequential initial per-type pulls.
            for kind in ["shows", "movies", "anime"] {
                let value = self
                    .request(
                        credentials,
                        "GET",
                        &format!("/sync/all-items/{kind}"),
                        &query,
                        None,
                    )?
                    .body;
                merge_titles(
                    &mut titles,
                    parse_titles(&value, ListStatus::Planning, true)?,
                );
            }
        }
        let playback = self
            .request(credentials, "GET", "/sync/playback", &[], None)?
            .body;
        for title in &mut titles {
            title.progress = None;
            title.playback_episode = None;
        }
        apply_playback(&mut titles, &playback)?;
        if titles.len() > MAX_TITLES {
            return Err(ApiError::InvalidResponse);
        }
        *cache = Some(SimklCache {
            credentials: credentials.clone(),
            activities,
            titles: titles.clone(),
        });
        Ok(titles)
    }
    fn mdb_pages(
        &self,
        credentials: &Credentials,
        path: &str,
        status: ListStatus,
    ) -> Result<Vec<RemoteTitle>, ApiError> {
        let mut result = Vec::new();
        let mut cursor: Option<String> = None;
        let mut offset = 0usize;
        for _ in 0..MAX_PAGES {
            let mut query = vec![("limit", "1000".to_owned())];
            if let Some(c) = cursor.as_ref() {
                query.push(("cursor", c.clone()));
            } else {
                query.push(("offset", offset.to_string()));
            }
            let response = self.request(credentials, "GET", path, &query, None)?;
            let count = ["movies", "shows", "episodes", "seasons"]
                .iter()
                .filter_map(|k| response.body[k].as_array())
                .map(Vec::len)
                .sum::<usize>();
            merge_titles(&mut result, parse_titles(&response.body, status, false)?);
            if result.len() > MAX_TITLES {
                return Err(ApiError::InvalidResponse);
            }
            let next = response
                .body
                .pointer("/pagination/next_cursor")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
            let total = response
                .body
                .pointer("/pagination/total")
                .and_then(Value::as_u64);
            let body_has_more = response
                .body
                .pointer("/pagination/has_more")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if next.is_none()
                && !response.has_more
                && !body_has_more
                && total.is_none_or(|t| (offset + count) as u64 >= t)
            {
                return Ok(result);
            }
            if count == 0 || next.as_ref().is_some_and(|n| Some(n) == cursor.as_ref()) {
                return Err(ApiError::InvalidResponse);
            }
            offset = offset.saturating_add(count);
            cursor = next;
        }
        Err(ApiError::InvalidResponse)
    }
    pub fn set_list_status(
        &self,
        credentials: &Credentials,
        ids: &MediaIds,
        kind: MediaType,
        status: ListStatus,
    ) -> Result<(), ApiError> {
        let mut item = json!({"ids":request_ids(ids,credentials.service)?});
        let path = match credentials.service {
            Service::Simkl => {
                if status == ListStatus::Removed {
                    return Err(ApiError::Unsupported);
                }
                item["to"] = json!(match status {
                    ListStatus::Planning => "plantowatch",
                    ListStatus::Watching => "watching",
                    ListStatus::Completed => "completed",
                    ListStatus::OnHold => "hold",
                    ListStatus::Dropped => "dropped",
                    ListStatus::Removed => unreachable!(),
                });
                "/sync/add-to-list"
            }
            Service::MdbList => match status {
                ListStatus::Planning => "/watchlist/items/add",
                ListStatus::Removed => "/watchlist/items/remove",
                _ => return Err(ApiError::Unsupported),
            },
        };
        let body = grouped(kind, item);
        mutation_receipt(
            self.request(credentials, "POST", path, &[], Some(body))?
                .body,
        )
    }
    pub fn set_watched(
        &self,
        credentials: &Credentials,
        ids: &MediaIds,
        kind: MediaType,
        episode: Option<Episode>,
        watched: bool,
    ) -> Result<(), ApiError> {
        validate_episode(kind, episode)?;
        let mut item = json!({"ids":request_ids(ids,credentials.service)?});
        if let Some(ep) = episode {
            item["seasons"] = json!([{"number":ep.season,"episodes":[{"number":ep.number}]}]);
        }
        let path = match (credentials.service, watched) {
            (Service::Simkl, true) => "/sync/history",
            (Service::Simkl, false) => "/sync/history/remove",
            (Service::MdbList, true) => "/sync/watched",
            (Service::MdbList, false) => "/sync/watched/remove",
        };
        mutation_receipt(
            self.request(credentials, "POST", path, &[], Some(grouped(kind, item)))?
                .body,
        )
    }
    pub fn scrobble(
        &self,
        credentials: &Credentials,
        ids: &MediaIds,
        kind: MediaType,
        episode: Option<Episode>,
        progress: f64,
        action: ScrobbleAction,
    ) -> Result<(), ApiError> {
        validate_episode(kind, episode)?;
        if !progress.is_finite()
            || !(0.0..=100.0).contains(&progress)
            || (kind != MediaType::Movie && episode.is_none())
        {
            return Err(ApiError::InvalidInput);
        }
        let mut body =
            json!({"progress":progress,"app_name":"Nova","app_version":env!("CARGO_PKG_VERSION")});
        if kind == MediaType::Movie {
            body["movie"] = json!({"ids":request_ids(ids,credentials.service)?});
        } else {
            let ep = episode.ok_or(ApiError::InvalidInput)?;
            body["show"] = json!({"ids":request_ids(ids,credentials.service)?});
            if credentials.service == Service::MdbList {
                body["show"]["season"] = json!(ep.season);
                body["show"]["episode"] = json!(ep.number);
            } else {
                body["episode"] = json!({"season":ep.season,"number":ep.number});
            }
        }
        let action = match action {
            ScrobbleAction::Start => "start",
            ScrobbleAction::Pause => "pause",
            ScrobbleAction::Stop => "stop",
        };
        mutation_receipt(
            self.request(
                credentials,
                "POST",
                &format!("/scrobble/{action}"),
                &[],
                Some(body),
            )?
            .body,
        )
    }
    pub fn lists(&self, credentials: &Credentials) -> Result<Vec<RemoteList>, ApiError> {
        if credentials.service != Service::MdbList {
            return Err(ApiError::Unsupported);
        }
        let body = self
            .request(credentials, "GET", "/lists/user", &[], None)?
            .body;
        serde_json::from_value(body).map_err(|_| ApiError::InvalidResponse)
    }
    pub fn list_items(
        &self,
        credentials: &Credentials,
        list_id: u64,
    ) -> Result<Vec<RemoteTitle>, ApiError> {
        if credentials.service != Service::MdbList {
            return Err(ApiError::Unsupported);
        }
        if list_id == 0 {
            return Err(ApiError::InvalidInput);
        }
        self.mdb_pages(
            credentials,
            &format!("/lists/{list_id}/items"),
            ListStatus::Planning,
        )
    }
    /// MDBList permits mutations of owned static lists, rejecting dynamic/foreign lists.
    pub fn set_list_item(
        &self,
        credentials: &Credentials,
        list_id: u64,
        ids: &MediaIds,
        kind: MediaType,
        present: bool,
    ) -> Result<(), ApiError> {
        if credentials.service != Service::MdbList {
            return Err(ApiError::Unsupported);
        }
        if list_id == 0 {
            return Err(ApiError::InvalidInput);
        }
        let action = if present { "add" } else { "remove" };
        let body = grouped(kind, request_ids(ids, credentials.service)?);
        mutation_receipt(
            self.request(
                credentials,
                "POST",
                &format!("/lists/{list_id}/items/{action}"),
                &[],
                Some(body),
            )?
            .body,
        )
    }
    /// MDBList's collected library is distinct from its plan-to-watch watchlist.
    pub fn set_collection(
        &self,
        credentials: &Credentials,
        ids: &MediaIds,
        kind: MediaType,
        present: bool,
    ) -> Result<(), ApiError> {
        if credentials.service != Service::MdbList {
            return Err(ApiError::Unsupported);
        }
        let path = if present {
            "/sync/collection"
        } else {
            "/sync/collection/remove"
        };
        let body = grouped(kind, json!({"ids":request_ids(ids, credentials.service)?}));
        mutation_receipt(
            self.request(credentials, "POST", path, &[], Some(body))?
                .body,
        )
    }
    pub fn begin_device(&self, client_id: &str) -> Result<DeviceCode, ApiError> {
        validate_client_id(client_id)?;
        let body = self.device_request(
            "/oauth2/device",
            json!({"client_id":client_id,"scope":"media:read media:write"}),
        )?;
        let code: DeviceCode =
            serde_json::from_value(body).map_err(|_| ApiError::InvalidResponse)?;
        let uri = url::Url::parse(&code.verification_uri).map_err(|_| ApiError::InvalidResponse)?;
        if let Some(complete) = &code.verification_uri_complete {
            validate_verification_uri(complete)?;
        }
        validate_verification_uri(&code.verification_uri)?;
        if uri.scheme() != "https"
            || uri.host_str() != Some("simkl.com")
            || code.expires_in == 0
            || code.expires_in > 3600
            || code.interval == 0
            || code.interval > 60
            || code.user_code.is_empty()
            || code.user_code.len() > 32
        {
            return Err(ApiError::InvalidResponse);
        }
        Ok(code)
    }
    /// Caller enforces expires_in and interval; SlowDown adds five seconds.
    pub fn poll_device(&self, client_id: &str, device_code: &Secret) -> Result<Tokens, ApiError> {
        validate_client_id(client_id)?;
        parse_tokens(self.device_request("/oauth2/token",json!({"grant_type":"urn:ietf:params:oauth:grant-type:device_code","client_id":client_id,"device_code":device_code.expose()}))?)
    }
    /// Persist the returned token pair atomically; refresh invalidates old access tokens.
    pub fn refresh(&self, client_id: &str, refresh_token: &Secret) -> Result<Tokens, ApiError> {
        validate_client_id(client_id)?;
        parse_tokens(self.device_request("/oauth2/token",json!({"grant_type":"refresh_token","client_id":client_id,"refresh_token":refresh_token.expose()}))?)
    }
    fn device_request(&self, path: &str, body: Value) -> Result<Value, ApiError> {
        Ok(checked(self.transport.send(Request {
            method: "POST",
            url: format!("https://api.simkl.com{path}"),
            bearer: None,
            body: Some(body),
        })?)?
        .body)
    }
}
fn validate_verification_uri(value: &str) -> Result<(), ApiError> {
    let uri = url::Url::parse(value).map_err(|_| ApiError::InvalidResponse)?;
    if uri.scheme() != "https"
        || uri.host_str() != Some("simkl.com")
        || !uri.username().is_empty()
        || uri.password().is_some()
        || uri.port().is_some()
    {
        return Err(ApiError::InvalidResponse);
    }
    Ok(())
}
fn validate_credentials(c: &Credentials) -> Result<(), ApiError> {
    Secret::new(c.token.expose().to_owned())?;
    if c.service == Service::Simkl {
        validate_client_id(&c.client_id)?;
    }
    Ok(())
}
fn validate_client_id(client_id: &str) -> Result<(), ApiError> {
    if client_id.is_empty()
        || client_id.len() > 256
        || !client_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(ApiError::InvalidInput);
    }
    Ok(())
}
fn validate_episode(kind: MediaType, episode: Option<Episode>) -> Result<(), ApiError> {
    if episode.is_some_and(|ep| {
        kind == MediaType::Movie || ep.number == 0 || ep.season > 10000 || ep.number > 100000
    }) {
        return Err(ApiError::InvalidInput);
    }
    Ok(())
}
fn string(v: &Value) -> Result<String, ApiError> {
    v.as_str()
        .filter(|s| !s.is_empty() && s.len() <= 4096)
        .map(str::to_owned)
        .ok_or(ApiError::InvalidResponse)
}
fn parse_tokens(body: Value) -> Result<Tokens, ApiError> {
    let mut body = body;
    let result = (|| {
        let scope = body["scope"].as_str().ok_or(ApiError::InvalidResponse)?;
        if !scope.split_whitespace().any(|s| s == "media:read")
            || !scope.split_whitespace().any(|s| s == "media:write")
        {
            return Err(ApiError::Authentication);
        }
        let token = Tokens {
            access_token: Secret::new(string(&body["access_token"])?)
                .map_err(|_| ApiError::InvalidResponse)?,
            refresh_token: Secret::new(string(&body["refresh_token"])?)
                .map_err(|_| ApiError::InvalidResponse)?,
            expires_in: body["expires_in"]
                .as_u64()
                .filter(|t| *t > 0)
                .ok_or(ApiError::InvalidResponse)?,
        };
        Ok(token)
    })();
    wipe_json(&mut body);
    result
}
fn request_ids(ids: &MediaIds, service: Service) -> Result<Value, ApiError> {
    if !ids.valid() {
        return Err(ApiError::InvalidInput);
    }
    let mut value = json!({});
    if let Some(v) = &ids.imdb {
        value["imdb"] = json!(v);
    }
    if let Some(v) = ids.tmdb {
        value["tmdb"] = json!(v);
    }
    if let Some(v) = ids.tvdb {
        value["tvdb"] = json!(v);
    }
    if service == Service::Simkl
        && let Some(v) = ids.simkl
    {
        value["simkl"] = json!(v);
    }
    if value.as_object().is_none_or(|o| o.is_empty()) {
        return Err(ApiError::Unsupported);
    }
    Ok(value)
}
fn grouped(kind: MediaType, item: Value) -> Value {
    let key = if kind == MediaType::Movie {
        "movies"
    } else {
        "shows"
    };
    json!({key:[item]})
}
fn contains_failure(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_u64() != Some(0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => o.values().any(contains_failure),
    }
}
fn mutation_receipt(value: Value) -> Result<(), ApiError> {
    // Both APIs report unresolved media inside a successful HTTP response.
    if ["not_found", "errors", "error"]
        .iter()
        .any(|k| value.get(k).is_some_and(contains_failure))
    {
        return Err(ApiError::Rejected);
    }
    if value.is_null() || !value.is_object() {
        return Err(ApiError::InvalidResponse);
    }
    Ok(())
}
fn parse_ids(value: &Value, flat: bool) -> Result<MediaIds, ApiError> {
    let ids = value.get("ids").unwrap_or(&Value::Null);
    let number = |v: &Value| {
        v.as_u64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            .filter(|n| *n > 0)
    };
    let result = MediaIds {
        imdb: ids
            .get("imdb")
            .or_else(|| ids.get("imdbid"))
            .or_else(|| value.get("imdb_id"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
        tmdb: number(&ids["tmdb"])
            .or_else(|| number(&ids["tmdbid"]))
            .or_else(|| if flat { number(&value["id"]) } else { None }),
        simkl: number(&ids["simkl"]),
        tvdb: number(&ids["tvdb"])
            .or_else(|| number(&ids["tvdbid"]))
            .or_else(|| number(&value["tvdb_id"])),
    };
    if !result.valid() {
        return Err(ApiError::InvalidResponse);
    }
    Ok(result)
}
fn parse_status(value: &Value, default: ListStatus) -> Result<ListStatus, ApiError> {
    Ok(match value.as_str() {
        None => default,
        Some("plantowatch") => ListStatus::Planning,
        Some("watching") => ListStatus::Watching,
        Some("completed") => ListStatus::Completed,
        Some("hold") => ListStatus::OnHold,
        Some("dropped" | "notinteresting") => ListStatus::Dropped,
        Some("removed") => ListStatus::Removed,
        _ => return Err(ApiError::InvalidResponse),
    })
}
fn episode(value: &Value, season: Option<u32>) -> Result<Episode, ApiError> {
    let s = season
        .or_else(|| value["season"].as_u64().and_then(|n| u32::try_from(n).ok()))
        .ok_or(ApiError::InvalidResponse)?;
    let n = value
        .get("number")
        .or_else(|| value.get("episode"))
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n > 0)
        .ok_or(ApiError::InvalidResponse)?;
    Ok(Episode {
        season: s,
        number: n,
    })
}
fn parse_title(
    row: &Value,
    kind: MediaType,
    default: ListStatus,
    simkl: bool,
) -> Result<RemoteTitle, ApiError> {
    let key = if kind == MediaType::Movie {
        "movie"
    } else {
        "show"
    };
    // MDBList watched episodes nest the parent under episode.show; the
    // episode's own TMDb/TVDB identifiers must never become a show identity.
    let media = row
        .get(key)
        .filter(|value| value.is_object())
        .or_else(|| row.get("anime").filter(|value| value.is_object()))
        .or_else(|| {
            (kind != MediaType::Movie)
                .then(|| row.pointer("/episode/show"))
                .flatten()
                .filter(|value| value.is_object())
        })
        .unwrap_or(row);
    let ids = parse_ids(media, !simkl && media.get("ids").is_none())?;
    let status = parse_status(&row["status"], default)?;
    let mut episodes = Vec::new();
    if let Some(seasons) = row
        .get("seasons")
        .filter(|_| simkl || default == ListStatus::Completed)
        .and_then(Value::as_array)
    {
        for season in seasons {
            let number = season["number"]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or(ApiError::InvalidResponse)?;
            if let Some(eps) = season["episodes"].as_array() {
                for ep in eps {
                    // Simkl rows in all-items are watched episode rows, while
                    // optional false flags from other sources remain unwatched.
                    if ep.get("watched").and_then(Value::as_bool) != Some(false) {
                        episodes.push(episode(ep, Some(number))?);
                    }
                }
            }
        }
    }
    if let Some(eps) = row
        .get("episodes")
        .filter(|_| simkl || default == ListStatus::Completed)
        .and_then(Value::as_array)
    {
        for ep in eps {
            if ep.get("watched").and_then(Value::as_bool) != Some(false) {
                episodes.push(episode(ep, Some(1))?);
            }
        }
    }
    if let Some(ep) = row.get("episode").filter(|e| e.is_object())
        && default == ListStatus::Completed
    {
        episodes.push(episode(ep, None)?);
    }
    let watched = kind == MediaType::Movie && status == ListStatus::Completed;
    let title = media
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    if title.len() > 4096 || episodes.len() > MAX_TITLES {
        return Err(ApiError::InvalidResponse);
    }
    Ok(RemoteTitle {
        ids,
        media_type: kind,
        title,
        year: media
            .get("year")
            .or_else(|| media.get("release_year"))
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok()),
        status,
        episodes,
        watched,
        progress: None,
        playback_episode: None,
    })
}
fn parse_titles(
    value: &Value,
    status: ListStatus,
    simkl: bool,
) -> Result<Vec<RemoteTitle>, ApiError> {
    if !value.is_object() {
        return Err(ApiError::InvalidResponse);
    }
    if !simkl
        && !["movies", "shows", "episodes", "seasons"]
            .iter()
            .any(|k| value.get(k).is_some())
    {
        return Err(ApiError::InvalidResponse);
    }
    let mut result = Vec::new();
    for (key, kind) in [
        ("movies", MediaType::Movie),
        ("shows", MediaType::Show),
        ("anime", MediaType::Anime),
        ("episodes", MediaType::Show),
    ] {
        if let Some(rows) = value.get(key).filter(|v| !v.is_null()) {
            for row in rows.as_array().ok_or(ApiError::InvalidResponse)? {
                let mut title = parse_title(row, kind, status, simkl)?;
                if key == "episodes" && status == ListStatus::Completed && title.episodes.is_empty()
                {
                    title.episodes.push(episode(row, None)?);
                }
                merge_titles(&mut result, vec![title]);
                if result.len() > MAX_TITLES {
                    return Err(ApiError::InvalidResponse);
                }
            }
        }
    }
    Ok(result)
}
fn same_title(left: &RemoteTitle, right: &RemoteTitle) -> bool {
    // IMDb/TMDb movie and show namespaces cannot be interchanged.
    (left.media_type == MediaType::Movie) == (right.media_type == MediaType::Movie)
        && left.ids.matches(&right.ids)
}
fn merge_titles(target: &mut Vec<RemoteTitle>, rows: Vec<RemoteTitle>) {
    for mut row in rows {
        if let Some(old) = target.iter_mut().find(|old| same_title(old, &row)) {
            if row.title.is_empty() {
                row.title = old.title.clone();
            }
            row.ids.imdb = row.ids.imdb.or_else(|| old.ids.imdb.clone());
            row.ids.tmdb = row.ids.tmdb.or(old.ids.tmdb);
            row.ids.simkl = row.ids.simkl.or(old.ids.simkl);
            row.ids.tvdb = row.ids.tvdb.or(old.ids.tvdb);
            // Episode rows from MDBList arrive separately from collection rows;
            // Simkl's complete per-title replacement is handled before this call.
            for ep in &old.episodes {
                if !row.episodes.contains(ep) {
                    row.episodes.push(*ep);
                }
            }
            *old = row;
        } else {
            target.push(row);
        }
    }
}
fn apply_playback(target: &mut Vec<RemoteTitle>, value: &Value) -> Result<(), ApiError> {
    let owned;
    let rows = if let Some(rows) = value.as_array() {
        rows
    } else if value.is_object() {
        owned = ["movies", "shows", "anime"]
            .iter()
            .filter_map(|k| value[k].as_array())
            .flat_map(|a| a.iter().cloned())
            .collect::<Vec<_>>();
        &owned
    } else {
        return Err(ApiError::InvalidResponse);
    };
    for row in rows {
        let progress = row["progress"]
            .as_f64()
            .filter(|p| p.is_finite() && (0.0..=100.0).contains(p))
            .ok_or(ApiError::InvalidResponse)?;
        let kind = if row.get("movie").is_some_and(|m| m.is_object()) {
            MediaType::Movie
        } else if row.get("anime").is_some_and(|a| a.is_object()) {
            MediaType::Anime
        } else {
            MediaType::Show
        };
        let mut title = parse_title(row, kind, ListStatus::Watching, false)?;
        title.progress = Some(progress);
        title.playback_episode = row
            .get("episode")
            .filter(|e| e.is_object())
            .map(|e| episode(e, None))
            .transpose()?;
        if let Some(old) = target.iter_mut().find(|old| same_title(old, &title)) {
            old.progress = title.progress;
            old.playback_episode = title.playback_episode;
        } else {
            target.push(title);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    #[derive(Default)]
    struct Fake {
        requests: Mutex<Vec<Request>>,
        responses: Mutex<VecDeque<Response>>,
    }
    impl Fake {
        fn push(&self, value: Value) {
            self.responses.lock().unwrap().push_back(Response {
                status: 200,
                retry_after: None,
                has_more: false,
                body: value,
            });
        }
    }
    impl Transport for Fake {
        fn send(&self, r: Request) -> Result<Response, ApiError> {
            self.requests.lock().unwrap().push(r);
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or(ApiError::Offline)
        }
    }
    fn credentials(service: Service) -> Credentials {
        Credentials {
            service,
            client_id: "nova-client".into(),
            token: Secret::new("private-token".into()).unwrap(),
        }
    }
    fn ids() -> MediaIds {
        MediaIds {
            imdb: Some("tt0903747".into()),
            ..Default::default()
        }
    }
    #[test]
    fn secrets_are_redacted_and_ids_reject_episode_strings() {
        let c = credentials(Service::Simkl);
        assert!(!format!("{c:?}").contains("private-token"));
        assert!(
            !MediaIds {
                imdb: Some("tt0903747:1:2".into()),
                ..Default::default()
            }
            .valid()
        );
        assert!(!MediaIds::default().valid());
    }
    #[test]
    fn history_targets_one_exact_episode_and_rejects_partial_success() {
        let fake = Arc::new(Fake::default());
        fake.push(json!({"not_found":{"shows":[{"ids":{"imdb":"tt0903747"}}]}}));
        let client = Client::new(fake.clone());
        assert_eq!(
            client.set_watched(
                &credentials(Service::MdbList),
                &ids(),
                MediaType::Show,
                Some(Episode {
                    season: 2,
                    number: 3
                }),
                true
            ),
            Err(ApiError::Rejected)
        );
        let requests = fake.requests.lock().unwrap();
        assert_eq!(
            requests[0].body.as_ref().unwrap()["shows"][0]["seasons"],
            json!([{"number":2,"episodes":[{"number":3}]}])
        );
        assert!(
            requests[0]
                .url
                .starts_with("https://api.mdblist.com/sync/watched?apikey=")
        );
        assert!(requests[0].bearer.is_none());
    }
    #[test]
    fn unsupported_status_never_sends_network_request() {
        let fake = Arc::new(Fake::default());
        let client = Client::new(fake.clone());
        assert_eq!(
            client.set_list_status(
                &credentials(Service::MdbList),
                &ids(),
                MediaType::Show,
                ListStatus::OnHold
            ),
            Err(ApiError::Unsupported)
        );
        assert!(fake.requests.lock().unwrap().is_empty());
    }
    #[test]
    fn simkl_status_is_per_item_and_auth_has_required_query() {
        let fake = Arc::new(Fake::default());
        fake.push(json!({"added":{"shows":1}}));
        Client::new(fake.clone())
            .set_list_status(
                &credentials(Service::Simkl),
                &ids(),
                MediaType::Show,
                ListStatus::Planning,
            )
            .unwrap();
        let requests = fake.requests.lock().unwrap();
        let r = &requests[0];
        assert_eq!(r.body.as_ref().unwrap()["shows"][0]["to"], "plantowatch");
        assert!(r.url.contains("app-name=nova"));
        assert!(r.url.contains("client_id=nova-client"));
        assert_eq!(r.bearer.as_ref().unwrap().expose(), "private-token");
    }
    #[test]
    fn pagination_collects_all_pages_and_detects_repeated_cursor() {
        let fake = Arc::new(Fake::default());
        let page = json!({"movies":[{"id":1,"title":"A","imdb_id":"tt000001"}],"pagination":{"next_cursor":"same","total":2}});
        fake.push(page.clone());
        fake.push(page);
        assert_eq!(
            Client::new(fake).mdb_pages(
                &credentials(Service::MdbList),
                "/watchlist/items",
                ListStatus::Planning
            ),
            Err(ApiError::InvalidResponse)
        );
    }
    #[test]
    fn simkl_unchanged_activities_skip_the_full_library() {
        let fake = Arc::new(Fake::default());
        fake.push(json!({"all":"2026-01-01T00:00:00Z"}));
        fake.push(json!({"shows":[]}));
        fake.push(json!({"movies":[]}));
        fake.push(json!({"anime":[]}));
        fake.push(json!([]));
        fake.push(json!({"all":"2026-01-01T00:00:00Z"}));
        let client = Client::new(fake.clone());
        let c = credentials(Service::Simkl);
        assert!(client.library(&c).unwrap().is_empty());
        assert!(client.library(&c).unwrap().is_empty());
        assert_eq!(fake.requests.lock().unwrap().len(), 6);
    }
    #[test]
    fn scrobble_payloads_differ_between_official_apis() {
        for service in [Service::Simkl, Service::MdbList] {
            let fake = Arc::new(Fake::default());
            fake.push(json!({"action":"pause"}));
            Client::new(fake.clone())
                .scrobble(
                    &credentials(service),
                    &ids(),
                    MediaType::Show,
                    Some(Episode {
                        season: 1,
                        number: 2,
                    }),
                    42.0,
                    ScrobbleAction::Pause,
                )
                .unwrap();
            let requests = fake.requests.lock().unwrap();
            let b = requests[0].body.as_ref().unwrap();
            if service == Service::Simkl {
                assert_eq!(b["episode"], json!({"season":1,"number":2}));
            } else {
                assert_eq!(b["show"]["episode"], 2);
                assert!(b.get("episode").is_none());
            }
        }
    }
    #[test]
    fn device_pending_and_rate_limits_are_explicit() {
        let r = Response {
            status: 400,
            retry_after: None,
            has_more: false,
            body: json!({"error":"authorization_pending"}),
        };
        assert!(matches!(checked(r), Err(ApiError::Pending)));
        let r = Response {
            status: 429,
            retry_after: Some(120),
            has_more: false,
            body: json!({}),
        };
        assert!(matches!(
            checked(r),
            Err(ApiError::RateLimited { retry_after: 120 })
        ));
    }
    #[test]
    fn exact_episode_history_survives_flat_mdblist_rows() {
        let rows=parse_titles(&json!({"episodes":[{"show":{"title":"A","ids":{"tmdb":20}},"episode":{"season":1,"number":3}}]}),ListStatus::Completed,false).unwrap();
        assert_eq!(
            rows[0].episodes,
            vec![Episode {
                season: 1,
                number: 3
            }]
        );
        assert!(!rows[0].watched);
    }
    #[test]
    fn mdblist_library_accepts_documented_watched_and_playback_shapes() {
        let fake = Arc::new(Fake::default());
        fake.push(json!({"movies": [], "shows": []}));
        fake.push(json!({"movies": [], "shows": []}));
        fake.push(json!({"episodes": [{
            "last_watched_at": "2025-10-15T20:00:00Z",
            "episode": {
                "season": 1, "number": 1, "ids": {"tmdb": 62085},
                "show": {"title": "Breaking Bad", "year": 2008,
                    "ids": {"tmdb": 1396, "imdb": "tt0903747"}}
            }
        }]}));
        fake.push(json!([
            {"type": "movie", "progress": 45.25,
                "movie": {"title": "The Shawshank Redemption", "year": 1994,
                    "ids": {"imdbid": "tt0111161", "tmdbid": 278}},
                "episode": null, "show": null},
            {"type": "episode", "progress": 68.5, "movie": null,
                "episode": {"season": 1, "number": 3,
                    "ids": {"tmdbid": 63174, "tvdbid": 349232}},
                "show": {"title": "Friends", "year": 1994,
                    "ids": {"imdbid": "tt0108778", "tmdbid": 1668, "tvdbid": 79168}}}
        ]));
        let rows = Client::new(fake.clone())
            .library(&credentials(Service::MdbList))
            .unwrap();
        assert_eq!(rows.len(), 3);
        let watched = rows.iter().find(|row| row.ids.tmdb == Some(1396)).unwrap();
        assert_eq!(watched.title, "Breaking Bad");
        assert_eq!(
            watched.episodes,
            vec![Episode {
                season: 1,
                number: 1
            }]
        );
        assert!(!watched.watched);
        let movie = rows.iter().find(|row| row.ids.tmdb == Some(278)).unwrap();
        assert_eq!(movie.media_type, MediaType::Movie);
        assert_eq!(movie.progress, Some(45.25));
        assert_eq!(movie.playback_episode, None);
        let show = rows.iter().find(|row| row.ids.tmdb == Some(1668)).unwrap();
        assert_eq!(show.ids.tvdb, Some(79168));
        assert_eq!(show.progress, Some(68.5));
        assert_eq!(
            show.playback_episode,
            Some(Episode {
                season: 1,
                number: 3
            })
        );
        assert!(
            show.episodes.is_empty(),
            "a resume point is not watched history"
        );
        assert!(
            rows.iter()
                .all(|row| row.ids.tmdb != Some(62085) && row.ids.tmdb != Some(63174))
        );
        assert_eq!(fake.requests.lock().unwrap().len(), 4);
    }
    #[test]
    fn mdblist_pagination_honors_body_has_more_without_cursor_or_header() {
        let fake = Arc::new(Fake::default());
        fake.push(json!({"movies": [{"id": 1, "title": "A"}],
            "pagination": {"has_more": true, "total_movies": 2}}));
        fake.push(json!({"movies": [{"id": 2, "title": "B"}],
            "pagination": {"has_more": false, "total_movies": 2}}));
        let rows = Client::new(fake.clone())
            .mdb_pages(
                &credentials(Service::MdbList),
                "/watchlist/items",
                ListStatus::Planning,
            )
            .unwrap();
        assert_eq!(rows.len(), 2);
        let requests = fake.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].url.contains("offset=1"));
    }
    #[test]
    fn collected_episodes_never_invent_watch_history() {
        let rows = parse_titles(&json!({"episodes":[{"show":{"title":"A","ids":{"tmdb":20}},"episode":{"season":1,"number":3}}]}), ListStatus::Planning, false).unwrap();
        assert!(rows[0].episodes.is_empty());
        assert!(!rows[0].watched);
    }
    #[test]
    fn simkl_unwatched_delta_replaces_previous_episode_snapshot() {
        let fake = Arc::new(Fake::default());
        fake.push(json!({"all":"2026-01-01T00:00:00Z"}));
        fake.push(json!({"shows":[{"show":{"title":"A","ids":{"simkl":20}},"status":"watching","seasons":[{"number":1,"episodes":[{"number":3}]}]}]}));
        fake.push(json!({"movies":[]}));
        fake.push(json!({"anime":[]}));
        fake.push(json!([]));
        fake.push(json!({"all":"2026-01-02T00:00:00Z"}));
        fake.push(json!({"shows":[{"show":{"title":"A","ids":{"simkl":20}},"status":"watching","seasons":[]}]}));
        fake.push(json!([]));
        let client = Client::new(fake.clone());
        let c = credentials(Service::Simkl);
        assert_eq!(client.library(&c).unwrap()[0].episodes.len(), 1);
        assert!(client.library(&c).unwrap()[0].episodes.is_empty());
        assert!(
            fake.requests.lock().unwrap()[6]
                .url
                .contains("date_from=2026-01-01")
        );
    }
    #[test]
    fn static_list_uses_flat_ids_while_collection_uses_nested_ids() {
        let fake = Arc::new(Fake::default());
        fake.push(json!({"added":1}));
        fake.push(json!({"updated":{"shows":1}}));
        let client = Client::new(fake.clone());
        let c = credentials(Service::MdbList);
        client
            .set_list_item(&c, 10, &ids(), MediaType::Show, true)
            .unwrap();
        client
            .set_collection(&c, &ids(), MediaType::Show, true)
            .unwrap();
        let r = fake.requests.lock().unwrap();
        assert_eq!(r[0].body.as_ref().unwrap()["shows"][0]["imdb"], "tt0903747");
        assert_eq!(
            r[1].body.as_ref().unwrap()["shows"][0]["ids"]["imdb"],
            "tt0903747"
        );
    }
    #[test]
    fn readonly_device_grant_and_untrusted_browser_url_are_rejected() {
        assert!(matches!(
            parse_tokens(
                json!({"access_token":"access","refresh_token":"refresh","expires_in":600,"scope":"media:read"})
            ),
            Err(ApiError::Authentication)
        ));
        assert!(validate_verification_uri("https://user:pass@simkl.com/pin").is_err());
        assert!(validate_verification_uri("https://simkl.com.attacker.example/pin").is_err());
    }
    #[test]
    fn conflicting_shared_identifiers_do_not_merge_titles() {
        let left = MediaIds {
            imdb: Some("tt0903747".into()),
            tmdb: Some(1),
            ..Default::default()
        };
        let right = MediaIds {
            imdb: Some("tt0903747".into()),
            tmdb: Some(2),
            ..Default::default()
        };
        assert!(!left.matches(&right));
    }
    #[test]
    fn restored_checkpoint_checks_only_activities_when_unchanged() {
        let fake = Arc::new(Fake::default());
        fake.push(json!({"all":"2026-01-01T00:00:00Z"}));
        let client = Client::new(fake.clone());
        let c = credentials(Service::Simkl);
        client
            .restore_simkl_checkpoint(
                &c,
                SimklCheckpoint {
                    activities: json!({"all":"2026-01-01T00:00:00Z"}),
                    titles: vec![],
                },
            )
            .unwrap();
        assert!(client.library(&c).unwrap().is_empty());
        assert_eq!(fake.requests.lock().unwrap().len(), 1);
        assert!(client.simkl_checkpoint(&c).is_some());
    }
    #[test]
    fn collected_nested_show_episodes_are_membership_only() {
        let rows = parse_titles(&json!({"shows":[{"show":{"title":"A","ids":{"tmdb":20}},"seasons":[{"number":1,"episodes":[{"number":3}]}]}]}), ListStatus::Planning, false).unwrap();
        assert!(rows[0].episodes.is_empty());
    }
}
