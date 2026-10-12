//! Durable package descriptors and immutable, hash-addressed addon archives.
use super::*;
use nova_providers::AddonPackage;
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
};

pub(super) const KEY: &str = "addon_packages:v1";
pub(super) const DOMAIN: &str = "addon_packages";
const IDS_KEY: &str = "addon_package_ids:v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PackageDescriptor {
    pub id: String,
    pub version: String,
    pub sha256: String,
}
impl PackageDescriptor {
    fn from_package(package: &AddonPackage) -> Self {
        Self {
            id: package.id().into(),
            version: package.version().into(),
            sha256: package.sha256().into(),
        }
    }
    fn validate(&self, key: &str) -> Result<(), String> {
        if key != self.id
            || !valid_id(&self.id)
            || !valid_hash(&self.sha256)
            || semver::Version::parse(&self.version).is_err()
        {
            return Err("invalid addon package descriptor".into());
        }
        Ok(())
    }
    fn matches(&self, package: &AddonPackage) -> bool {
        self.id == package.id()
            && self.version == package.version()
            && self.sha256 == package.sha256()
    }
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
}
fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn parse(raw: &str) -> Result<BTreeMap<String, PackageDescriptor>, String> {
    let descriptors: BTreeMap<String, PackageDescriptor> =
        serde_json::from_str(raw).map_err(|e| e.to_string())?;
    for (id, descriptor) in &descriptors {
        descriptor.validate(id)?;
    }
    Ok(descriptors)
}
pub(super) fn records(raw: &str) -> Result<Vec<(String, String, u64)>, String> {
    parse(raw)?
        .into_iter()
        .map(|(id, descriptor)| {
            serde_json::to_string(&descriptor)
                .map(|raw| (id, raw, 0))
                .map_err(|e| e.to_string())
        })
        .collect()
}
fn archive_path(hash: &str) -> Result<PathBuf, String> {
    if !valid_hash(hash) {
        return Err("invalid addon package hash".into());
    }
    Ok(nova_config::app_data_dir()
        .join("addons")
        .join("packages")
        .join(format!("{hash}.zip")))
}
fn read_archive(hash: &str) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    fs::File::open(archive_path(hash)?)
        .map_err(|e| e.to_string())?
        .take((nova_providers::MAX_ARCHIVE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > nova_providers::MAX_ARCHIVE_BYTES {
        return Err("addon package archive exceeds size limit".into());
    }
    Ok(bytes)
}
fn load_descriptor(descriptor: &PackageDescriptor) -> Result<Arc<AddonPackage>, String> {
    let package =
        AddonPackage::parse(&read_archive(&descriptor.sha256)?).map_err(|e| e.to_string())?;
    if !descriptor.matches(&package) {
        return Err("addon package identity or hash mismatch".into());
    }
    Ok(package)
}
fn store_archive(package: &AddonPackage, bytes: &[u8]) -> Result<(), String> {
    let path = archive_path(package.sha256())?;
    if read_archive(package.sha256()).is_ok_and(|old| old == bytes) {
        return Ok(());
    }
    let dir = path.parent().unwrap();
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let temporary = dir.join(format!(
        ".{}.{}-{}.tmp",
        package.sha256(),
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, &path)?;
        // Directory durability matters: metadata must never reference an archive
        // whose rename was lost after an otherwise successful transaction.
        #[cfg(unix)]
        fs::File::open(dir)?.sync_all()?;
        Ok::<_, std::io::Error>(())
    })();
    let _ = fs::remove_file(&temporary);
    result.map_err(|e| e.to_string())
}
fn remember_ids(descriptors: &BTreeMap<String, PackageDescriptor>, addons: &[AddonStore]) {
    let previous = read_json::<Vec<String>>(IDS_KEY).unwrap_or_default();
    let mut ids = previous.clone();
    ids.extend(descriptors.keys().cloned());
    ids.extend(addons.iter().filter_map(|a| {
        a.url
            .strip_prefix("nova-provider://")
            .map(|id| id.trim_end_matches('/').to_owned())
    }));
    ids.retain(|id| valid_id(id));
    ids.sort();
    ids.dedup();
    nova_providers::mark_known_provider_ids(ids.clone());
    if ids != previous {
        write_json(IDS_KEY, &ids);
    }
}
fn snapshot_addons(bridge: &Bridge) -> Vec<AddonStore> {
    let persisted = addon_mgr::read_persisted_addons();
    bridge
        .shared
        .lock()
        .unwrap()
        .installed
        .iter()
        .map(|addon| AddonStore {
            capabilities: nova_providers::provider_capabilities(&addon.url).or_else(|| {
                persisted
                    .iter()
                    .find(|old| old.url == addon.url)
                    .and_then(|old| old.capabilities)
            }),
            url: addon.url.clone(),
            enabled: addon.enabled,
            configure_ok: addon.configure_ok,
            label: addon.label.clone(),
        })
        .collect()
}
fn publish(bridge: &Bridge) {
    let state = bridge.shared.lock().unwrap();
    let packages = state
        .installed
        .iter()
        .filter_map(|addon| {
            let id = addon
                .url
                .strip_prefix("nova-provider://")?
                .trim_end_matches('/');
            state.loaded_packages.get(id).cloned()
        })
        .collect();
    drop(state);
    nova_providers::set_installed_packages(packages);
    // A receipt may have been missing when membership/capabilities arrived.
    // Apply the durable flags after registration makes its manifest available.
    for addon in addon_mgr::read_persisted_addons() {
        if nova_providers::has_installed_provider(&addon.url) {
            nova_providers::configure_provider_capabilities(
                &addon.url,
                addon.capabilities.unwrap_or_default(),
            );
        }
    }
}
fn current_bridge(bridge: &Bridge) -> bool {
    bridge.app().is_some()
        && super::bridge::with_global_bridge(|current| Arc::ptr_eq(&bridge.shared, &current.shared))
            == Some(true)
}
fn accepted_update(old: Option<&PackageDescriptor>, new: &PackageDescriptor) -> Result<(), String> {
    if let Some(old) = old {
        let old_version = semver::Version::parse(&old.version).map_err(|e| e.to_string())?;
        let version = semver::Version::parse(&new.version).map_err(|e| e.to_string())?;
        if version.cmp_precedence(&old_version).is_lt() {
            return Err("addon package downgrade rejected".into());
        }
        if version.cmp_precedence(&old_version).is_eq() && old.sha256 != new.sha256 {
            return Err("addon version already identifies different package bytes".into());
        }
    }
    Ok(())
}

