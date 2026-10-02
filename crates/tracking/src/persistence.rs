use serde::{Deserialize, Serialize};

use crate::{TrackingState, ValidationError};

pub const TRACKING_STATE_KEY: &str = "tracking:state:v1";
// Keep the storage key stable so an older binary encounters the newer
// envelope and refuses to overwrite durable intents it cannot understand.
const SCHEMA_VERSION: u32 = 3;

/// Injection keeps recovery tests independent of the process-global database.
pub trait StateStorage {
    fn read(&self, key: &str) -> Result<Option<String>, nova_storage::Error>;
    fn write(&self, entries: &[(String, Option<String>)]) -> Result<(), nova_storage::Error>;
}

pub struct KvStorage;
impl StateStorage for KvStorage {
    fn read(&self, key: &str) -> Result<Option<String>, nova_storage::Error> {
        nova_storage::try_get_str(key)
    }
    fn write(&self, entries: &[(String, Option<String>)]) -> Result<(), nova_storage::Error> {
        nova_storage::try_write_batch(entries)
    }
}

#[derive(Debug)]
pub enum LoadError {
    Storage(nova_storage::Error),
    InvalidState(String),
    UnsupportedVersion(u64),
}
impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Storage(error) => write!(f, "tracking storage: {error}"),
            Self::InvalidState(reason) => write!(f, "unreadable tracking state retained: {reason}"),
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported tracking schema {version}; state retained")
            }
        }
    }
}
impl std::error::Error for LoadError {}
impl From<nova_storage::Error> for LoadError {
    fn from(error: nova_storage::Error) -> Self {
        Self::Storage(error)
    }
}
impl From<ValidationError> for LoadError {
    fn from(error: ValidationError) -> Self {
        Self::InvalidState(error.to_string())
    }
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    version: u32,
    state: TrackingState,
}

