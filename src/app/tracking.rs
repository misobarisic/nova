//! Local tracker state belongs to the app, separate from addon credentials and
//! the Nova-to-Nova sync projection. Delivery is not enabled at this stage.
use super::*;

pub(super) type StateHandle =
    Arc<Mutex<Option<Result<nova_tracking::Store, nova_tracking::LoadError>>>>;

impl Bridge {
    pub(super) fn initialize_tracking(&self) {
        let tracking = self.tracking.clone();
        thread::spawn(move || {
            let loaded = nova_tracking::Store::load(nova_tracking::KvStorage);
            match &loaded {
                Ok(store) => tracing::debug!(
                    accounts = store.state().accounts.len(),
                    bindings = store.state().bindings.len(),
                    "loaded local tracking state"
                ),
                Err(error) => {
                    tracing::warn!(%error, "tracking unavailable; playback remains available")
                }
            }
            *tracking.lock().unwrap_or_else(|error| error.into_inner()) = Some(loaded);
        });
    }
}
