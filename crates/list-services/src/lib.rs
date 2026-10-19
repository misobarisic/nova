//! Official Simkl, MDBList and Trakt APIs. Credentials never appear in Debug or errors.
//! The caller owns scheduling, profile isolation and explicit synchronization scope.
mod api;
mod models;
pub use api::*;
pub use models::*;
