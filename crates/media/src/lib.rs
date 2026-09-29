//! Media layer: HTTP transport and the decoded/poster image cache.
//!
//! Split out of the app crate so poster loading, the disk cache and the
//! network retry policy can be reused without pulling in the whole catalog
//! app. The reverse dependency (transport decoding through the cache
//! pipeline) is internal to this crate.
pub mod cache;
pub mod net;
