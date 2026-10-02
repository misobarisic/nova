use serde::{Deserialize, Serialize};

use crate::{TrackingState, ValidationError};

pub const TRACKING_STATE_KEY: &str = "tracking:state:v1";
const SCHEMA_VERSION: u32 = 1;

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
            if version != u64::from(SCHEMA_VERSION) {
                return Ok(Err(version));
            }
            let envelope: Envelope = serde_json::from_value(value).map_err(|e| e.to_string())?;
            envelope.state.validate().map_err(|e| e.to_string())?;
            Ok::<_, String>(Ok(envelope.state))
        })();
        match decoded {
            Ok(Ok(state)) => Ok(Self { storage, state }),
            Ok(Err(version)) => Err(LoadError::UnsupportedVersion(version)),
            Err(reason) => {
                quarantine(&storage, &raw)?;
                Err(LoadError::InvalidState(reason))
            }
        }
    }

    pub fn state(&self) -> &TrackingState {
        &self.state
    }

    /// Publish in-memory state only after durable commit.
    pub fn save(&mut self, state: TrackingState) -> Result<(), LoadError> {
        state.validate()?;
        let raw = serde_json::to_string(&Envelope {
            version: SCHEMA_VERSION,
            state: state.clone(),
        })
        .map_err(|error| LoadError::InvalidState(error.to_string()))?;
        self.storage
            .write(&[(TRACKING_STATE_KEY.into(), Some(raw))])?;
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