pub(super) fn initialize(bridge: &Bridge) {
    configure_blob_store();
    // Retained unfamiliar domains must be recovered before an empty snapshot
    // becomes the durable baseline on startup.
    let snapshot = match storage::try_get_str(KEY) {
        Ok(snapshot) => snapshot,
        Err(_) => {
            io::block_unreadable(KEY);
            crate::web_log("nova: unreadable addon package snapshot retained");
            return;
        }
    };
    if snapshot.is_none() {
        let restored = (|| -> Result<(), String> {
            let owner = nova_sync::local_store().map_err(|e| e.to_string())?;
            let store = owner.lock().unwrap();
            if !store.domains().iter().any(|domain| domain == DOMAIN) {
                return Ok(());
            }
            let values: BTreeMap<String, PackageDescriptor> = store
                .records(DOMAIN)
                .into_iter()
                .map(|(id, raw)| serde_json::from_str(&raw).map(|descriptor| (id, descriptor)))
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?;
            drop(store);
            let raw = serde_json::to_string(&values).map_err(|e| e.to_string())?;
            parse(&raw)?;
            let _guard = sync::ApplyingGuard::new();
            sync::persist_sync_snapshot(KEY, &raw, false).map_err(|e| e.to_string())
        })();
        if restored.is_err() {
            io::block_unreadable(KEY);
            crate::web_log("nova: could not restore retained addon packages");
            return;
        }
    }
    let descriptors = read_json::<BTreeMap<String, PackageDescriptor>>(KEY).unwrap_or_default();
    if descriptors
        .iter()
        .any(|(id, descriptor)| descriptor.validate(id).is_err())
    {
        io::block_unreadable(KEY);
        crate::web_log("nova: invalid addon package descriptors retained");
        return;
    }
    let addons = addon_mgr::read_persisted_addons();
    remember_ids(&descriptors, &addons);
    let mut loaded = BTreeMap::new();
    for addon in &addons {
        let Some(id) = addon
            .url
            .strip_prefix("nova-provider://")
            .map(|id| id.trim_end_matches('/'))
        else {
            continue;
        };
        if let Some(descriptor) = descriptors.get(id)
            && let Ok(package) = load_descriptor(descriptor)
        {
            loaded.insert(id.to_owned(), package);
        }
    }
    nova_providers::set_installed_packages(
        addons
            .iter()
            .filter_map(|addon| {
                loaded
                    .get(
                        addon
                            .url
                            .strip_prefix("nova-provider://")?
                            .trim_end_matches('/'),
                    )
                    .cloned()
            })
            .collect(),
    );
    let mut state = bridge.shared.lock().unwrap();
    state.package_descriptors = descriptors;
    state.loaded_packages = loaded;
}

