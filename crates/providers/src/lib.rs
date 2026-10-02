//! Shared provider contract and isolated JavaScript runtime.
//!
//! Provider identifiers are opaque and always namespaced by the provider ID.
//! Providers return normalized Nova models. A Stremio adapter and the bundled
//! source's protocol bridge let the app reuse its catalog and library flows.

mod anikoto;
mod host;
mod matching;
mod models;
mod runtime;
mod stremio;

pub use anikoto::{
    ANIKOTO_PROVIDER_URL, builtin_manifest, builtin_stream_lookup_url, fetch_builtin_addon,
};
pub use host::{HttpResponse, MemoryProviderHost, ProviderHost, ProviderHostError, ScopedHttpHost};
pub use matching::{MetadataMatch, match_metadata};
pub use models::{
    CatalogRequest, ContentProvider, Episode, ExternalIds, MediaItem, MediaRequest,
    ProviderCatalog, ProviderDescriptor, ProviderError, ProviderFuture, ProviderStream,
    ProviderSubtitle, SearchRequest, StreamLookupRequest, StreamRequest,
};
pub use runtime::{PluginLimits, PluginManifest, PluginPermissions, PluginRuntime};
pub use stremio::StremioProvider;
