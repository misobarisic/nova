use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc};

use serde::{Deserialize, Serialize};

use crate::ProviderHost;

pub type ProviderFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ProviderError>> + Send + 'a>>;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderDescriptor {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    #[serde(default)]
    pub media_types: Vec<String>,
    #[serde(default)]
    pub catalogs: Vec<ProviderCatalog>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderCatalog {
    pub id: String,
    pub name: String,
    pub media_type: String,
    #[serde(default)]
    pub supports_search: bool,
    #[serde(default)]
    pub supports_pagination: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExternalIds {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imdb: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmdb: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anilist: Option<String>,
    /// All normalized claims, including contradictory values. Legacy fields
    /// remain for provider compatibility; tracker resolution uses these IDs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub typed: Vec<crate::ExternalId>,
    #[serde(default)]
    pub other: BTreeMap<String, String>,
}

/// The provider ID and source ID stay separate so a site's slug can never be
/// mistaken for a Stremio or another provider's canonical identifier.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MediaItem {
    pub provider_id: String,
    pub source_id: String,
    pub media_type: String,
    pub title: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poster: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    /// Transparent title artwork supplied by the metadata source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(default)]
    pub external_ids: ExternalIds,
}

impl MediaItem {
    pub fn stable_id(&self) -> String {
        if self.provider_id.is_empty() {
            self.source_id.clone()
        } else {
            format!("{}:{}", self.provider_id, self.source_id)
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Episode {
    pub provider_id: String,
    pub source_id: String,
    pub parent_id: String,
    pub number: u32,
    #[serde(default)]
    pub season: u32,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub released: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<String>,
}

impl Episode {
    pub fn stable_id(&self) -> String {
        if self.provider_id.is_empty() {
            self.source_id.clone()
        } else {
            format!("{}:{}", self.provider_id, self.source_id)
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderStream {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub info_hash: Option<String>,
    #[serde(default)]
    pub file_idx: Option<u32>,
    #[serde(default)]
    pub resolver: Option<ProviderStreamResolver>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub seeders: Option<u32>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub subtitles: Vec<ProviderSubtitle>,
}

/// An opaque deferred playback token can only be returned to its owning provider.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderStreamResolver {
    pub provider_id: String,
    pub token: String,
}

impl ProviderStream {
    pub fn validate(&self, provider_id: &str) -> Result<(), ProviderError> {
        let invalid = || ProviderError::InvalidData("invalid provider stream target".into());
        if usize::from(!self.url.is_empty())
            + usize::from(self.info_hash.is_some())
            + usize::from(self.resolver.is_some())
            != 1
        {
            return Err(invalid());
        }
        if !self.url.is_empty() {
            validate_playback_url(&self.url)?;
        }
        for subtitle in &self.subtitles {
            validate_playback_url(&subtitle.url)?;
        }
        if self.headers.iter().any(|(name, value)| {
            name.is_empty()
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
                || value.chars().any(char::is_control)
        }) {
            return Err(invalid());
        }
        if self
            .info_hash
            .as_ref()
            .is_some_and(|hash| hash.len() != 40 || !hash.bytes().all(|b| b.is_ascii_hexdigit()))
            || (self.file_idx.is_some() && self.info_hash.is_none())
        {
            return Err(invalid());
        }
        if let Some(resolver) = &self.resolver
            && (resolver.provider_id != provider_id
                || resolver.token.is_empty()
                || resolver.token.len() > 8192)
        {
            return Err(invalid());
        }
        Ok(())
    }
}

fn validate_playback_url(raw: &str) -> Result<(), ProviderError> {
    let invalid = || ProviderError::InvalidData("invalid provider playback URL".into());
    if raw.chars().any(char::is_control) || raw.len() > 16_384 {
        return Err(invalid());
    }
    let url = url::Url::parse(raw).map_err(|_| invalid())?;
    if !matches!(url.scheme(), "https" | "http")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(invalid());
    }
    let public = match url.host() {
        Some(url::Host::Ipv4(ip)) => crate::host::is_public_ip(ip.into()),
        Some(url::Host::Ipv6(ip)) => crate::host::is_public_ip(ip.into()),
        Some(url::Host::Domain(host)) => {
            let host = host.trim_end_matches('.').to_ascii_lowercase();
            !host.is_empty()
                && host.contains('.')
                && !["localhost", "local", "internal", "home", "lan"]
                    .iter()
                    .any(|suffix| host == *suffix || host.ends_with(&format!(".{suffix}")))
        }
        None => false,
    };
    if !public {
        return Err(invalid());
    }
    Ok(())
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderSubtitle {
    pub url: String,
    #[serde(default)]
    pub language: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct CatalogRequest {
    pub catalog_id: String,
    pub media_type: String,
    pub skip: u32,
    pub genre: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct SearchRequest {
    pub media_type: String,
    pub query: String,
    pub skip: u32,
}

#[derive(Clone, Debug, Default)]
pub struct MediaRequest {
    pub media_type: String,
    pub source_id: String,
}

#[derive(Clone, Debug, Default)]
pub struct StreamRequest {
    pub media_type: String,
    pub episode_id: String,
}

/// Metadata supplied by another catalog for a source stream lookup. The
/// original media ID remains the app's library/progress identity.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StreamLookupRequest {
    pub media_id: String,
    pub media_type: String,
    pub title: String,
    pub year: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub season: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode: Option<u32>,
    pub absolute_episode: Option<u32>,
    /// Counts include regular numbered episodes only. Missing or incomplete
    /// metadata leaves them unknown, rather than inventing season boundaries.
    #[serde(default)]
    pub season_episode_count: Option<u32>,
    #[serde(default)]
    pub series_episode_count: Option<u32>,
    pub episode_title: Option<String>,
    pub released: Option<String>,
}

/// A bounded user-facing resolver failure, separate from internal host errors.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderResolveError {
    pub code: String,
    pub message: String,
}
impl ProviderResolveError {
    pub fn generic() -> Self {
        Self {
            code: "provider_error".into(),
            message: "Could not resolve this stream.".into(),
        }
    }
    pub fn cancelled() -> Self {
        Self {
            code: "cancelled".into(),
            message: "Provider operation cancelled.".into(),
        }
    }
    pub fn validate(&self) -> bool {
        !self.code.is_empty()
            && self.code.len() <= 64
            && self
                .code
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            && !self.message.trim().is_empty()
            && self.message.len() <= 256
            && !self.message.chars().any(char::is_control)
    }
}
impl std::fmt::Display for ProviderResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for ProviderResolveError {}
impl From<ProviderError> for ProviderResolveError {
    fn from(_: ProviderError) -> Self {
        Self::generic()
    }
}

#[derive(Clone, Debug)]
pub enum ProviderError {
    Unsupported(String),
    InvalidData(String),
    Host(String),
    Script(String),
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported(message)
            | Self::InvalidData(message)
            | Self::Host(message)
            | Self::Script(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ProviderError {}

/// The app calls providers on background workers. Boxed futures keep the
/// contract object-safe while allowing network-backed providers to be async.
pub trait ContentProvider: Send + Sync {
    fn descriptor(&self) -> &ProviderDescriptor;

    fn browse<'a>(
        &'a self,
        host: Arc<dyn ProviderHost>,
        request: CatalogRequest,
    ) -> ProviderFuture<'a, Vec<MediaItem>>;

    fn search<'a>(
        &'a self,
        host: Arc<dyn ProviderHost>,
        request: SearchRequest,
    ) -> ProviderFuture<'a, Vec<MediaItem>>;

    fn details<'a>(
        &'a self,
        host: Arc<dyn ProviderHost>,
        request: MediaRequest,
    ) -> ProviderFuture<'a, MediaItem>;

    fn episodes<'a>(
        &'a self,
        host: Arc<dyn ProviderHost>,
        request: MediaRequest,
    ) -> ProviderFuture<'a, Vec<Episode>>;

    fn streams<'a>(
        &'a self,
        host: Arc<dyn ProviderHost>,
        request: StreamRequest,
    ) -> ProviderFuture<'a, Vec<ProviderStream>>;
}