pub(super) fn project(bridge: &Bridge, raw: &str) -> Result<(), String> {
    let descriptors = parse(raw)?;
    sync::persist_sync_snapshot(KEY, raw, false).map_err(|e| e.to_string())?;
    bridge.shared.lock().unwrap().package_descriptors = descriptors.clone();
    remember_ids(&descriptors, &snapshot_addons(bridge));
    Ok(())
}

pub(super) fn remove(bridge: &Bridge, url: &str) {
    let Some(id) = url
        .strip_prefix("nova-provider://")
        .map(|id| id.trim_end_matches('/'))
    else {
        return;
    };
    let mut descriptors = bridge.shared.lock().unwrap().package_descriptors.clone();
    let descriptor = descriptors.get(id).cloned();
    // Membership and descriptors are separate sync domains. Applying an addon
    // tombstone must not manufacture a descriptor tombstone while another
    // domain's incoming records are still waiting to be projected.
    if !sync::applying() {
        descriptors.remove(id);
        if let Err(error) = sync::persist_package_state(&snapshot_addons(bridge), &descriptors) {
            bridge.addon_zip_import_failed(&error);
            publish(bridge);
            return;
        }
        bridge.shared.lock().unwrap().package_descriptors = descriptors;
    }
    {
        let mut state = bridge.shared.lock().unwrap();
        state.loaded_packages.remove(id);
        if let Some(descriptor) = descriptor {
            state.package_retry_at.remove(&descriptor.sha256);
            if let Some(cancel) = state.package_fetches.remove(&descriptor.sha256) {
                cancel.store(true, Ordering::Relaxed);
            }
        }
    }
    publish(bridge);
}

pub(super) fn reconcile(bridge: &Bridge) {
    let descriptors = bridge.shared.lock().unwrap().package_descriptors.clone();
    remember_ids(&descriptors, &snapshot_addons(bridge));
    let (jobs, unavailable) = {
        let mut state = bridge.shared.lock().unwrap();
        let members: HashSet<String> = state
            .installed
            .iter()
            .filter_map(|a| {
                a.url
                    .strip_prefix("nova-provider://")
                    .map(|id| id.trim_end_matches('/').to_owned())
            })
            .collect();
        state
            .loaded_packages
            .retain(|id, _| members.contains(id) && descriptors.contains_key(id));
        let loaded_ids: HashSet<_> = state.loaded_packages.keys().cloned().collect();
        let mut unavailable = Vec::new();
        for addon in &mut state.installed {
            if let Some(id) = addon.url.strip_prefix("nova-provider://")
                && addon.available
                && !loaded_ids.contains(id.trim_end_matches('/'))
            {
                addon.available = false;
                unavailable.push(addon.url.clone());
            }
        }
        let desired_hashes: HashSet<_> = descriptors
            .iter()
            .filter(|(id, _)| members.contains(*id))
            .map(|(_, d)| d.sha256.as_str())
            .collect();
        state.package_fetches.retain(|hash, cancel| {
            if desired_hashes.contains(hash.as_str()) {
                true
            } else {
                cancel.store(true, Ordering::Relaxed);
                false
            }
        });
        state
            .package_retry_at
            .retain(|hash, _| desired_hashes.contains(hash.as_str()));
        let now = std::time::Instant::now();
        let mut jobs = Vec::new();
        for (id, descriptor) in &descriptors {
            if !members.contains(id)
                || state
                    .loaded_packages
                    .get(id)
                    .is_some_and(|p| descriptor.matches(p))
                || state.package_fetches.contains_key(&descriptor.sha256)
                || state
                    .package_retry_at
                    .get(&descriptor.sha256)
                    .is_some_and(|at| *at > now)
            {
                continue;
            }
            let cancel = Arc::new(AtomicBool::new(false));
            state
                .package_fetches
                .insert(descriptor.sha256.clone(), cancel.clone());
            jobs.push((descriptor.clone(), cancel));
        }
        (jobs, unavailable)
    };
    publish(bridge);
    for url in &unavailable {
        bridge.invalidate_package_generation(url);
    }
    if !unavailable.is_empty() {
        bridge.refresh_all(true);
    }
    for (descriptor, cancel) in jobs {
        let bridge = bridge.clone();
        std::thread::spawn(move || {
            let result: Result<Arc<AddonPackage>, String> =
                load_descriptor(&descriptor).or_else(|_| {
                    let bytes = nova_sync::fetch_package(&descriptor.sha256, cancel.clone())
                        .map_err(|e| e.to_string())?;
                    let package = AddonPackage::parse(&bytes).map_err(|e| e.to_string())?;
                    if !descriptor.matches(&package) {
                        return Err("received addon package identity or hash mismatch".into());
                    }
                    if cancel.load(Ordering::Relaxed) {
                        return Err("addon package fetch cancelled".into());
                    }
                    store_archive(&package, &bytes)?;
                    Ok(package)
                });
            let _ = slint::invoke_from_event_loop(move || {
                if !current_bridge(&bridge) {
                    return;
                }
                let activate = {
                    let mut state = bridge.shared.lock().unwrap();
                    let belongs = state
                        .installed
                        .iter()
                        .any(|a| a.url == format!("nova-provider://{}", descriptor.id));
                    let desired =
                        state.package_descriptors.get(&descriptor.id) == Some(&descriptor);
                    let owns = state
                        .package_fetches
                        .get(&descriptor.sha256)
                        .is_some_and(|active| Arc::ptr_eq(active, &cancel));
                    if owns {
                        state.package_fetches.remove(&descriptor.sha256);
                    }
                    if !belongs || !desired || !owns || cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    match result {
                        Ok(package) => {
                            state.package_retry_at.remove(&descriptor.sha256);
                            if let Some(old) = state.loaded_packages.get(&descriptor.id)
                                && accepted_update(
                                    Some(&PackageDescriptor::from_package(old)),
                                    &descriptor,
                                )
                                .is_err()
                            {
                                crate::web_log(
                                    "nova: rejected addon package downgrade or conflicting version",
                                );
                                state.package_retry_at.insert(
                                    descriptor.sha256.clone(),
                                    std::time::Instant::now() + Duration::from_secs(30),
                                );
                                return;
                            }
                            let replaced = state
                                .loaded_packages
                                .get(&descriptor.id)
                                .is_some_and(|old| old.sha256() != package.sha256());
                            state
                                .loaded_packages
                                .insert(descriptor.id.clone(), package.clone());
                            Some((package, replaced))
                        }
                        Err(error) => {
                            state.package_retry_at.insert(
                                descriptor.sha256.clone(),
                                std::time::Instant::now() + Duration::from_secs(30),
                            );
                            tracing::warn!(%error, id = %descriptor.id, "addon package receipt unavailable; keeping previous package");
                            None
                        }
                    }
                };
                if let Some((package, replaced)) = activate {
                    let _guard = sync::ApplyingGuard::new();
                    if replaced {
                        bridge.invalidate_package_generation(&package.url());
                        if let Err(error) =
                            nova_providers::plugin_settings::cache_clear(package.id())
                        {
                            tracing::warn!(%error, "could not clear previous addon package cache");
                        }
                    }
                    publish(&bridge);
                    bridge.install_ok(
                        package.url(),
                        package.addon_manifest().clone(),
                        true,
                        Some(false),
                        None,
                    );
                }
            });
        });
    }
}

