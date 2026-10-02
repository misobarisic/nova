//! Account-scoped anime tracking domain and native service clients.
pub mod api;
pub mod auth;
mod cache;
pub mod callback;
mod edits;
mod journal;
mod mapping;
mod models;
mod outbox;
mod persistence;
mod projection;
mod resolution;
pub use resolution::{TitleMatch, filter_linked_candidates, rank_candidates, title_match};
mod service;

pub use mapping::{ValidationError, validate_bindings};
pub use models::*;
pub use outbox::{
    DeliveryAttempt, DeliveryFailure, DeliveryOutcome, DeliveryState, MappingStamp, Outbox,
    PendingProgress, ProgressIntent, TrackingError,
};
pub use persistence::{KvStorage, LoadError, StateStorage, Store, TRACKING_STATE_KEY};
pub use projection::{Observation, ProgressProposal, Projection, ProjectionError};

#[cfg(test)]
mod tests;

pub use service::*;
#[cfg(test)]
mod api_tests;

pub use journal::*;

pub use edits::*;

pub use cache::*;
