use crate::{ApiError, LibraryItem, Secret};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{io::Read, time::Duration};
use zeroize::{Zeroize, Zeroizing};

pub const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
pub struct Request {
    pub method: &'static str,
    pub url: String,
    pub body: Zeroizing<Vec<u8>>,
}
impl Drop for Request {
    fn drop(&mut self) {
        self.url.zeroize();
    }
}
pub struct Response {
    pub status: u16,
    pub retry_after: Option<u64>,
    pub body: Zeroizing<Vec<u8>>,
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
            .user_agent("Nova/0.1 Stremio sync");
        // cargo-apk2 does not package the platform verifier's Kotlin/JNI runtime.
        #[cfg(target_os = "android")]
        let builder = {
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let tls = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
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
fn validate_request(request: &Request) -> Result<url::Url, ApiError> {
    let url = url::Url::parse(&request.url).map_err(|_| ApiError::InvalidInput)?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.fragment().is_some()
    {
        return Err(ApiError::InvalidInput);
    }
    let valid = match (url.host_str(), request.method, url.path()) {
        (
            Some("api.strem.io"),
            "POST",
            "/api/getUser"
            | "/api/addonCollectionGet"
            | "/api/datastoreMeta"
            | "/api/datastoreGet"
            | "/api/datastorePut",
        ) => url.query().is_none(),
        (Some("link.stremio.com"), "GET", "/api/v2/create" | "/api/v2/read") => {
            request.body.is_empty()
        }
        _ => false,
    };
    if !valid || request.body.len() > MAX_RESPONSE_BYTES {
        return Err(ApiError::InvalidInput);
    }
    Ok(url)
}
impl Transport for HttpsTransport {
    fn send(&self, request: Request) -> Result<Response, ApiError> {
        let url = validate_request(&request)?;
        let mut builder = self.client.request(
            reqwest::Method::from_bytes(request.method.as_bytes())
                .map_err(|_| ApiError::InvalidInput)?,
            url,
        );
        if !request.body.is_empty() {
            builder = builder
                .header("content-type", "application/json")
                .body(request.body.to_vec());
        }
        let mut response = builder.send().map_err(|error| {
            if error.is_timeout() {
                ApiError::Timeout
            } else {
                ApiError::Offline
            }
        })?;
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok());
        let mut bytes = Zeroizing::new(Vec::new());
        response
            .by_ref()
            .take(MAX_RESPONSE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::TimedOut
                    || error
                        .get_ref()
                        .and_then(|error| error.downcast_ref::<reqwest::Error>())
                        .is_some_and(reqwest::Error::is_timeout)
                {
                    ApiError::Timeout
                } else {
                    ApiError::Offline
                }
            })?;
        if bytes.len() > MAX_RESPONSE_BYTES {
            return Err(ApiError::InvalidResponse);
        }
        Ok(Response {
            status,
            retry_after,
            body: bytes,
        })
    }
}
// serde_json values may contain auth keys in both request and response envelopes.
fn wipe(value: &mut Value) {
    match value {
        Value::String(s) => s.zeroize(),
        Value::Array(v) => v.iter_mut().for_each(wipe),
        Value::Object(v) => v.values_mut().for_each(wipe),
        _ => {}
    }
}
struct Json(Value);
impl Drop for Json {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct User {
    #[serde(rename = "_id", alias = "id")]
    pub id: String,
    #[serde(default)]
    pub email: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct LinkCode {
    pub code: Secret,
    pub link: String,
}
impl Drop for LinkCode {
    fn drop(&mut self) {
        self.link.zeroize();
    }
}
impl std::fmt::Debug for LinkCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LinkCode(<redacted>)")
    }
}
pub struct Client<T: Transport> {
    transport: T,
}
impl<T: Transport> Client<T> {
    pub fn new(transport: T) -> Self {
        Self { transport }
    }
    fn request(
        &self,
        method: &'static str,
        url: String,
        body: Value,
        now: u64,
    ) -> Result<Json, ApiError> {
        let body = Json(body);
        let bytes = if method == "GET" {
            Vec::new()
        } else {
            serde_json::to_vec(&body.0).map_err(|_| ApiError::InvalidInput)?
        };
        let request = Request {
            method,
            url,
            body: Zeroizing::new(bytes),
        };
        validate_request(&request)?;
        let response = self.transport.send(request)?;
        match response.status {
            200..=299 => {}
            401 | 403 => return Err(ApiError::Authentication),
            429 => {
                return Err(ApiError::RateLimited {
                    retry_at: now
                        .saturating_add(response.retry_after.unwrap_or(60).clamp(1, 86400)),
                });
            }
            status => return Err(ApiError::Http { status }),
        }
        if response.body.len() > MAX_RESPONSE_BYTES {
            return Err(ApiError::InvalidResponse);
        }
        let mut value =
            Json(serde_json::from_slice(&response.body).map_err(|_| ApiError::InvalidResponse)?);
        if let Some(error) = value.0.get("error").filter(|error| !error.is_null()) {
            return Err(match error.get("code").and_then(Value::as_u64) {
                Some(1 | 2) => ApiError::Authentication,
                code => ApiError::RemoteRejected { code },
            });
        }
        let result = value
            .0
            .get_mut("result")
            .map(Value::take)
            .ok_or(ApiError::InvalidResponse)?;
        Ok(Json(result))
    }
    fn post(&self, path: &str, key: &Secret, mut body: Value, now: u64) -> Result<Json, ApiError> {
        if key.expose().is_empty() || key.expose().len() > 8192 {
            return Err(ApiError::InvalidInput);
        }
        body["authKey"] = json!(key.expose());
        self.request(
            "POST",
            format!("https://api.strem.io/api/{path}"),
            body,
            now,
        )
    }
    pub fn get_user(&self, key: &Secret, now: u64) -> Result<User, ApiError> {
        let value = self.post("getUser", key, json!({"type":"GetUser"}), now)?;
        let user: User =
            serde_json::from_value(value.0.clone()).map_err(|_| ApiError::InvalidResponse)?;
        if user.id.is_empty() || user.id.len() > 4096 || user.email.len() > 4096 {
            return Err(ApiError::InvalidResponse);
        }
        Ok(user)
    }
    pub fn addon_collection(&self, key: &Secret, now: u64) -> Result<Vec<Value>, ApiError> {
        let value = self.post(
            "addonCollectionGet",
            key,
            json!({"type":"AddonCollectionGet","update":false}),
            now,
        )?;
        value
            .0
            .get("addons")
            .and_then(Value::as_array)
            .cloned()
            .ok_or(ApiError::InvalidResponse)
    }
    pub fn library_meta(&self, key: &Secret, now: u64) -> Result<Vec<(String, u64)>, ApiError> {
        let value = self.post(
            "datastoreMeta",
            key,
            json!({"collection":"libraryItem"}),
            now,
        )?;
        serde_json::from_value(value.0.clone()).map_err(|_| ApiError::InvalidResponse)
    }
    pub fn library_get(
        &self,
        key: &Secret,
        ids: &[String],
        all: bool,
        now: u64,
    ) -> Result<Vec<LibraryItem>, ApiError> {
        if ids.len() > 100_000 || ids.iter().any(|id| id.is_empty() || id.len() > 4096) {
            return Err(ApiError::InvalidInput);
        }
        let value = self.post(
            "datastoreGet",
            key,
            json!({"collection":"libraryItem","ids":ids,"all":all}),
            now,
        )?;
        let items: Vec<LibraryItem> =
            serde_json::from_value(value.0.clone()).map_err(|_| ApiError::InvalidResponse)?;
        if items.len() > 100_000
            || items
                .iter()
                .any(|i| i.id().is_empty() || i.id().len() > 4096)
        {
            return Err(ApiError::InvalidResponse);
        }
        Ok(items)
    }
    pub fn library_put(
        &self,
        key: &Secret,
        changes: &[LibraryItem],
        now: u64,
    ) -> Result<(), ApiError> {
        if changes.len() > 100_000
            || changes
                .iter()
                .any(|i| i.id().is_empty() || i.id().len() > 4096)
        {
            return Err(ApiError::InvalidInput);
        }
        let value = self.post(
            "datastorePut",
            key,
            json!({"collection":"libraryItem","changes":changes}),
            now,
        )?;
        if value.0.get("success").and_then(Value::as_bool) != Some(true) {
            return Err(ApiError::RemoteRejected { code: None });
        }
        Ok(())
    }
    pub fn create_link(&self, now: u64) -> Result<LinkCode, ApiError> {
        let value = self.request(
            "GET",
            "https://link.stremio.com/api/v2/create?type=Create".into(),
            Value::Null,
            now,
        )?;
        let code = value
            .0
            .get("code")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && s.len() <= 4096)
            .ok_or(ApiError::InvalidResponse)?;
        let link = value
            .0
            .get("link")
            .and_then(Value::as_str)
            .ok_or(ApiError::InvalidResponse)?;
        let parsed = url::Url::parse(link).map_err(|_| ApiError::InvalidResponse)?;
        if parsed.scheme() != "https"
            || !matches!(
                parsed.host_str(),
                Some("link.stremio.com" | "www.stremio.com" | "stremio.com")
            )
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.port().is_some()
        {
            return Err(ApiError::InvalidResponse);
        }
        Ok(LinkCode {
            code: Secret::new(code),
            link: link.into(),
        })
    }
    pub fn read_link(&self, code: &Secret, now: u64) -> Result<Option<Secret>, ApiError> {
        if code.expose().is_empty() || code.expose().len() > 4096 {
            return Err(ApiError::InvalidInput);
        }
        let mut url = url::Url::parse("https://link.stremio.com/api/v2/read?type=Read").unwrap();
        url.query_pairs_mut().append_pair("code", code.expose());
        let value = self.request("GET", url.into(), Value::Null, now)?;
        if value.0.is_null() {
            return Ok(None);
        }
        value
            .0
            .get("authKey")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && s.len() <= 8192)
            .map(|s| Some(Secret::new(s)))
            .ok_or(ApiError::InvalidResponse)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credentials_never_leave_official_paths() {
        for url in [
            "http://api.strem.io/api/getUser",
            "https://api.strem.io.evil.test/api/getUser",
            "https://user@api.strem.io/api/getUser",
            "https://api.strem.io:444/api/getUser",
            "https://api.strem.io/api/getUser?redirect=x",
            "https://api.strem.io/other",
            "https://link.stremio.com/api/getUser",
        ] {
            let request = Request {
                method: "POST",
                url: url.into(),
                body: Zeroizing::new(b"secret".to_vec()),
            };
            assert_eq!(validate_request(&request), Err(ApiError::InvalidInput));
        }
    }
    #[test]
    fn mocked_requests_match_official_wire_contract() {
        struct Verify;
        impl Transport for Verify {
            fn send(&self, request: Request) -> Result<Response, ApiError> {
                assert_eq!(request.method, "POST");
                assert_eq!(request.url, "https://api.strem.io/api/datastoreGet");
                assert_eq!(
                    serde_json::from_slice::<Value>(&request.body).unwrap(),
                    json!({"authKey":"private","collection":"libraryItem","ids":["tt1"],"all":false})
                );
                Ok(Response {
                    status: 200,
                    retry_after: None,
                    body: Zeroizing::new(br#"{"result":[]}"#.to_vec()),
                })
            }
        }
        Client::new(Verify)
            .library_get(&Secret::new("private"), &["tt1".into()], false, 0)
            .unwrap();
    }
}
