use std::num::{NonZeroU32, NonZeroU64};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Service {
    MyAnimeList,
    AniList,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Anime,
}

/// Account identity is verified by the service, never inferred from a name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountKey {
    pub service: Service,
    pub remote_user_id: NonZeroU32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub key: AccountKey,
    /// Reconnecting or replacing credentials invalidates older queued work.
    pub generation: NonZeroU64,
    pub display_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetKey {
    pub account: AccountKey,
    pub media_kind: MediaKind,
    pub remote_media_id: NonZeroU32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRef {
    pub provider_id: String,
    pub source_id: String,
    pub media_type: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceEpisode {
    pub source: SourceRef,
    pub episode_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assignment {
    pub episode_id: String,
    pub target_episode: NonZeroU32,
}

/// Explicit assignments use stable episode IDs, not displayed numbering.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub id: String,
    pub source: SourceRef,
    pub target: TargetKey,
    pub account_generation: NonZeroU64,
    pub mapping_revision: NonZeroU64,
    pub enabled: bool,
    pub assignments: Vec<Assignment>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub key: TargetKey,
    /// AniList list-entry IDs are separate from catalog media IDs.
    pub remote_entry_id: Option<NonZeroU32>,
    pub final_episode_total: Option<NonZeroU32>,
    /// Unknown/ongoing releases cannot complete from a cached source count.
    pub release_finished: bool,
}

/// One atomic, local-only record. Credentials and resolver caches are excluded.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackingState {
    pub accounts: Vec<Account>,
    pub targets: Vec<Target>,
    pub bindings: Vec<Binding>,
    #[serde(default)]
    pub projections: Vec<crate::Projection>,
}
