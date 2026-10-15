//! Official Stremio transport and pure, field-level library reconciliation.
//! Protocol reference: Stremio/stremio-core, development branch,
//! `src/types/{api,library}` and `stremio-watched-bitfield`.
//! Verified against upstream revision c4806e8614452fd49d5dbb22db74afa88959b554.
mod api;
mod library;
#[cfg(test)]
mod tests;
pub use api::*;
pub use library::*;

use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt};
use zeroize::Zeroize;

#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);
impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}
impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiError {
    Authentication,
    Offline,
    Timeout,
    RateLimited { retry_at: u64 },
    Http { status: u16 },
    RemoteRejected { code: Option<u64> },
    Rejected,
    InvalidInput,
    InvalidResponse,
}
impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ApiError {}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resume {
    pub video_id: String,
    pub position_ms: u64,
    pub duration_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Projection {
    pub saved: bool,
    pub watched: BTreeMap<String, bool>,
    pub resume: Option<Resume>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamped<T> {
    pub value: T,
    pub changed_at: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Canonical {
    pub saved: Stamped<bool>,
    pub watched: BTreeMap<String, Stamped<bool>>,
    pub resume: Stamped<Option<Resume>>,
}
impl Canonical {
    pub fn from_projection(value: &Projection, stamp: u64) -> Self {
        Self {
            saved: Stamped {
                value: value.saved,
                changed_at: stamp,
            },
            watched: value
                .watched
                .iter()
                .map(|(id, value)| {
                    (
                        id.clone(),
                        Stamped {
                            value: *value,
                            changed_at: stamp,
                        },
                    )
                })
                .collect(),
            resume: Stamped {
                value: value.resume.clone(),
                changed_at: stamp,
            },
        }
    }
    pub fn projection(&self) -> Projection {
        Projection {
            saved: self.saved.value,
            watched: self
                .watched
                .iter()
                .map(|(id, v)| (id.clone(), v.value))
                .collect(),
            resume: self.resume.value.clone(),
        }
    }
    /// Observe changes against a previously acknowledged side. Unrelated title
    /// edits do not acquire a newer timestamp for every watched episode.
    pub fn observe(&mut self, value: &Projection, stamp: u64) {
        fn update<T: PartialEq + Clone>(target: &mut Stamped<T>, value: &T, stamp: u64) {
            if target.value != *value {
                *target = Stamped {
                    value: value.clone(),
                    changed_at: stamp,
                };
            }
        }
        update(&mut self.saved, &value.saved, stamp);
        update(&mut self.resume, &value.resume, stamp);
        // Missing episode keys mean unavailable metadata, not an unwatch event.
        for (id, v) in &value.watched {
            match self.watched.get_mut(id) {
                Some(target) => update(target, v, stamp),
                None => {
                    self.watched.insert(
                        id.clone(),
                        Stamped {
                            value: *v,
                            changed_at: stamp,
                        },
                    );
                }
            }
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Baseline {
    pub local: Canonical,
    pub remote: Canonical,
}

/// Initial linking unions positive membership/history. After linking, explicit
/// removals and unwatches are ordinary events; equal timestamps favor Stremio.
pub fn reconcile(local: &Canonical, remote: &Canonical, initial: bool) -> Canonical {
    fn latest<T: Clone>(a: &Stamped<T>, b: &Stamped<T>) -> Stamped<T> {
        if a.changed_at > b.changed_at {
            a.clone()
        } else {
            b.clone()
        }
    }
    fn boolean(a: &Stamped<bool>, b: &Stamped<bool>, initial: bool) -> Stamped<bool> {
        if initial && a.value != b.value {
            if a.value { a.clone() } else { b.clone() }
        } else {
            latest(a, b)
        }
    }
    let mut watched = local.watched.clone();
    for (id, b) in &remote.watched {
        watched
            .entry(id.clone())
            .and_modify(|a| *a = boolean(a, b, initial))
            .or_insert_with(|| b.clone());
    }
    Canonical {
        saved: boolean(&local.saved, &remote.saved, initial),
        watched,
        resume: latest(&local.resume, &remote.resume),
    }
}
