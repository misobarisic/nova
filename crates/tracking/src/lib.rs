//! Local anime tracking domain. No credentials, UI, or network calls live here.
mod mapping;
mod models;
mod persistence;
mod projection;

pub use mapping::{ValidationError, validate_bindings};
pub use models::*;
pub use persistence::{KvStorage, LoadError, StateStorage, Store, TRACKING_STATE_KEY};
pub use projection::{Observation, ProgressProposal, Projection, ProjectionError};

#[cfg(test)]
mod tests;
