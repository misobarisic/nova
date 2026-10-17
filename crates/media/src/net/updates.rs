//! Small, bounded GitHub request independent of addon enrichment and retries.
use std::io::Read;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const ENDPOINT: &str = "https://api.github.com/repos/misobarisic/nova/releases/latest";
const LIMIT: u64 = 1024 * 1024;

pub enum Response {
    Unchanged,
    Release { body: Vec<u8>, etag: Option<String> },
}

#[derive(Debug)]
pub struct Error {
    pub message: String,
    /// A GitHub cooldown applies to manual checks too.
    pub retry_at: Option<u64>,
}

impl From<reqwest::Error> for Error {
    fn from(error: reqwest::Error) -> Self {
        Self {
            message: error.to_string(),
            retry_at: None,
        }
    }
}

/// Runs on the caller's worker. No tokens, telemetry, downloads or installation.
pub fn fetch_latest(etag: Option<&str>, version: &str) -> Result<Response, Error> {
    let builder = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent(format!("Nova/{version}"))
        .redirect(reqwest::redirect::Policy::none());
    #[cfg(target_os = "android")]
    let builder =
        builder.tls_backend_preconfigured(super::imp::tls_config().map_err(|message| Error {
            message,
            retry_at: None,
        })?);
    let client = builder.build()?;
    let mut request = client
        .get(ENDPOINT)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2026-03-10");
    if let Some(etag) = etag {
        request = request.header("If-None-Match", etag);
    }
    let response = request.send()?;
    if response.status() == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(Response::Unchanged);
    }
    if response.status() == reqwest::StatusCode::FORBIDDEN
        || response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS
    {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let header = |name| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
        };
        let retry_at = cooldown(now, header("retry-after"), header("x-ratelimit-reset"));
        return Err(Error {
            message: format!("GitHub returned {}", response.status()),
            retry_at: Some(retry_at),
        });
    }
    // Treat redirects and unexpected statuses as failures, not a clean check.
    if response.status() != reqwest::StatusCode::OK {
        return Err(Error {
            message: format!("GitHub returned {}", response.status()),
            retry_at: None,
        });
    }
    let etag = response
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .filter(|v| v.len() <= 512)
        .map(str::to_owned);
    let mut body = Vec::new();
    response
        .take(LIMIT + 1)
        .read_to_end(&mut body)
        .map_err(|e| Error {
            message: e.to_string(),
            retry_at: None,
        })?;
    if body.len() as u64 > LIMIT {
        return Err(Error {
            message: "release response exceeds byte limit".into(),
            retry_at: None,
        });
    }
    Ok(Response::Release { body, etag })
}

fn cooldown(now: u64, retry_after: Option<u64>, reset: Option<u64>) -> u64 {
    now.saturating_add(60)
        .max(now.saturating_add(retry_after.unwrap_or(0)))
        .max(reset.unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cooldown_respects_both_headers_and_has_a_minimum() {
        assert_eq!(cooldown(100, None, None), 160);
        assert_eq!(cooldown(100, Some(90), Some(300)), 300);
        assert_eq!(cooldown(100, Some(400), Some(300)), 500);
        assert_eq!(cooldown(100, None, Some(50)), 160);
    }
}
