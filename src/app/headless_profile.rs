//! Explicit durable provider authority shared by background jobs. Constructing
//! this value never publishes a profile, player, Slint model or active service.
use super::*;
#[derive(Clone)]
pub(super) struct Runtime {
    pub owner: profiles::ProfileStore,
    pub settings: nova_providers::plugin_settings::SettingsCapability,
    pub inventory: nova_providers::OwnedProviderSnapshot,
    pub addons: Vec<AddonStore>,
    baseline: Vec<(String, Option<String>)>,
}
impl Runtime {
    pub fn new(owner: profiles::ProfileStore) -> Result<Self, String> {
        if owner.retired().map_err(|e| e.to_string())? {
            return Err("profile retired".into());
        }
        let baseline = [
            "addons",
            addon_packages::KEY,
            plugin_settings::KEY,
            "settings",
            profile_policy::KEY,
        ]
        .into_iter()
        .map(|key| {
            owner
                .read(key)
                .map(|raw| (key.to_owned(), raw))
                .map_err(|e| e.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
        let addons: Vec<AddonStore> = owner
            .read("addons")
            .map_err(|e| e.to_string())?
            .map(|raw| serde_json::from_str(&raw))
            .transpose()
            .map_err(|e| e.to_string())?
            .unwrap_or_default();
        let packages = addon_packages::owned_packages(&owner)?;
        let metadata = addons
            .iter()
            .filter(|a| a.enabled)
            .filter_map(|a| {
                read_cached_manifest_for(&a.url).map(|manifest| nova_providers::MetadataAddon {
                    url: a.url.clone(),
                    manifest,
                })
            })
            .collect();
        let configs = addons
            .iter()
            .filter(|a| a.enabled)
            .map(|a| (a.url.clone(), a.capabilities.unwrap_or_default()))
            .collect();
        let prepared = nova_providers::plugin_settings::prepare_plugin_settings(Arc::new(
            plugin_settings::Backend::for_store(owner.clone()),
        ))
        .map_err(|e| e.to_string())?;
        Ok(Self {
            owner,
            settings: nova_providers::plugin_settings::owned_settings_capability(prepared),
            inventory: nova_providers::OwnedProviderSnapshot::new(packages, configs, metadata),
            addons,
            baseline,
        })
    }
    pub fn current(&self) -> bool {
        self.settings.is_current()
            && self.owner.retired().is_ok_and(|retired| !retired)
            && self
                .baseline
                .iter()
                .all(|(key, saved)| self.owner.read(key).is_ok_and(|raw| raw == *saved))
    }
    pub fn fetch_cancelled(&self, url: String, cancel: &AtomicBool) -> Result<Vec<u8>, String> {
        if !self.current() {
            return Err("background owner changed".into());
        }
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        nova_media::net::fetch_bytes_with_progress_for_owner(
            url,
            Arc::new(|_| {}),
            self.settings.clone(),
            self.inventory.clone(),
            move |result| {
                let _ = tx.send(result);
            },
        );
        let start = std::time::Instant::now();
        loop {
            if cancel.load(Ordering::Acquire) || !self.current() {
                return Err("background request cancelled".into());
            }
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(result) => return result.map_err(|error| error.message),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("background request ended".into());
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                    if start.elapsed() < Duration::from_secs(40) => {}
                Err(_) => return Err("background request timed out".into()),
            }
        }
    }
}

/// One bounded metadata refresh using the same typed cache/reconciliation as UI
/// requests. A stale/revoked owner never publishes a late response.
#[cfg(target_os = "android")]
pub(super) fn refresh_metadata(
    owner: &profiles::ProfileStore,
    media_type: &str,
    id: &str,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let runtime = Runtime::new(owner.clone())?;
    let addons_baseline = owner.read("addons").map_err(|e| e.to_string())?;
    let policy_baseline = owner.read(profile_policy::KEY).map_err(|e| e.to_string())?;
    let cache_key = episodes_key(media_type, id);
    let cache_baseline = owner.read(&cache_key).map_err(|e| e.to_string())?;
    if !profile_policy::allows_title(owner, media_type, id) {
        return Err("title unavailable".into());
    }
    for addon in runtime.addons.iter().filter(|a| a.enabled).take(4) {
        if cancel.load(Ordering::Acquire) {
            return Err("cancelled".into());
        }
        let Some(manifest) = read_cached_manifest_for(&addon.url) else {
            continue;
        };
        if !manifest.accepts("meta", media_type, id) {
            continue;
        }
        let endpoint = Addon::new(&addon.url)
            .map_err(|e| e.to_string())?
            .meta_url(media_type, id);
        let Ok(bytes) = runtime.fetch_cancelled(endpoint, cancel) else {
            continue;
        };
        if cancel.load(Ordering::Acquire) || !runtime.current() {
            return Err("cancelled".into());
        }
        if let Some(item) = Addon::parse_meta(&bytes).map_err(|e| e.to_string())?
            && meta_matches_request(&item, media_type, id)
        {
            let saved = owner.run(|| {
                detail::write_episode_meta_cache_guarded(media_type, id, &item, || {
                    runtime.current()
                        && !cancel.load(Ordering::Acquire)
                        && owner.read("addons").is_ok_and(|raw| raw == addons_baseline)
                        && owner
                            .read(profile_policy::KEY)
                            .is_ok_and(|raw| raw == policy_baseline)
                        && owner
                            .read(&cache_key)
                            .is_ok_and(|raw| raw == cache_baseline)
                        && profile_policy::allows_title(owner, media_type, id)
                })
            });
            if saved.is_none() {
                return Err("metadata baseline changed".into());
            }
            return Ok(());
        }
    }
    Err("metadata unavailable; cached inventory retained".into())
}
