//! Tracker contracts shared by the adapters and delivery coordinator.
//! Credentials are deliberately absent from every serializable model.
use crate::{AccountKey, Service};
use serde::{Deserialize, Serialize};
use std::num::NonZeroU32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListStatus {
    Planning,
    Watching,
    Completed,
    OnHold,
    Dropped,
    Repeating,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScoreFormat {
    Point100,
    Point10Decimal,
    Point10,
    Point5,
    Point3,
}
impl ScoreFormat {
    pub fn maximum(self) -> u32 {
        match self {
            Self::Point100 => 1000,
            Self::Point10Decimal | Self::Point10 => 100,
            Self::Point5 => 50,
            Self::Point3 => 30,
        }
    }
    pub fn valid(self, tenths: u32) -> bool {
        tenths <= self.maximum() && (self == Self::Point10Decimal || tenths.is_multiple_of(10))
    }
    pub(crate) fn anilist(self) -> &'static str {
        match self {
            Self::Point100 => "POINT_100",
            Self::Point10Decimal => "POINT_10_DECIMAL",
            Self::Point10 => "POINT_10",
            Self::Point5 => "POINT_5",
            Self::Point3 => "POINT_3",
        }
    }
}
/// Fuzzy dates retain unknown components. No timezone conversion is involved.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListDate {
    pub year: Option<u16>,
    pub month: Option<u8>,
    pub day: Option<u8>,
}
impl ListDate {
    pub fn valid(self) -> bool {
        if self.year.is_some_and(|v| v == 0 || v > 9999)
            || self.month.is_some_and(|v| !(1..=12).contains(&v))
        {
            return false;
        }
        let max = match self.month {
            Some(4 | 6 | 9 | 11) => 30,
            Some(2) => {
                if self.year.is_none_or(|y| {
                    y.is_multiple_of(400) || (y.is_multiple_of(4) && !y.is_multiple_of(100))
                }) {
                    29
                } else {
                    28
                }
            }
            _ => 31,
        };
        self.day.is_none_or(|v| (1..=max).contains(&v))
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Media {
    pub id: NonZeroU32,
    pub mal_id: Option<NonZeroU32>,
    pub title: String,
    pub format: String,
    pub episodes: Option<NonZeroU32>,
    pub finished: bool,
    pub year: Option<u16>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteEntry {
    pub account: AccountKey,
    pub media_id: NonZeroU32,
    pub entry_id: Option<NonZeroU32>,
    pub progress: u32,
    pub status: ListStatus,
    pub score_tenths: u32,
    pub started: ListDate,
    pub completed: ListDate,
    pub repeating: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Viewer {
    pub account: AccountKey,
    pub name: String,
    pub score_format: ScoreFormat,
}
/// None means omit the field, while Some(empty date) explicitly clears it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryPatch {
    pub progress: Option<u32>,
    pub status: Option<ListStatus>,
    pub score_tenths: Option<u32>,
    pub started: Option<ListDate>,
    pub completed: Option<ListDate>,
}
impl EntryPatch {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
    pub fn validate(&self, service: Service, score: ScoreFormat) -> Result<(), ApiError> {
        if self.is_empty()
            || self.progress.is_some_and(|v| v > i32::MAX as u32)
            || self.score_tenths.is_some_and(|v| !score.valid(v))
            || self.started.is_some_and(|v| !v.valid())
            || self.completed.is_some_and(|v| !v.valid())
        {
            return Err(ApiError::InvalidInput);
        }
        if service == Service::MyAnimeList
            && (self.started.is_some()
                || self.completed.is_some()
                || self.status == Some(ListStatus::Repeating))
        {
            return Err(ApiError::UnsupportedField);
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiError {
    Offline,
    Authentication,
    RateLimited { retry_at: u64 },
    Rejected,
    InvalidResponse,
    InvalidInput,
    UnsupportedField,
    WrongAccount,
}
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Offline => "tracker unavailable",
            Self::Authentication => "tracker sign-in required",
            Self::RateLimited { .. } => "tracker rate limit",
            Self::Rejected => "tracker rejected the update",
            Self::InvalidResponse => "invalid tracker response",
            Self::InvalidInput => "invalid tracker input",
            Self::UnsupportedField => "field unsupported by this tracker",
            Self::WrongAccount => "tracker account changed",
        })
    }
}
impl std::error::Error for ApiError {}
/// Redacted, zeroized, session-only secret. No Serialize implementation.
pub struct Secret(zeroize::Zeroizing<String>);
impl Secret {
    pub fn new(value: String) -> Result<Self, ApiError> {
        if value.is_empty()
            || value.len() > 16384
            || value
                .bytes()
                .any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
        {
            return Err(ApiError::InvalidInput);
        }
        Ok(Self(zeroize::Zeroizing::new(value)))
    }
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret([redacted])")
    }
}
