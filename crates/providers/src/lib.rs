//! Shared provider contract and isolated JavaScript runtime.
//!
//! Provider identifiers are opaque and always namespaced by the provider ID.
//! Providers return normalized Nova models. A Stremio adapter and the installed
//! source's protocol bridge let the app reuse its catalog and library flows.

pub mod catalog_grouping;
mod hls_proxy;
mod host;
mod ids;
mod matching;
mod metadata;
mod models;
mod package;
pub mod plugin_settings;
mod registry;
pub use package::{AddonPackage, MAX_ARCHIVE_BYTES};
mod runtime;
mod sequence;
mod stremio;
#[cfg(test)]
mod supernova_description_tests;
#[cfg(test)]
mod supernova_evidence_tests;
#[cfg(test)]
mod supernova_live_tests;
#[cfg(test)]
mod supernova_source_evidence_tests;
#[cfg(test)]
mod supernova_tests;
#[cfg(test)]
mod supernova_track_metadata_tests;
mod torrent_metadata;

pub use host::{
    HttpBody, HttpRequest, HttpResponse, HttpResponseType, MemoryProviderHost, ProviderHost,
    ProviderHostError, ScopedHttpHost,
};
pub use ids::{ExternalId, IdNamespace, IdResolution};
pub use matching::{MetadataMatch, match_metadata};
pub use metadata::{
    AddonMetadataTransport, EnrichmentRequest, EnrichmentResult, EpisodeEnrichment, MetadataAddon,
    MetadataCache, MetadataConnection, ProviderDetails, addon_metadata_id, apply_enrichment,
    configure_metadata_addons, enrich_addon_response, enrich_addon_response_with_budget,
    enrich_metadata, metadata_revision, reconcile_episode_metadata, set_metadata_cache,
    set_metadata_transport,
};
pub use models::{
    CatalogRequest, ContentProvider, Episode, ExternalIds, MediaItem, MediaRequest,
    ProviderCatalog, ProviderDescriptor, ProviderError, ProviderFuture, ProviderResolveError,
    ProviderStream, ProviderStreamResolver, ProviderSubtitle, SearchRequest, StreamLookupRequest,
    StreamRequest,
};
pub use registry::{
    ANIKOTO_PROVIDER_URL, OwnedProviderSnapshot, ProviderCapabilities, ResolverCapability,
    SUPERNOVA_PROVIDER_URL, capture_resolver_capability, configure_provider_capabilities,
    configured_provider_manifest, fetch_builtin_addon, fetch_builtin_addon_with_progress,
    fetch_builtin_addon_with_progress_for_generation, fetch_builtin_addon_with_timeout,
    has_installed_provider, installed_packages, mark_known_provider_ids, private_provider_id,
    provider_asset_bytes, provider_capabilities, provider_owns_id, provider_settings_schema,
    provider_uses_stream_preferences, resolve_provider_stream, set_installed_packages,
    stable_stream_identity, stream_lookup_url, supports_capability_config,
    supports_contextual_streams, supports_provider_configuration,
};
pub use runtime::{
    EphemeralPlaybackCredential, PluginCapabilities, PluginLimits, PluginManifest,
    PluginPermissions, PluginRuntime,
};
pub use sequence::{EpisodeResolution, SequenceEpisode, SourceSequence, resolve_episode};
pub use stremio::{StremioProvider, normalize_external_ids};
pub use torrent_metadata::{TorrentMetadata, TorrentMetadataFile, inspect_torrent_metadata};

#[cfg(test)]
pub use registry::{bundled_asset_bytes, bundled_providers};