struct BlobStore;
impl nova_sync::PackageBlobStore for BlobStore {
    fn load(&self, hash: &str) -> nova_sync::Result<Option<Vec<u8>>> {
        if !valid_hash(hash) {
            return Ok(None);
        }
        let descriptors = read_json::<BTreeMap<String, PackageDescriptor>>(KEY).unwrap_or_default();
        let Some((id, descriptor)) = descriptors.iter().find(|(_, d)| d.sha256 == hash) else {
            return Ok(None);
        };
        if descriptor.validate(id).is_err() {
            return Ok(None);
        }
        let Ok(bytes) = read_archive(hash) else {
            return Ok(None);
        };
        let Ok(package) = AddonPackage::parse(&bytes) else {
            return Ok(None);
        };
        Ok(descriptor.matches(&package).then_some(bytes))
    }
}
pub(super) fn configure_blob_store() {
    nova_sync::configure_package_store(Arc::new(BlobStore));
}

impl Bridge {
    pub(super) fn addon_zip_import_failed(&self, message: &str) {
        tracing::warn!(%message, "addon ZIP import failed");
        if let Some(app) = self.app() {
            app.set_addon_zip_import_busy(false);
            let key = match message {
                "The addon ZIP could not be read"
                | "The addon ZIP exceeds the 8 MiB size limit"
                | "The addon file picker could not be opened" => message,
                _ => "The addon ZIP could not be installed",
            };
            app.set_addon_zip_import_status(text::tr(key).into());
        }
    }
    pub(super) fn import_addon_zip(&self, bytes: Vec<u8>) {
        if let Some(app) = self.app() {
            app.set_addon_zip_import_busy(true);
            app.set_addon_zip_import_status(text::tr("Installing addon ZIP…").into());
        }
        let generations: HashMap<String, u64> = self
            .shared
            .lock()
            .unwrap()
            .installed
            .iter()
            .map(|addon| (addon.url.clone(), addon.generation))
            .collect();
        let bridge = self.clone();
        std::thread::spawn(move || {
            let result = AddonPackage::parse(&bytes)
                .map_err(|e| e.to_string())
                .and_then(|package| {
                    store_archive(&package, &bytes)?;
                    Ok(package)
                });
            let _ = slint::invoke_from_event_loop(move || {
                if !current_bridge(&bridge) {
                    return;
                }
                let package = match result {
                    Ok(package) => package,
                    Err(error) => {
                        bridge.addon_zip_import_failed(&error);
                        return;
                    }
                };
                if let Some(generation) = generations.get(&package.url()) {
                    let still_desired =
                        bridge.shared.lock().unwrap().installed.iter().any(|addon| {
                            addon.url == package.url() && addon.generation == *generation
                        });
                    if !still_desired {
                        bridge.addon_zip_import_failed("addon membership changed during import");
                        return;
                    }
                }
                let descriptor = PackageDescriptor::from_package(&package);
                let mut descriptors = bridge.shared.lock().unwrap().package_descriptors.clone();
                let loaded = bridge
                    .shared
                    .lock()
                    .unwrap()
                    .loaded_packages
                    .get(&descriptor.id)
                    .cloned();
                let update = accepted_update(descriptors.get(&descriptor.id), &descriptor)
                    .and_then(|()| {
                        accepted_update(
                            loaded
                                .as_ref()
                                .map(|p| PackageDescriptor::from_package(p))
                                .as_ref(),
                            &descriptor,
                        )
                    });
                if let Err(error) = update {
                    bridge.addon_zip_import_failed(&error);
                    return;
                }
                let replaced = loaded.is_some_and(|old| old.sha256() != package.sha256());
                descriptors.insert(descriptor.id.clone(), descriptor.clone());
                // Reinstalling identical bytes repairs a missing archive on the
                // worker, but must not churn rows, generations or sync records.
                let already_active = {
                    let state = bridge.shared.lock().unwrap();
                    state.package_descriptors.get(&descriptor.id) == Some(&descriptor)
                        && state
                            .loaded_packages
                            .get(&descriptor.id)
                            .is_some_and(|loaded| descriptor.matches(loaded))
                        && state
                            .installed
                            .iter()
                            .any(|addon| addon.url == package.url() && addon.available)
                };
                if already_active {
                    if let Some(app) = bridge.app() {
                        app.set_addon_zip_import_busy(false);
                        app.set_addon_zip_import_status(text::tr("Addon installed").into());
                    }
                    return;
                }
                let mut addons = snapshot_addons(&bridge);
                if !addons.iter().any(|a| a.url == package.url()) {
                    addons.push(AddonStore {
                        capabilities: None,
                        url: package.url(),
                        enabled: true,
                        configure_ok: Some(false),
                        label: package.addon_manifest().name.clone(),
                    });
                }
                if let Err(error) = sync::persist_package_state(&addons, &descriptors) {
                    bridge.addon_zip_import_failed(&error);
                    return;
                }
                {
                    let mut state = bridge.shared.lock().unwrap();
                    state.package_descriptors = descriptors.clone();
                    state.package_retry_at.remove(&descriptor.sha256);
                    state
                        .loaded_packages
                        .insert(descriptor.id.clone(), package.clone());
                    if let Some(cancel) = state.package_fetches.remove(&descriptor.sha256) {
                        cancel.store(true, Ordering::Relaxed);
                    }
                }
                remember_ids(&descriptors, &addons);
                bridge.ensure_desired_addon(
                    &package.url(),
                    true,
                    Some(false),
                    Some(package.addon_manifest().name.clone()),
                );
                if replaced {
                    bridge.invalidate_package_generation(&package.url());
                    if let Err(error) = nova_providers::plugin_settings::cache_clear(package.id()) {
                        tracing::warn!(%error, "could not clear previous addon package cache");
                    }
                }
                publish(&bridge);
                bridge.install_ok(
                    package.url(),
                    package.addon_manifest().clone(),
                    true,
                    Some(false),
                    None,
                );
                if let Some(app) = bridge.app() {
                    app.set_addon_zip_import_busy(false);
                    app.set_addon_zip_import_status(text::tr("Addon installed").into());
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nova_sync::PackageBlobStore as _;
    fn descriptor(version: &str, hash: char) -> PackageDescriptor {
        PackageDescriptor {
            id: "example".into(),
            version: version.into(),
            sha256: hash.to_string().repeat(64),
        }
    }
    #[test]
    fn descriptors_reject_key_identity_and_unsafe_hashes() {
        let good = descriptor("1.2.3", 'a');
        assert!(good.validate("example").is_ok());
        assert!(good.validate("another").is_err());
        for id in ["../example", "Example", "", "example/path"] {
            let mut invalid = good.clone();
            invalid.id = id.into();
            assert!(invalid.validate(id).is_err());
        }
        for hash in [
            "a".repeat(63),
            "A".repeat(64),
            "../file".into(),
            "g".repeat(64),
        ] {
            let mut invalid = good.clone();
            invalid.sha256 = hash;
            assert!(invalid.validate("example").is_err());
        }
        assert!(descriptor("1.2", 'a').validate("example").is_err());
    }
    #[test]
    fn update_acceptance_preserves_version_to_bytes_identity() {
        let old = descriptor("2.0.0", 'a');
        assert!(accepted_update(Some(&old), &old).is_ok());
        assert!(accepted_update(Some(&old), &descriptor("1.9.9", 'b')).is_err());
        assert!(accepted_update(Some(&old), &descriptor("2.0.0", 'b')).is_err());
        assert!(accepted_update(Some(&old), &descriptor("2.1.0", 'b')).is_ok());
        assert!(accepted_update(Some(&old), &descriptor("2.0.0+repacked", 'b')).is_err());
        assert!(accepted_update(Some(&old), &descriptor("2.0.0-beta.1", 'b')).is_err());
        assert!(accepted_update(None, &old).is_ok());
    }
    #[test]
    fn descriptor_records_round_trip_and_deletions_remove_records() {
        let original = BTreeMap::from([("example".to_owned(), descriptor("1.0.0", 'a'))]);
        let raw = serde_json::to_string(&original).unwrap();
        let records = records(&raw).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].0, "example");
        assert_eq!(
            serde_json::from_str::<PackageDescriptor>(&records[0].1).unwrap(),
            original["example"]
        );
        assert!(super::records("{}").unwrap().is_empty());
        assert!(super::records(&raw.replace("sha256", "futureHash")).is_err());
        assert!(super::records(&raw.replace("1.0.0", "invalid")).is_err());
    }

    fn fixture_zip(id: &str, version: &str, revision: &str) -> Vec<u8> {
        use zip::{ZipWriter, write::SimpleFileOptions};
        let plugin = serde_json::json!({
            "formatVersion": 1, "id": id, "name": id, "version": version,
            "entry": "index.js", "permissions": {},
            "capabilities": {"configurableCapabilities": true}
        });
        let addon = serde_json::json!({"id": format!("nova.{id}"), "name": id, "version": version});
        let mut archive = ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, bytes) in [
            ("manifest.json", serde_json::to_vec(&plugin).unwrap()),
            ("stremio-manifest.json", serde_json::to_vec(&addon).unwrap()),
            ("LICENSE", b"fixture license".to_vec()),
            ("NOTICE.md", b"fixture notice".to_vec()),
            ("index.js", format!("globalThis.novaProvider = {{handle(request) {{return request.manifest || [];}}}}; // {revision}").into_bytes()),
        ] {
            archive.start_file(name, SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)).unwrap();
            archive.write_all(&bytes).unwrap();
        }
        archive.finish().unwrap().into_inner()
    }

    async fn event_loop_delay() {
        let ready = Rc::new(std::cell::Cell::new(false));
        let waker = Rc::new(std::cell::RefCell::new(None::<std::task::Waker>));
        let timer_ready = ready.clone();
        let timer_waker = waker.clone();
        slint::Timer::single_shot(Duration::from_millis(10), move || {
            timer_ready.set(true);
            if let Some(waker) = timer_waker.borrow_mut().take() {
                waker.wake();
            }
        });
        std::future::poll_fn(|cx| {
            if ready.get() {
                std::task::Poll::Ready(())
            } else {
                *waker.borrow_mut() = Some(cx.waker().clone());
                std::task::Poll::Pending
            }
        })
        .await;
    }
    async fn import_and_wait(bridge: &Bridge, bytes: Vec<u8>) {
        bridge.import_addon_zip(bytes);
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        while bridge.app().unwrap().get_addon_zip_import_busy() {
            assert!(
                std::time::Instant::now() < deadline,
                "addon import worker timed out"
            );
            event_loop_delay().await;
        }
    }

    #[test]
    fn package_import_preserves_state_and_repairs_identical_archives() {
        // Slint, storage and package registries are process globals. Keep the
        // production worker/event-loop path isolated from other unit fixtures.
        const TEST_DIR: &str = "NOVA_ADDON_PACKAGE_IMPORT_TEST_DIR";
        let Some(root) = std::env::var_os(TEST_DIR) else {
            let root = std::env::temp_dir().join(format!(
                "nova-addon-package-test-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(TEST_DIR, &root)
                .env("XDG_DATA_HOME", root.join("data"))
                .env("XDG_CACHE_HOME", root.join("cache"))
                .env("LOCALAPPDATA", root.join("data"))
                .env("NOVA_DATA_DIR", root.join("data").join("nova"))
                .args(["--exact", "app::addon_packages::tests::package_import_preserves_state_and_repairs_identical_archives", "--nocapture"])
                .output().unwrap();
            let _ = fs::remove_dir_all(root);
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        let root = PathBuf::from(root);
        i_slint_backend_testing::init_integration_test_with_system_time();
        storage::init_at(&root.join("storage"));
        nova_providers::plugin_settings::configure_plugin_settings(Arc::new(
            plugin_settings::Backend,
        ))
        .unwrap();
        let app = AppWindow::new().unwrap();
        app.global::<crate::Anim>().set_enabled(false);
        let player = crate::player::Player::setup(&app);
        let downloads = DownloadCoordinator::new(root.join("downloads"));
        #[cfg(feature = "desktop")]
        let bridge = {
            let (hi, _) = mpsc::channel();
            let (lo, _) = mpsc::channel();
            Bridge::new(
                app.as_weak(),
                PosterTx { hi, lo },
                Arc::new(AtomicU64::new(0)),
                player,
                downloads,
            )
        };
        #[cfg(not(feature = "desktop"))]
        let bridge = Bridge::new(
            app.as_weak(),
            Arc::new(AtomicU64::new(0)),
            player,
            downloads,
        );
        super::super::bridge::install_global_bridge(&bridge);
        initialize(&bridge);
        app.show().unwrap();
        let completed = Rc::new(std::cell::Cell::new(false));
        let completed2 = completed.clone();
        slint::spawn_local(async move {
            let first = fixture_zip("example", "1.0.0", "first");
            import_and_wait(&bridge, first.clone()).await;
            let first_package = AddonPackage::parse(&first).unwrap();
            assert_eq!(bridge.shared.lock().unwrap().installed.len(), 1);
            assert!(bridge.shared.lock().unwrap().installed[0].enabled);
            assert!(archive_path(first_package.sha256()).unwrap().is_file());
            assert_eq!(
                read_json::<BTreeMap<String, PackageDescriptor>>(KEY).unwrap()["example"].sha256,
                first_package.sha256()
            );
            assert!(BlobStore.load(first_package.sha256()).unwrap().is_some());

            import_and_wait(&bridge, fixture_zip("second", "1.0.0", "other")).await;
            bridge.move_addon(0, 1);
            bridge.toggle_addon(1);
            let capabilities = nova_providers::ProviderCapabilities {
                streams: false,
                metadata: true,
            };
            nova_providers::configure_provider_capabilities(&first_package.url(), capabilities);
            bridge.persist_installed();
            nova_providers::plugin_settings::set_setting(
                "example",
                "token",
                serde_json::json!("preserved"),
            )
            .unwrap();
            let stored_settings = storage::try_get_str(plugin_settings::KEY).unwrap();
            let rows = storage::try_get_str("addons").unwrap();
            let descriptors = storage::try_get_str(KEY).unwrap();
            let generation = bridge.shared.lock().unwrap().installed[1].generation;
            import_and_wait(&bridge, first.clone()).await;
            assert_eq!(storage::try_get_str("addons").unwrap(), rows);
            assert_eq!(storage::try_get_str(KEY).unwrap(), descriptors);
            assert_eq!(
                bridge.shared.lock().unwrap().installed[1].generation,
                generation
            );

            for rejected in [
                fixture_zip("example", "1.0.0", "conflicting bytes"),
                fixture_zip("example", "0.9.0", "downgrade"),
                b"not a ZIP".to_vec(),
            ] {
                import_and_wait(&bridge, rejected).await;
                assert_eq!(storage::try_get_str("addons").unwrap(), rows);
                assert_eq!(storage::try_get_str(KEY).unwrap(), descriptors);
                assert_eq!(
                    bridge.shared.lock().unwrap().loaded_packages["example"].sha256(),
                    first_package.sha256()
                );
                assert_eq!(
                    app.get_addon_zip_import_status().as_str(),
                    "The addon ZIP could not be installed"
                );
            }
            let newer = fixture_zip("example", "2.0.0", "new version");
            import_and_wait(&bridge, newer.clone()).await;
            let newer_package = AddonPackage::parse(&newer).unwrap();
            {
                let state = bridge.shared.lock().unwrap();
                assert_eq!(state.installed[0].url, "nova-provider://second");
                assert_eq!(state.installed[1].url, "nova-provider://example");
                assert!(!state.installed[1].enabled);
                assert_ne!(state.installed[1].generation, generation);
                assert_eq!(
                    state.loaded_packages["example"].sha256(),
                    newer_package.sha256()
                );
            }
            assert_eq!(
                nova_providers::provider_capabilities(&newer_package.url()),
                Some(capabilities)
            );
            assert_eq!(
                storage::try_get_str(plugin_settings::KEY).unwrap(),
                stored_settings
            );
            let new_generation = bridge.shared.lock().unwrap().installed[1].generation;
            fs::remove_file(archive_path(newer_package.sha256()).unwrap()).unwrap();
            import_and_wait(&bridge, newer.clone()).await;
            assert_eq!(read_archive(newer_package.sha256()).unwrap(), newer);
            assert_eq!(
                bridge.shared.lock().unwrap().installed[1].generation,
                new_generation
            );
            import_and_wait(&bridge, first).await;
            assert_eq!(
                bridge.shared.lock().unwrap().loaded_packages["example"].sha256(),
                newer_package.sha256()
            );

            // Removal supersedes a local import already parsing on a worker.
            // No event-loop yield occurs until after removal, so even an early
            // worker completion cannot commit its queued callback first.
            bridge.import_addon_zip(fixture_zip("example", "3.0.0", "stale local import"));
            bridge.remove_addon_at(1);
            let deadline = std::time::Instant::now() + Duration::from_secs(15);
            while app.get_addon_zip_import_busy() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "stale local import timed out"
                );
                event_loop_delay().await;
            }
            assert!(
                !bridge
                    .shared
                    .lock()
                    .unwrap()
                    .installed
                    .iter()
                    .any(|addon| addon.url == newer_package.url())
            );
            assert!(!nova_providers::has_installed_provider(
                &newer_package.url()
            ));
            // An explicit new import after removal is allowed to add membership.
            import_and_wait(&bridge, newer.clone()).await;
            assert_eq!(
                bridge.shared.lock().unwrap().installed[1].url,
                newer_package.url()
            );

            // Package selection can disappear before addon membership projects.
            // Such a row stays desired but cannot advertise unavailable code.
            {
                let _guard = sync::ApplyingGuard::new();
                project(&bridge, "{}").unwrap();
                reconcile(&bridge);
            }
            assert!(!bridge.shared.lock().unwrap().installed[1].available);
            assert!(!nova_providers::has_installed_provider(
                &newer_package.url()
            ));
            import_and_wait(&bridge, newer.clone()).await;
            assert!(bridge.shared.lock().unwrap().installed[1].available);

            // An already queued receipt must not revive removed membership.
            let future = fixture_zip("example", "3.0.0", "pending update");
            let future_package = AddonPackage::parse(&future).unwrap();
            store_archive(&future_package, &future).unwrap();
            let mut desired = bridge.shared.lock().unwrap().package_descriptors.clone();
            desired.insert(
                "example".into(),
                PackageDescriptor::from_package(&future_package),
            );
            sync::persist_package_state(&snapshot_addons(&bridge), &desired).unwrap();
            bridge.shared.lock().unwrap().package_descriptors = desired;
            reconcile(&bridge);
            let cancelled_receipt =
                bridge.shared.lock().unwrap().package_fetches[future_package.sha256()].clone();
            bridge.remove_addon_at(1);
            let deadline = std::time::Instant::now() + Duration::from_secs(15);
            while Arc::strong_count(&cancelled_receipt) > 1 {
                assert!(
                    std::time::Instant::now() < deadline,
                    "cancelled receipt callback timed out"
                );
                event_loop_delay().await;
            }
            assert!(
                !bridge
                    .shared
                    .lock()
                    .unwrap()
                    .installed
                    .iter()
                    .any(|a| a.url == newer_package.url())
            );
            assert!(
                !bridge
                    .shared
                    .lock()
                    .unwrap()
                    .loaded_packages
                    .contains_key("example")
            );
            assert!(!nova_providers::has_installed_provider(
                &newer_package.url()
            ));
            assert!(BlobStore.load(future_package.sha256()).unwrap().is_none());
            assert_eq!(
                storage::try_get_str(plugin_settings::KEY).unwrap(),
                stored_settings
            );
            completed2.set(true);
            slint::quit_event_loop().unwrap();
        })
        .unwrap();
        slint::run_event_loop().unwrap();
        assert!(completed.get());
    }
}