/// Only successfully loaded state is writable. A corrupt or future record
/// cannot silently become an empty, writable store. Keep one owner per process.
pub struct Store<S: StateStorage = KvStorage> {
    storage: S,
    state: TrackingState,
}
impl<S: StateStorage> Store<S> {
    /// Explicit user recovery only: retain the original record before creating
    /// a fresh local state. Never call this as an automatic load fallback.
    pub fn reset_retaining_backup(storage: S) -> Result<Self, LoadError> {
        Self::reset_with_journal_backup(storage, &[])
    }
    /// The app supplies journal rows while holding its history writer lock.
    /// Retain them before atomically deleting the old journal and resetting its
    /// sequence, so a failed reset leaves the original state usable.
    pub fn reset_with_journal_backup(
        storage: S,
        rows: &[(String, String)],
    ) -> Result<Self, LoadError> {
        if rows
            .iter()
            .any(|(key, _)| !key.starts_with(crate::EVENT_PREFIX))
        {
            return Err(LoadError::InvalidState("invalid journal key".into()));
        }
        if !rows.is_empty() {
            let raw = serde_json::to_string(rows)
                .map_err(|error| LoadError::InvalidState(error.to_string()))?;
            quarantine(&storage, &raw)?;
        }
        if let Some(raw) = storage.read(TRACKING_STATE_KEY)? {
            quarantine(&storage, &raw)?;
        }
        if let Some(counter) = storage.read(crate::EVENT_COUNTER_KEY)? {
            quarantine(&storage, &counter)?;
        }
        if let Some(marker) = storage.read(crate::JOURNAL_ERROR_KEY)? {
            quarantine(&storage, &marker)?;
        }
        let state = TrackingState::default();
        let raw = serde_json::to_string(&Envelope {
            version: SCHEMA_VERSION,
            state: state.clone(),
        })
        .map_err(|error| LoadError::InvalidState(error.to_string()))?;
        let mut entries = vec![
            (TRACKING_STATE_KEY.into(), Some(raw)),
            (crate::JOURNAL_ERROR_KEY.into(), None),
            (crate::EVENT_COUNTER_KEY.into(), Some("0".into())),
        ];
        entries.extend(rows.iter().map(|(key, _)| (key.clone(), None)));
        storage.write(&entries)?;
        Ok(Self { storage, state })
    }
    pub fn load(storage: S) -> Result<Self, LoadError> {
        let Some(raw) = storage.read(TRACKING_STATE_KEY)? else {
            return Ok(Self {
                storage,
                state: TrackingState::default(),
            });
        };
        // Inspect the version before decoding fields that a newer schema might
        // intentionally represent differently. Newer versions are read-only.
        let decoded = (|| {
            let value: serde_json::Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
            let version = value
                .get("version")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| "missing schema version".to_owned())?;
            if !(1..=u64::from(SCHEMA_VERSION)).contains(&version) {
                return Ok(Err(version));
            }
            if version >= 2
                && value
                    .get("state")
                    .and_then(|state| state.get("outbox"))
                    .is_none()
            {
                return Err("missing outbox in tracking schema 2".to_owned());
            }
            if version >= 3
                && (value["state"].get("link_checkpoints").is_none()
                    || value["state"].get("snapshots").is_none()
                    || value["state"]["outbox"].get("edits").is_none())
            {
                return Err("missing tracking journal or manual-edit fields in schema 3".into());
            }
            let envelope: Envelope = serde_json::from_value(value).map_err(|e| e.to_string())?;
            envelope.state.validate().map_err(|e| e.to_string())?;
            Ok::<_, String>(Ok(envelope.state))
        })();
        match decoded {
            Ok(Ok(mut state)) => {
                state.outbox.recover_interrupted();
                Ok(Self { storage, state })
            }
            Ok(Err(version)) => Err(LoadError::UnsupportedVersion(version)),
            Err(reason) => {
                quarantine(&storage, &raw)?;
                Err(LoadError::InvalidState(reason))
            }
        }
    }

    /// Tracking checkpoints and their pending intent commit together. The app's
    /// playback-history transaction must be integrated separately before hooks
    /// can call this from local progress persistence.
    pub fn observe_episode(
        &mut self,
        binding_id: &str,
        episode: &crate::SourceEpisode,
        watched: bool,
    ) -> Result<Option<std::num::NonZeroU64>, crate::TrackingError> {
        self.mutate(|state| state.observe_episode(binding_id, episode, watched))
    }

    pub fn replace_progress(
        &mut self,
        target: &crate::TargetKey,
        progress: u32,
        observations: Vec<crate::Observation>,
    ) -> Result<std::num::NonZeroU64, crate::TrackingError> {
        self.mutate(|state| state.replace_progress(target, progress, observations))
    }

    pub fn begin_delivery(
        &mut self,
        target: &crate::TargetKey,
        now: u64,
    ) -> Result<Option<crate::DeliveryAttempt>, crate::TrackingError> {
        self.mutate(|state| state.begin_delivery(target, now))
    }

    pub fn finish_delivery(
        &mut self,
        attempt: &crate::DeliveryAttempt,
        outcome: crate::DeliveryOutcome,
        now: u64,
    ) -> Result<(), crate::TrackingError> {
        self.mutate(|state| state.finish_delivery(attempt, outcome, now))
    }

    pub fn retry_target(
        &mut self,
        target: &crate::TargetKey,
        now: u64,
    ) -> Result<bool, crate::TrackingError> {
        self.mutate(|state| Ok(state.retry_target(target, now)))
    }

    /// Call only after the adapter has verified refreshed authentication for
    /// this exact account/generation. Replaced accounts require new bindings.
    pub fn resume_account(
        &mut self,
        account: &crate::AccountKey,
        generation: std::num::NonZeroU64,
    ) -> Result<(), crate::TrackingError> {
        self.mutate(|state| state.resume_account(account, generation))
    }

    pub fn mutate<T>(
        &mut self,
        apply: impl FnOnce(&mut TrackingState) -> Result<T, crate::TrackingError>,
    ) -> Result<T, crate::TrackingError> {
        let mut state = self.state.clone();
        let result = apply(&mut state)?;
        if state != self.state {
            self.save(state)?;
        }
        Ok(result)
    }

    pub fn state(&self) -> &TrackingState {
        &self.state
    }

    /// Publish in-memory state only after durable commit.
    pub fn save(&mut self, state: TrackingState) -> Result<(), LoadError> {
        self.save_with_events(state, &[])
    }

    /// Consumed journal keys are deleted in the same transaction as their
    /// observations/intents. Neither side may succeed independently.
    pub fn save_with_events(
        &mut self,
        state: TrackingState,
        consumed: &[String],
    ) -> Result<(), LoadError> {
        if consumed
            .iter()
            .any(|key| !key.starts_with(crate::EVENT_PREFIX))
        {
            return Err(LoadError::InvalidState("invalid tracking event key".into()));
        }
        state.validate()?;
        let raw = serde_json::to_string(&Envelope {
            version: SCHEMA_VERSION,
            state: state.clone(),
        })
        .map_err(|error| LoadError::InvalidState(error.to_string()))?;
        let mut entries = vec![(TRACKING_STATE_KEY.into(), Some(raw))];
        entries.extend(consumed.iter().map(|key| (key.clone(), None)));
        self.storage.write(&entries)?;
        self.state = state;
        Ok(())
    }
}

fn quarantine(storage: &impl StateStorage, raw: &str) -> Result<(), nova_storage::Error> {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in raw.bytes() {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    let prefix = format!("tracking:quarantine:{hash:016x}");
    let mut key = prefix.clone();
    let mut collision = 0_u64;
    loop {
        match storage.read(&key)? {
            Some(existing) if existing == raw => return Ok(()),
            Some(_) => {
                collision += 1;
                key = format!("{prefix}:{collision}");
            }
            None => return storage.write(&[(key, Some(raw.into()))]),
        }
    }
}
