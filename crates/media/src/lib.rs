//! Media layer: platform HTTP transport and Nova adapters to nova-kit's image cache.
//!
//! Transport stays local; decoded artwork, processed disk variants, cache policy
//! and maintenance are shared through `nova-cache`.
pub mod cache;
pub mod net;
