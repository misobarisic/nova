use serde::{Deserialize, Serialize};
use std::fmt;
use zeroize::Zeroize;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum Service {
    Simkl,
    MdbList,
}

#[derive(Clone, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Secret(String);
impl Secret {
    pub fn new(value: String) -> Result<Self, ApiError> {
        if value.is_empty() || value.len() > 8192 || value.chars().any(char::is_control) {
            return Err(ApiError::InvalidInput);
        }
        Ok(Self(value))
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}
impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Credentials {
    pub service: Service,
    /// A registered public Simkl app client ID; empty for MDBList API keys.
    pub client_id: String,
    pub token: Secret,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub name: String,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum MediaType {
    Movie,
    Show,
    Anime,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ListStatus {
    Planning,
    Watching,
    Completed,
    OnHold,
    Dropped,
    Removed,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScrobbleAction {
    Start,
    Pause,
    Stop,
}
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct MediaIds {
    pub imdb: Option<String>,
    pub tmdb: Option<u64>,
    pub simkl: Option<u64>,
    pub tvdb: Option<u64>,
}
impl MediaIds {
    pub fn valid(&self) -> bool {
        let imdb_valid = self.imdb.as_ref().is_none_or(|id| {
            id.len() >= 4
                && id.len() <= 20
                && id.starts_with("tt")
                && id[2..].bytes().all(|b| b.is_ascii_digit())
        });
        imdb_valid
            && [self.tmdb, self.simkl, self.tvdb]
                .iter()
                .all(|v| *v != Some(0))
            && (self.imdb.is_some()
                || self.tmdb.is_some()
                || self.simkl.is_some()
                || self.tvdb.is_some())
    }
    pub fn matches(&self, other: &Self) -> bool {
        let pairs = [
            (self.tmdb, other.tmdb),
            (self.simkl, other.simkl),
            (self.tvdb, other.tvdb),
        ];
        if pairs
            .iter()
            .any(|(a, b)| a.is_some() && b.is_some() && a != b)
            || (self.imdb.is_some() && other.imdb.is_some() && self.imdb != other.imdb)
        {
            return false;
        }
        (self.imdb.is_some() && self.imdb == other.imdb)
            || pairs.iter().any(|(a, b)| a.is_some() && a == b)
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct Episode {
    pub season: u32,
    pub number: u32,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RemoteTitle {
    pub ids: MediaIds,
    pub media_type: MediaType,
    pub title: String,
    pub year: Option<u32>,
    pub status: ListStatus,
    /// Exact watched episodes only; counts are never expanded to guessed episodes.
    pub episodes: Vec<Episode>,
    pub watched: bool,
    /// Percentage, never a timestamp or guessed duration.
    pub progress: Option<f64>,
    pub playback_episode: Option<Episode>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RemoteList {
    pub id: u64,
    pub name: String,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApiError {
    InvalidInput,
    InvalidResponse,
    Authentication,
    Offline,
    Rejected,
    Unsupported,
    RateLimited { retry_after: u64 },
    Pending,
    SlowDown,
    Expired,
}
impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidInput => "invalid service input",
            Self::InvalidResponse => "invalid service response",
            Self::Authentication => "service authentication failed",
            Self::Offline => "service unavailable",
            Self::Rejected => "service rejected the request",
            Self::Unsupported => "operation unsupported by this service",
            Self::RateLimited { .. } => "service rate limit reached",
            Self::Pending => "authorization pending",
            Self::SlowDown => "authorization polling must slow down",
            Self::Expired => "authorization expired",
        })
    }
}
impl std::error::Error for ApiError {}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceCode {
    pub device_code: Secret,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
    pub interval: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: Secret,
    pub refresh_token: Secret,
    pub expires_in: u64,
}

/// Device-local Simkl sync baseline. Never transfer it between different accounts.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SimklCheckpoint {
    pub activities: serde_json::Value,
    pub titles: Vec<RemoteTitle>,
}
