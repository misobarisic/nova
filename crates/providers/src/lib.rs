//! Shared provider contract and isolated JavaScript runtime.
//!
//! Provider identifiers are opaque and always namespaced by the provider ID.
//! Providers return normalized Nova models. A Stremio adapter and the bundled
//! source's protocol bridge let the app reuse its catalog and library flows.

mod host;
mod ids;
mod matching;
mod metadata;
mod models;
mod registry;
mod runtime;
mod sequence;
mod stremio;

pub use host::{HttpResponse, MemoryProviderHost, ProviderHost, ProviderHostError, ScopedHttpHost};
pub use ids::{ExternalId, IdNamespace, IdResolution};
pub use matching::{MetadataMatch, match_metadata};
pub use metadata::{
    AddonMetadataTransport, EnrichmentRequest, EnrichmentResult, EpisodeEnrichment, MetadataAddon,
    MetadataCache, MetadataConnection, ProviderDetails, addon_metadata_id, apply_enrichment,
    configure_metadata_addons, enrich_addon_response, enrich_metadata, metadata_revision,
    set_metadata_cache, set_metadata_transport,
};
pub use models::{
    CatalogRequest, ContentProvider, Episode, ExternalIds, MediaItem, MediaRequest,
    ProviderCatalog, ProviderDescriptor, ProviderError, ProviderFuture, ProviderStream,
    ProviderSubtitle, SearchRequest, StreamLookupRequest, StreamRequest,
};
pub use registry::{
    ANIKOTO_PROVIDER_URL, bundled_providers, fetch_builtin_addon, fetch_builtin_addon_with_timeout,
    private_provider_id, provider_owns_id, stream_lookup_url, supports_contextual_streams,
};
pub use runtime::{
    PluginCapabilities, PluginLimits, PluginManifest, PluginPermissions, PluginRuntime,
};
pub use sequence::{EpisodeResolution, SequenceEpisode, SourceSequence, resolve_episode};
pub use stremio::{StremioProvider, normalize_external_ids};
