//! Settings sync regressions: one executable, isolated cases.

#[path = "support/isolation.rs"]
mod isolation;

#[path = "support/destinations.rs"]
mod destinations;

#[path = "settings_sync/qr.rs"]
mod qr;

#[path = "settings_sync/scopes.rs"]
mod scopes;

#[path = "settings_sync/sync_overflow.rs"]
mod sync_overflow;
