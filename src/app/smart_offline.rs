//! Personal offline intent, device opt-in and bounded background reconciliation.
//! Durable workers own ProfileStore; UI continuations never activate a profile.
use super::*;
use crate::download::offline::{DevicePolicy, Environment, GIB, Ledger, Owner, PauseReason};
use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;

pub(super) const KEY: &str = "offline_policies:v1";
pub(super) const DOMAIN: &str = "offline_policies";
pub(super) const DEVICE_KEY: &str = "smart_downloads:device:v1";
const MAX_POLICIES: usize = 100;
static ENVIRONMENT: Mutex<Environment> = Mutex::new(Environment {
    connected: false,
    wifi_or_ethernet: false,
    unmetered: false,
    charging: false,
});
static DEVICE_SERIAL: Mutex<()> = Mutex::new(());
static RUNNING: AtomicBool = AtomicBool::new(false);
static NEXT_CHECK: AtomicU64 = AtomicU64::new(0);
static LAST_CHECK: AtomicU64 = AtomicU64::new(0);
static DIRTY: AtomicBool = AtomicBool::new(true);
static NOTICES: LazyLock<Mutex<BTreeMap<String, String>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct Policy {
    pub version: u32,
    pub media_type: String,
    pub title_id: String,
    pub title: String,
    pub enabled: bool,
    pub keep_count: u8,
    pub include_specials: bool,
    pub preference: Option<StreamPreference>,
    pub source_owner: String,
    pub cleanup_watched: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            version: 1,
            media_type: String::new(),
            title_id: String::new(),
            title: String::new(),
            enabled: true,
            keep_count: 3,
            include_specials: false,
            preference: None,
            source_owner: String::new(),
            cleanup_watched: true,
            extra: BTreeMap::new(),
        }
    }
}
pub(super) fn read_policies(
    owner: &profiles::ProfileStore,
) -> Result<BTreeMap<String, Policy>, String> {
    let map: BTreeMap<String, Policy> = owner
        .read(KEY)
        .map_err(|e| e.to_string())?
        .map(|raw| serde_json::from_str(&raw))
        .transpose()
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    if map.len() > MAX_POLICIES
        || map.iter().any(|(key, p)| {
            p.version != 1
                || p.media_type.is_empty()
                || p.media_type == "movie"
                || p.title_id.is_empty()
                || !(1..=10).contains(&p.keep_count)
                || *key != policy_key(&p.media_type, &p.title_id)
        })
    {
        return Err("invalid offline policies; saved data retained".into());
    }
    Ok(map)
}
fn policy_key(media_type: &str, id: &str) -> String {
    serde_json::to_string(&(media_type, id)).expect("typed policy identity")
}
pub(super) fn enabled_title(owner: &profiles::ProfileStore, media_type: &str, id: &str) -> bool {
    read_policies(owner).is_ok_and(|policies| {
        policies
            .get(&policy_key(media_type, id))
            .is_some_and(|p| p.enabled)
    })
}
pub(super) fn records(raw: &str) -> Result<Vec<(String, String, u64)>, serde_json::Error> {
    let map: BTreeMap<String, serde_json::Value> = serde_json::from_str(raw)?;
    map.into_iter()
        .map(|(key, value)| Ok((key, serde_json::to_string(&value)?, 0)))
        .collect()
}
pub(super) fn read_device() -> Result<DevicePolicy, String> {
    nova_storage::try_get_str(DEVICE_KEY)
        .map_err(|e| e.to_string())?
        .map(|raw| serde_json::from_str(&raw).map_err(|e| e.to_string()))
        .transpose()
        .map(Option::unwrap_or_default)
}
fn save_device(device: &DevicePolicy) -> Result<(), String> {
    nova_storage::try_set_str(
        DEVICE_KEY,
        &serde_json::to_string(device).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    trigger();
    #[cfg(target_os = "android")]
    super::smart_offline_android::schedule(device);
    Ok(())
}
pub(super) fn environment() -> Environment {
    *ENVIRONMENT.lock().unwrap()
}
pub(super) fn set_environment(environment: Environment) {
    let mut old = ENVIRONMENT.lock().unwrap();
    if *old != environment {
        *old = environment;
        trigger();
    }
}
pub(super) fn trigger() {
    DIRTY.store(true, Ordering::Release);
}
fn owner_for(owner: &Owner) -> profiles::ProfileStore {
    profiles::ProfileStore::for_profile(&owner.profile)
}
fn owner_enabled(owner: &Owner) -> bool {
    read_device().is_ok_and(|d| d.enabled_profiles.contains(&owner.profile))
}
fn policy_for(owner: &Owner) -> Option<Policy> {
    if !profiles::valid_id(&owner.profile) {
        return None;
    }
    read_policies(&owner_for(owner)).ok()?.remove(&owner.policy)
}
fn desired(policy: &Policy, owner: &profiles::ProfileStore) -> Vec<Video> {
    let videos = owner
        .run(|| read_episodes_cache_for(&policy.media_type, &policy.title_id))
        .unwrap_or_default();
    let progress: HashMap<String, EpisodeProgress> = owner
        .read("episode_progress")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    frontier(
        &videos,
        &progress,
        &policy.title_id,
        policy.keep_count,
        policy.include_specials,
        today_days(),
    )
}
fn frontier(
    videos: &[Video],
    progress: &HashMap<String, EpisodeProgress>,
    title: &str,
    count: u8,
    specials: bool,
    today: i64,
) -> Vec<Video> {
    let mut eligible: Vec<_> = videos
        .iter()
        .filter(|v| {
            !v.id.is_empty()
                && v.episode_number().is_some_and(|e| e > 0)
                && (specials || v.season.is_some_and(|s| s > 0))
                && episode_has_air_date(v)
                && episode_is_out(v, today)
                && !progress
                    .get(&progress_map_key(title, &v.id))
                    .is_some_and(|p| p.watched)
        })
        .cloned()
        .collect();
    eligible.sort_by_key(|v| {
        (
            v.season.unwrap_or(0),
            v.episode_number().unwrap_or(0),
            v.id.clone(),
        )
    });
    // Unfinished resume targets lead; retain native IDs rather than inferring
    // aliases from coincident season/episode labels across providers.
    if let Some((index, _)) = eligible
        .iter()
        .enumerate()
        .filter_map(|(i, v)| {
            progress
                .get(&progress_map_key(title, &v.id))
                .filter(|p| p.position_secs.is_finite() && p.position_secs > 0.0)
                .map(|p| (i, p.updated_at_secs))
        })
        .max_by_key(|(_, at)| *at)
    {
        let resume = eligible.remove(index);
        eligible.insert(0, resume);
    }
    eligible.truncate(usize::from(count));
    eligible
}
fn frontier_signature(policy: &Policy, owner: &profiles::ProfileStore) -> String {
    serde_json::to_string(
        &desired(policy, owner)
            .iter()
            .map(|v| &v.id)
            .collect::<Vec<_>>(),
    )
    .expect("frontier")
}
pub(super) fn owner_wants(
    owner: &Owner,
    media_type: &str,
    title: &str,
    episode: &str,
    source: &str,
) -> bool {
    if !profiles::valid_id(&owner.profile) || !owner_enabled(owner) {
        return false;
    }
    let store = owner_for(owner);
    if store.retired().unwrap_or(true)
        || !profile_policy::allows_title(&store, media_type, title)
        || !profile_policy::allows_addon(&store, source)
    {
        return false;
    }
    let Some(policy) = policy_for(owner) else {
        return false;
    };
    policy.enabled
        && policy.media_type == media_type
        && policy.title_id == title
        && policy.source_owner == source
        && desired(&policy, &store).iter().any(|v| v.id == episode)
}
fn source_revision(policy: &Policy, store: &profiles::ProfileStore) -> String {
    preference_hash(
        "offline-source",
        &serde_json::to_string(&(
            &policy.media_type,
            &policy.title_id,
            &policy.source_owner,
            &policy.preference,
            store.read("settings").ok(),
            store.read(plugin_settings::KEY).ok(),
            store.read("addons").ok(),
            store.read(addon_packages::KEY).ok(),
        ))
        .unwrap(),
    )
}
pub(super) fn source_revision_matches(owner: &Owner, revision: &str) -> bool {
    policy_for(owner).is_some_and(|policy| source_revision(&policy, &owner_for(owner)) == revision)
}
pub(super) fn removed_rule_keeps_file(owner: &Owner) -> bool {
    if !profiles::valid_id(&owner.profile) {
        return false;
    }
    let store = owner_for(owner);
    !store.retired().unwrap_or(true)
        && read_policies(&store).is_ok_and(|policies| !policies.contains_key(&owner.policy))
}
pub(super) fn job_pause(job: &DownloadJob) -> Option<PauseReason> {
    if !job.offline.automatic() {
        return None;
    }
    let Ok(device) = read_device() else {
        return Some(PauseReason::Disabled);
    };
    if !job.offline.owners.iter().any(|owner| {
        owner_wants(
            owner,
            &job.media_type,
            &job.media_id,
            &job.request_id,
            &job.offline.source_owner,
        )
    }) {
        return Some(PauseReason::Disabled);
    }
    if !job
        .offline
        .owners
        .iter()
        .any(|owner| source_revision_matches(owner, &job.offline.source_revision))
    {
        return Some(PauseReason::NeedsSource);
    }
    device.environmental_pause(environment())
}
pub(super) fn allows_download(owner: &profiles::ProfileStore, job: &DownloadJob) -> bool {
    profile_policy::allows_title(owner, &job.media_type, &job.media_id)
        && profile_policy::allows_addon(
            owner,
            if job.offline.source_owner.is_empty() {
                "local"
            } else {
                &job.offline.source_owner
            },
        )
}
pub(super) fn suppress_removed_job(job: &DownloadJob) {
    let _serial = device_guard();
    let Ok(mut device) = read_device() else {
        return;
    };
    for owner in &job.offline.owners {
        if let Some(policy) = policy_for(owner) {
            device.suppressed.insert(
                serde_json::to_string(&(owner, &job.request_id)).unwrap(),
                frontier_signature(&policy, &owner_for(owner)),
            );
        }
    }
    let _ = save_device(&device);
}
fn suppressed(device: &DevicePolicy, owner: &Owner, episode: &str, signature: &str) -> bool {
    device
        .suppressed
        .get(&serde_json::to_string(&(owner, episode)).unwrap())
        .is_some_and(|saved| saved == signature)
}
fn notice(owner: &Owner, message: &str) {
    NOTICES
        .lock()
        .unwrap()
        .insert(serde_json::to_string(owner).unwrap(), message.into());
}
pub(super) fn pause_text(reason: PauseReason) -> &'static str {
    match reason {
        PauseReason::Network => "Waiting for unmetered Wi-Fi or Ethernet",
        PauseReason::Charging => "Waiting for charging",
        PauseReason::Budget => "Automatic storage allowance reached",
        PauseReason::FreeSpace => "Preserving free storage reserve",
        PauseReason::Disabled => "Execution disabled on this device",
        PauseReason::NeedsSource => "Size unknown — choose Download manually",
        PauseReason::User => "Paused by you",
        PauseReason::Background => "Pending background opportunity",
    }
}

/// One bounded scan for UI startup, state changes and Android job windows.
/// All profiles retain separate durable authority; the coordinator remains shared.
pub(super) fn reconcile(
    coordinator: &DownloadCoordinator,
    cancel: &AtomicBool,
    playing: Option<&str>,
) -> Result<(), String> {
    let mut device = read_device()?;
    let mut wanted = HashSet::new();
    let mut work = Vec::new();
    let profiles = profiles::list()?;
    let valid = profiles
        .iter()
        .map(|p| p.id.as_str())
        .collect::<HashSet<_>>();
    if device
        .enabled_profiles
        .iter()
        .any(|id| !valid.contains(id.as_str()))
    {
        let _serial = device_guard();
        let mut latest = read_device()?;
        latest
            .enabled_profiles
            .retain(|id| valid.contains(id.as_str()));
        save_device(&latest)?;
        device = latest;
    }
    for profile in profiles {
        let store = profiles::ProfileStore::for_profile(&profile.id);
        for (key, policy) in read_policies(&store)?.into_iter().take(MAX_POLICIES) {
            let owner = Owner {
                profile: profile.id.clone(),
                policy: key,
            };
            let videos = desired(&policy, &store);
            for video in &videos {
                wanted.insert((owner.clone(), video.id.clone()));
            }
            let paused = !policy.enabled || !device.enabled_profiles.contains(&profile.id);
            if paused || !policy.cleanup_watched {
                for job in coordinator.jobs().iter().filter(|job| {
                    (paused || job.phase == crate::download::DownloadPhase::Completed)
                        && job.offline.owners.contains(&owner)
                }) {
                    wanted.insert((owner.clone(), job.request_id.clone()));
                }
            }
            if policy.enabled
                && device.enabled_profiles.contains(&profile.id)
                && profile_policy::allows_title(&store, &policy.media_type, &policy.title_id)
            {
                work.push((store.clone(), owner, policy, videos));
            }
        }
    }
    coordinator.reconcile_smart_owners(&wanted, playing, true);
    let mut lookups = 0;
    for (store, owner, policy, videos) in work {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        if let Some(reason) = device.environmental_pause(environment()) {
            notice(&owner, pause_text(reason));
            continue;
        }
        if policy.preference.is_none() || policy.source_owner.is_empty() {
            notice(&owner, "Choose a source for smart downloads");
            continue;
        }
        let signature = frontier_signature(&policy, &store);
        let fingerprint = serde_json::to_string(&(
            &policy,
            &signature,
            store.read("addons").ok(),
            store.read("settings").ok(),
            store.read(plugin_settings::KEY).ok(),
        ))
        .unwrap();
        for video in videos {
            if cancel.load(Ordering::Acquire) || lookups >= 4 {
                break;
            }
            if suppressed(&device, &owner, &video.id, &signature) {
                notice(&owner, "Removed manually — use Restore offline");
                continue;
            }
            let jobs = coordinator.jobs();
            if let Some(job) = jobs.iter().find(|j| {
                j.media_type == policy.media_type
                    && j.media_id == policy.title_id
                    && j.request_id == video.id
                    && allows_download(&store, j)
            }) {
                // A sibling/manual artifact satisfies a slot; no second copy.
                if job.phase != crate::download::DownloadPhase::Failed {
                    coordinator.attach_smart_owner(&job.id, owner.clone());
                    continue;
                }
                if job.offline.retry_at.is_none_or(|at| at > now_secs()) {
                    notice(&owner, "Download failed — Retry or choose a source");
                    continue;
                }
            }
            let receipt_key = serde_json::to_string(&(&owner, &video.id)).unwrap();
            if let Some(receipt) = device.receipts.get(&receipt_key)
                && receipt.blocked(&fingerprint, now_secs())
            {
                notice(&owner, &receipt.message);
                continue;
            }
            lookups += 1;
            match lookup(&store, &policy, &video, cancel) {
                Ok((request, resolver, bytes)) => {
                    #[cfg(target_os = "android")]
                    if matches!(
                        request.source,
                        crate::download::DownloadSource::Torrent { .. }
                    ) && crate::torrent::engine().is_none()
                    {
                        let settings = read_torrent_settings();
                        if !settings.enabled {
                            notice(&owner, "P2P downloads are disabled or unavailable.");
                            continue;
                        }
                        *CURRENT_TORRENT_SETTINGS.lock().unwrap() = settings.clone();
                        sync_torrent_engine(&settings);
                    }
                    match coordinator.smart_admit(request, resolver, owner.clone(), bytes) {
                        Ok(_) => notice(&owner, "Smart downloads ready"),
                        Err(reason) => notice(&owner, pause_text(reason)),
                    }
                }
                Err(error) => {
                    notice(&owner, &error);
                    let _serial = device_guard();
                    if let Ok(mut latest) = read_device() {
                        let receipt = latest.receipts.entry(receipt_key).or_default();
                        if receipt.fingerprint != fingerprint {
                            *receipt = Default::default();
                            receipt.fingerprint = fingerprint.clone();
                        }
                        let transient = ![
                            "Choose",
                            "choose",
                            "Size unknown",
                            "No matching",
                            "not allowed",
                            "Source removed",
                        ]
                        .iter()
                        .any(|label| error.contains(label));
                        receipt.message = error;
                        receipt.failure(now_secs(), transient);
                        let _ = save_device(&latest);
                    }
                }
            }
        }
    }
    Ok(())
}
fn lookup(
    store: &profiles::ProfileStore,
    policy: &Policy,
    video: &Video,
    cancel: &AtomicBool,
) -> Result<
    (
        downloads::NewDownload,
        Option<downloads::ResolverDownload>,
        Option<u64>,
    ),
    String,
> {
    let runtime = headless_profile::Runtime::new(store.clone())
        .map_err(|_| "Offline settings unavailable")?;
    let addon = runtime
        .addons
        .iter()
        .find(|a| a.enabled && a.url == policy.source_owner)
        .ok_or("Source removed — choose another source")?;
    if !profile_policy::allows_addon(store, &addon.url) {
        return Err("Source is not allowed".into());
    }
    let bytes = runtime
        .fetch_cancelled(
            Addon::new(&addon.url)
                .map_err(|e| e.to_string())?
                .stream_url(&policy.media_type, &video.id),
            cancel,
        )
        .map_err(|_| "Source lookup failed — retry later")?;
    if cancel.load(Ordering::Acquire) {
        return Err("Pending background opportunity".into());
    }
    let rows = Addon::parse_streams(&bytes)
        .map_err(|e| e.to_string())?
        .into_iter()
        .take(80)
        .map(|stream| {
            let source = runtime.inventory.with(|| {
                runtime.settings.with(|| {
                    stream_source_for_addon(
                        &stream,
                        &addon.url,
                        0,
                        nova_providers::plugin_settings::settings_generation(),
                    )
                })
            });
            StreamUi {
                id: format!(
                    "0:{}",
                    stream_choice_identity(&addon.url, &policy.media_type, &video.id, &stream)
                ),
                choice_id: None,
                owner_url: addon.url.clone(),
                display: stream_display(&stream),
                card: stream_card_text(&stream),
                source,
                addon: addon.label.clone(),
                download: None,
            }
        })
        .collect::<Vec<_>>();
    let settings: CacheSettings = store
        .read("settings")
        .map_err(|e| e.to_string())?
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(settings::recover_settings)
        .unwrap_or_default();
    let mut rows = rows;
    rows.retain(|row| {
        policy
            .preference
            .as_ref()
            .is_some_and(|preference| preference.offline_matches(row))
    });
    stream_filters::apply_stream_rules(&mut rows, &stream_filters::effective_rules(&settings));
    let index = recommended_stream(
        &rows,
        None,
        policy
            .preference
            .as_ref()
            .ok_or("Choose a source for smart downloads")?,
    )
    .ok_or("No matching release — choose another source")?;
    let row = &rows[index];
    let mut estimate = row.card.listing_bytes;
    let (source, resolver) = match &row.source {
        StreamSource::Url(url) => (crate::download::DownloadSource::http(url), None),
        StreamSource::UrlWithOptions { url, headers, .. } => (
            crate::download::DownloadSource::http_with_headers(url, headers.clone()),
            None,
        ),
        StreamSource::Resolver {
            provider_url,
            token,
            settings_generation,
            ..
        } => {
            let capability = runtime
                .inventory
                .with(|| {
                    runtime.settings.with(|| {
                        nova_providers::capture_resolver_capability(
                            provider_url,
                            *settings_generation,
                        )
                    })
                })
                .map_err(|e| e.to_string())?;
            let retained = runtime.clone();
            let authority = capability.clone();
            let store = store.clone();
            let policy = policy.clone();
            let current = Arc::new(move || {
                retained.current()
                    && profile_policy::allows_title(&store, &policy.media_type, &policy.title_id)
                    && profile_policy::allows_addon(&store, &policy.source_owner)
            });
            let resolver = downloads::ResolverDownload::for_offline(
                token.clone(),
                move |token, timeout, cancelled| authority.resolve(token, timeout, cancelled),
                current,
            );
            (
                crate::download::DownloadSource::http(format!("nova-resolver:{}", row.id)),
                Some(resolver),
            )
        }
        // Exact selected-file evidence plus the shared storage middleware
        // bounds every bundle write, including adjacent piece files.
        StreamSource::Torrent {
            info_hash,
            file_idx: Some(file_idx),
        } => (
            crate::download::DownloadSource::torrent(info_hash, Some(*file_idx)),
            None,
        ),
        StreamSource::Torrent { file_idx: None, .. } => {
            return Err("Choose an exact torrent file for smart downloads".into());
        }
        _ => return Err("Choose a downloadable source".into()),
    };
    if estimate.is_none() && resolver.is_none() {
        estimate = probe_size(&source);
    }
    if estimate.is_none() {
        return Err("Size unknown — choose Download manually".into());
    }
    if !runtime.current() {
        return Err("Source changed — choose another source".into());
    }
    Ok((
        downloads::NewDownload {
            media_type: policy.media_type.clone(),
            media_id: policy.title_id.clone(),
            request_id: video.id.clone(),
            title: policy.title.clone(),
            year: String::new(),
            display: row.display.clone(),
            addon: row.addon.clone(),
            source,
            source_owner: policy.source_owner.clone(),
            source_revision: source_revision(policy, store),
        },
        resolver,
        estimate,
    ))
}

/// A HEAD total bounds a single resource, never an HLS manifest. Provider
/// estimates are backed by the worker's strict reservation cutoff.
fn probe_size(source: &crate::download::DownloadSource) -> Option<u64> {
    crate::download::probe_http_size(source.url()?, source.headers().unwrap_or_default())
}

impl Bridge {
    pub(super) fn setup_smart_offline(&self) {
        let Some(app) = self.app() else { return };
        let global = app.global::<nova_ui_shared::OfflineBridge>();
        let b = self.clone();
        global.on_closed(move || {
            if let Some(app) = b.app() {
                app.global::<nova_ui_shared::OfflineBridge>()
                    .set_opened(false);
            }
            b.close_profile_management();
        });
        let b = self.clone();
        global.on_open_requested(move || {
            if b.request_offline_management() {
                if let Some(app) = b.app() {
                    app.global::<nova_ui_shared::OfflineBridge>()
                        .set_opened(true);
                }
                b.refresh_smart_offline();
            }
        });
        let b = self.clone();
        global.on_refresh(move || b.refresh_smart_offline());
        let b = self.clone();
        global.on_detail_toggle(move || b.toggle_detail_offline());
        let b = self.clone();
        global.on_device_toggle(move || {
            b.edit_offline_device(|d| {
                let id = profiles::current_id();
                if !d.enabled_profiles.remove(&id) {
                    d.enabled_profiles.insert(id);
                }
            })
        });
        let b = self.clone();
        global.on_budget(move |delta| {
            b.edit_offline_device(|d| {
                d.allowance_bytes = ((d.allowance_bytes / GIB) as i64 + i64::from(delta))
                    .clamp(1, i64::from(i32::MAX)) as u64
                    * GIB
            })
        });
        let b = self.clone();
        global.on_profile_toggle(move |id| {
            if profiles::valid_id(&id) {
                b.edit_offline_device(|d| {
                    if !d.enabled_profiles.remove(id.as_str()) {
                        d.enabled_profiles.insert(id.into());
                    }
                });
            }
        });
        let b = self.clone();
        global.on_charging_toggle(move || {
            b.edit_offline_device(|d| d.charging_only = !d.charging_only)
        });
        let b = self.clone();
        global.on_action(move |key, action| b.offline_action(&key, action));
        self.refresh_smart_offline();
        #[cfg(not(target_os = "android"))]
        start_environment_monitor();
        #[cfg(target_os = "android")]
        if let Ok(device) = read_device() {
            super::smart_offline_android::schedule(&device);
        }
    }
    fn edit_offline_device(&self, edit: impl FnOnce(&mut DevicePolicy)) {
        if !self.require_profile_management() {
            return;
        }
        let _serial = device_guard();
        if let Ok(mut device) = read_device() {
            edit(&mut device);
            if let Err(error) = save_device(&device) {
                tracing::warn!(%error,"offline settings not saved");
            }
        }
        self.refresh_smart_offline();
    }
    pub(super) fn toggle_detail_offline(&self) {
        let owner = profiles::ProfileStore::capture();
        if !self.request_offline_detail_management() {
            return;
        }
        let Some((media_type, id, title)) = self
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_ref()
            .filter(|m| m.type_ != "movie")
            .map(|m| (m.type_.clone(), m.id.clone(), m.name.clone()))
        else {
            return;
        };
        let Ok(mut policies) = read_policies(&owner) else {
            return;
        };
        let key = policy_key(&media_type, &id);
        if let Some(policy) = policies.get_mut(&key) {
            policy.enabled = !policy.enabled;
        } else {
            policies.insert(
                key,
                Policy {
                    media_type,
                    title_id: id,
                    title,
                    ..Default::default()
                },
            );
        }
        if let Ok(raw) = serde_json::to_string(&policies) {
            let _ = owner.persist_snapshot(KEY, &raw, false);
        }
        trigger();
        self.refresh_smart_offline();
    }
    fn offline_action(&self, key: &str, action: i32) {
        let owner = profiles::ProfileStore::capture();
        if !self.require_profile_management() {
            return;
        }
        let Ok(mut policies) = read_policies(&owner) else {
            return;
        };
        let Some(policy) = policies.get_mut(key) else {
            return;
        };
        let playing = self
            .shared
            .lock()
            .unwrap()
            .playback
            .as_ref()
            .and_then(|p| p.intro_download.as_ref().map(|j| j.id.clone()));
        match action {
            0 => policy.enabled = !policy.enabled,
            1 => policy.keep_count = (policy.keep_count + 1).min(10),
            2 => policy.keep_count = policy.keep_count.saturating_sub(1).max(1),
            3 => {
                let preview = MetaPreview {
                    id: policy.title_id.clone(),
                    type_: policy.media_type.clone(),
                    name: policy.title.clone(),
                    ..Default::default()
                };
                self.open_preview(preview, 0, false, None, false);
                if let Some(app) = self.app() {
                    app.global::<nova_ui_shared::OfflineBridge>()
                        .set_opened(false);
                    app.global::<nova_ui_shared::OfflineBridge>().set_notice(
                        text::tr("Open an episode and use this source for smart downloads").into(),
                    );
                }
                return;
            }
            4 => {
                let jobs = self.downloads.jobs();
                for job in jobs.iter().filter(|j| {
                    j.offline.owners.contains(&Owner {
                        profile: owner.id().into(),
                        policy: key.into(),
                    })
                }) {
                    self.downloads.resume(&job.id);
                }
                let _serial = device_guard();
                if let Ok(mut device) = read_device() {
                    device.receipts.retain(|k, _| {
                        serde_json::from_str::<(Owner, String)>(k)
                            .is_ok_and(|(o, _)| o.profile != owner.id() || o.policy != key)
                    });
                    device.suppressed.retain(|k, _| {
                        serde_json::from_str::<(Owner, String)>(k)
                            .is_ok_and(|(o, _)| o.profile != owner.id() || o.policy != key)
                    });
                    let _ = save_device(&device);
                }
            }
            5 => {
                policies.remove(key);
                self.downloads.keep_removed_policy(
                    &Owner {
                        profile: owner.id().into(),
                        policy: key.into(),
                    },
                    true,
                    playing.as_deref(),
                );
            }
            6 => policy.include_specials = !policy.include_specials,
            7 => policy.cleanup_watched = !policy.cleanup_watched,
            8 => {
                policies.remove(key);
                self.downloads.keep_removed_policy(
                    &Owner {
                        profile: owner.id().into(),
                        policy: key.into(),
                    },
                    false,
                    playing.as_deref(),
                );
            }
            _ => return,
        }
        if let Ok(raw) = serde_json::to_string(&policies) {
            let _ = owner.persist_snapshot(KEY, &raw, false);
        }
        trigger();
        self.refresh_smart_offline();
    }
    pub(super) fn smart_source_selected(&self, row: &StreamUi) {
        let owner = profiles::ProfileStore::capture();
        if !self.require_profile_management() {
            return;
        }
        let Some(key) = self
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_ref()
            .map(|m| policy_key(&m.type_, &m.id))
        else {
            return;
        };
        let Ok(mut policies) = read_policies(&owner) else {
            return;
        };
        let Some(policy) = policies.get_mut(&key) else {
            return;
        };
        policy.preference = Some(StreamPreference::from_row(row));
        policy.source_owner = row.owner_url.clone();
        if let Ok(raw) = serde_json::to_string(&policies) {
            let _ = owner.persist_snapshot(KEY, &raw, false);
        }
        trigger();
        self.refresh_smart_offline();
    }
    pub(super) fn smart_offline_tick(&self) {
        let now = now_secs();
        if now < LAST_CHECK.load(Ordering::Acquire).saturating_add(2) {
            return;
        }
        if !DIRTY.load(Ordering::Acquire) && now < NEXT_CHECK.load(Ordering::Acquire) {
            return;
        }
        if RUNNING.swap(true, Ordering::AcqRel) {
            return;
        }
        DIRTY.store(false, Ordering::Release);
        LAST_CHECK.store(now, Ordering::Release);
        NEXT_CHECK.store(now.saturating_add(30), Ordering::Release);
        let coordinator = self.downloads.clone();
        let b = self.clone();
        let playing = self
            .shared
            .lock()
            .unwrap()
            .playback
            .as_ref()
            .and_then(|p| p.intro_download.as_ref().map(|job| job.id.clone()));
        std::thread::spawn(move || {
            struct Done;
            impl Drop for Done {
                fn drop(&mut self) {
                    RUNNING.store(false, Ordering::Release);
                }
            }
            let _done = Done;
            if let Err(error) = reconcile(&coordinator, &AtomicBool::new(false), playing.as_deref())
            {
                tracing::warn!(%error,"offline reconciliation deferred");
            }
            let _ = slint::invoke_from_event_loop(move || b.refresh_smart_offline());
        });
    }
    pub(super) fn refresh_smart_offline(&self) {
        if profile_policy::entry_blocked() {
            return;
        }
        let Some(app) = self.app() else { return };
        let global = app.global::<nova_ui_shared::OfflineBridge>();
        let owner = profiles::ProfileStore::capture();
        let policies = read_policies(&owner).unwrap_or_default();
        let device = read_device().unwrap_or_default();
        let jobs = self.downloads.jobs();
        let ledger = Ledger::from_jobs(&jobs);
        global.set_profiles(
            Rc::new(VecModel::from(
                profiles::list()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|profile| nova_ui_shared::OfflineDeviceProfile {
                        enabled: device.enabled_profiles.contains(&profile.id),
                        id: profile.id.into(),
                        name: profile.name.into(),
                    })
                    .collect::<Vec<_>>(),
            ))
            .into(),
        );
        global.set_device_enabled(device.enabled_profiles.contains(owner.id()));
        global.set_charging_only(device.charging_only);
        global.set_budget_gib((device.allowance_bytes / GIB).min(i32::MAX as u64) as i32);
        global.set_storage_summary(
            text::smart_offline_storage(
                ledger.automatic_bytes,
                ledger.outstanding,
                ledger.manual_bytes,
                device.allowance_bytes,
            )
            .into(),
        );
        let detail = self
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_ref()
            .map(|m| policy_key(&m.type_, &m.id));
        global.set_detail_enabled(
            detail
                .as_ref()
                .is_some_and(|key| policies.get(key).is_some_and(|p| p.enabled)),
        );
        let rows = policies
            .into_iter()
            .map(|(key, p)| {
                let own = Owner {
                    profile: owner.id().into(),
                    policy: key.clone(),
                };
                let matching: Vec<_> = jobs
                    .iter()
                    .filter(|j| j.media_type == p.media_type && j.media_id == p.title_id)
                    .collect();
                let ready = matching
                    .iter()
                    .filter(|j| {
                        j.phase == crate::download::DownloadPhase::Completed
                            && j.artifact_path.as_ref().is_some_and(|p| p.is_file())
                    })
                    .count();
                let active = matching
                    .iter()
                    .filter(|j| {
                        matches!(
                            j.phase,
                            crate::download::DownloadPhase::Downloading
                                | crate::download::DownloadPhase::Resolving
                        )
                    })
                    .count();
                let message = NOTICES
                    .lock()
                    .unwrap()
                    .get(&serde_json::to_string(&own).unwrap())
                    .cloned()
                    .unwrap_or_else(|| {
                        if !device.enabled_profiles.contains(owner.id()) {
                            "Execution disabled on this device".into()
                        } else if p.preference.is_none() {
                            "Choose a source for smart downloads".into()
                        } else {
                            "Pending background opportunity".into()
                        }
                    });
                nova_ui_shared::SmartOfflineRow {
                    id: key.into(),
                    title: p.title.into(),
                    enabled: p.enabled,
                    keep_count: i32::from(p.keep_count),
                    include_specials: p.include_specials,
                    cleanup: p.cleanup_watched,
                    source_ready: p.preference.is_some(),
                    status: text::smart_offline_status(ready, active, &message).into(),
                }
            })
            .collect::<Vec<_>>();
        let old_key = global.get_focused_policy();
        let next = rows
            .iter()
            .position(|row| row.id == old_key)
            .map(|i| i as i32)
            .unwrap_or_else(|| global.get_focused_index().min(rows.len() as i32 - 1));
        let model = global.get_rows();
        if model.row_count() == rows.len()
            && model.iter().zip(&rows).all(|(old, row)| old.id == row.id)
        {
            for (index, row) in rows.into_iter().enumerate() {
                let old = model.row_data(index).unwrap();
                if old.title != row.title
                    || old.status != row.status
                    || old.enabled != row.enabled
                    || old.keep_count != row.keep_count
                    || old.cleanup != row.cleanup
                    || old.include_specials != row.include_specials
                    || old.source_ready != row.source_ready
                {
                    model.set_row_data(index, row);
                }
            }
        } else {
            global.set_rows(Rc::new(VecModel::from(rows)).into());
        }
        global.set_focused_index(next);
        if next >= 0
            && let Some(row) = global.get_rows().row_data(next as usize)
        {
            global.set_focused_policy(row.id);
        }
    }
}

#[cfg(not(target_os = "android"))]
fn start_environment_monitor() {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    std::thread::spawn(|| {
        loop {
            set_environment(probe_desktop_environment());
            std::thread::park_timeout(Duration::from_secs(5));
        }
    });
}
/// Only the actual Internet route interfaces may authorize unattended bytes.
/// All routable families must have known allowed cost/type; a local-only free
/// link cannot authorize traffic sent on a different metered default link.
#[cfg(any(target_os = "linux", test))]
fn default_route_environment(
    defaults: &[String],
    links: &BTreeMap<String, Environment>,
) -> Environment {
    if defaults.is_empty()
        || !defaults.iter().all(|device| {
            links
                .get(device)
                .is_some_and(|env| env.connected && env.wifi_or_ethernet && env.unmetered)
        })
    {
        return Environment::default();
    }
    Environment {
        connected: true,
        wifi_or_ethernet: true,
        unmetered: true,
        charging: false,
    }
}

#[cfg(not(target_os = "android"))]
fn probe_desktop_environment() -> Environment {
    // Unknown OS cost information fails closed. NetworkManager supplies the
    // active link's explicit metering status; never equate all Wi-Fi with free.
    #[cfg(target_os = "linux")]
    {
        let probes = [
            ("-4", "1.1.1.1"),
            ("-4", "8.8.8.8"),
            ("-6", "2606:4700:4700::1111"),
            ("-6", "2001:4860:4860::8888"),
        ];
        let mut defaults = Vec::new();
        for family in probes.chunks(2) {
            let mut family_routes = Vec::new();
            for &(family, address) in family {
                let Ok(output) = std::process::Command::new("ip")
                    .env("LC_ALL", "C")
                    .env("LANG", "C")
                    .args(["-j", family, "route", "get", address])
                    .output()
                else {
                    return Environment::default();
                };
                if !output.status.success() {
                    if String::from_utf8_lossy(&output.stderr).contains("Network is unreachable") {
                        family_routes.push(None);
                        continue;
                    }
                    return Environment::default();
                }
                let Ok(routes) = serde_json::from_slice::<Vec<serde_json::Value>>(&output.stdout)
                else {
                    return Environment::default();
                };
                if routes.len() != 1 {
                    return Environment::default();
                }
                let Some(device) = routes[0]
                    .get("dev")
                    .and_then(serde_json::Value::as_str)
                    .filter(|name| !name.is_empty())
                else {
                    return Environment::default();
                };
                family_routes.push(Some(device.to_owned()));
            }
            if family_routes.iter().any(Option::is_some)
                && family_routes.iter().any(Option::is_none)
            {
                return Environment::default();
            }
            defaults.extend(family_routes.into_iter().flatten());
        }
        let mut links = BTreeMap::new();
        for device in &defaults {
            let Ok(output) = std::process::Command::new("nmcli")
                .env("LC_ALL", "C")
                .env("LANG", "C")
                .args([
                    "-g",
                    "GENERAL.TYPE,GENERAL.STATE,GENERAL.METERED",
                    "device",
                    "show",
                    device,
                ])
                .output()
            else {
                return Environment::default();
            };
            if !output.status.success() {
                return Environment::default();
            }
            let output = String::from_utf8_lossy(&output.stdout);
            let fields = output.lines().collect::<Vec<_>>();
            if fields.len() != 3 {
                return Environment::default();
            }
            links.insert(
                device.clone(),
                Environment {
                    connected: fields[1].split_whitespace().next() == Some("100"),
                    wifi_or_ethernet: matches!(fields[0].trim(), "wifi" | "ethernet"),
                    unmetered: matches!(fields[2].trim(), "no" | "no (guessed)"),
                    charging: false,
                },
            );
        }
        let mut env = default_route_environment(&defaults, &links);
        if let Ok(entries) = std::fs::read_dir("/sys/class/power_supply") {
            let paths = entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .collect::<Vec<_>>();
            let battery = paths.iter().any(|path| {
                std::fs::read_to_string(path.join("type"))
                    .is_ok_and(|kind| kind.trim() == "Battery")
            });
            env.charging = !battery
                || paths.iter().any(|path| {
                    std::fs::read_to_string(path.join("online")).is_ok_and(|s| s.trim() == "1")
                });
        }
        env
    }
    #[cfg(target_os = "windows")]
    {
        let script = r#"$ErrorActionPreference='Stop'; [Windows.Networking.Connectivity.NetworkInformation,Windows.Networking.Connectivity,ContentType=WindowsRuntime] > $null; $p=[Windows.Networking.Connectivity.NetworkInformation]::GetInternetConnectionProfile(); if($null -eq $p){exit}; $t=$p.NetworkAdapter.IanaInterfaceType; $free=$p.GetConnectionCost().NetworkCostType -eq 'Unrestricted'; $b=@(Get-CimInstance Win32_Battery); $ac=$b.Count -eq 0 -or @($b | Where-Object {$_.BatteryStatus -ne 2}).Count -eq 0; Write-Output "$t,$free,$ac""#;
        let Ok(output) = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .output()
        else {
            return Environment::default();
        };
        let result = String::from_utf8_lossy(&output.stdout);
        let parts: Vec<_> = result.trim().split(',').collect();
        if parts.len() != 3 {
            return Environment::default();
        }
        Environment {
            connected: output.status.success(),
            wifi_or_ethernet: matches!(parts[0], "6" | "71"),
            unmetered: parts[1] == "True",
            charging: parts[2] == "True",
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        Environment::default()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nondefault_free_lan_cannot_authorize_metered_internet_routes() {
        let free = Environment {
            connected: true,
            wifi_or_ethernet: true,
            unmetered: true,
            charging: false,
        };
        let metered = Environment {
            unmetered: false,
            ..free
        };
        let links = BTreeMap::from([("local-ethernet".into(), free), ("tether".into(), metered)]);
        assert!(!default_route_environment(&["tether".into()], &links).unmetered);
        assert!(
            !default_route_environment(&["local-ethernet".into(), "tether".into()], &links)
                .unmetered
        );
        assert!(!default_route_environment(&["unknown".into()], &links).connected);
        assert!(!default_route_environment(&[], &links).connected);
        assert!(default_route_environment(&["local-ethernet".into()], &links).unmetered);
    }

    #[test]
    fn frontier_prioritizes_resume_excludes_specials_future_and_unknown() {
        let videos = vec![
            Video {
                id: "future".into(),
                season: Some(1),
                episode: Some(4),
                released: Some("2099-01-01T00:00:00Z".into()),
                ..Default::default()
            },
            Video {
                id: "a".into(),
                season: Some(1),
                episode: Some(1),
                released: Some("2020-01-01T00:00:00Z".into()),
                ..Default::default()
            },
            Video {
                id: "b".into(),
                season: Some(1),
                episode: Some(2),
                released: Some("2020-01-02T00:00:00Z".into()),
                ..Default::default()
            },
            Video {
                id: "s".into(),
                season: Some(0),
                episode: Some(1),
                released: Some("2020-01-01T00:00:00Z".into()),
                ..Default::default()
            },
            Video {
                id: "unknown".into(),
                season: Some(1),
                episode: Some(3),
                ..Default::default()
            },
            Video {
                id: "malformed".into(),
                season: Some(1),
                episode: Some(5),
                released: Some("not-a-date".into()),
                ..Default::default()
            },
            Video {
                id: "invalid".into(),
                season: Some(1),
                episode: Some(6),
                released: Some("2026-02-30".into()),
                ..Default::default()
            },
        ];
        let mut progress = HashMap::new();
        progress.insert(
            progress_map_key("show", "b"),
            EpisodeProgress {
                position_secs: 12.0,
                ..Default::default()
            },
        );
        assert_eq!(
            frontier(&videos, &progress, "show", 3, false, 20000)
                .iter()
                .map(|v| v.id.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "a"]
        );
        progress
            .get_mut(&progress_map_key("show", "b"))
            .unwrap()
            .watched = true;
        assert_eq!(
            frontier(&videos, &progress, "show", 3, false, 20000)
                .iter()
                .map(|v| v.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a"]
        );
    }
}

/// Stage this device edit in the same atomic import transaction as personal
/// policies. Import/sync never grants a new device execution capability.
pub(crate) fn portable_import_device_edits_for_profiles(
    ids: &[String],
) -> Result<Vec<(String, Option<String>)>, String> {
    let mut device = read_device()?;
    device
        .enabled_profiles
        .retain(|profile| !ids.contains(profile));
    device.receipts.retain(|key, _| {
        serde_json::from_str::<(Owner, String)>(key)
            .is_ok_and(|(entry, _)| !ids.contains(&entry.profile))
    });
    Ok(vec![(
        DEVICE_KEY.into(),
        Some(serde_json::to_string(&device).map_err(|e| e.to_string())?),
    )])
}

/// Rotate a bounded, durable freshness inventory in no-Activity job windows.
/// Shared metadata code retains cached fallback and checks baseline ownership.
#[cfg(target_os = "android")]
pub(super) fn refresh_headless_inventory(cancel: &AtomicBool) -> Result<(), String> {
    let device = read_device()?;
    if device.environmental_pause(environment()).is_some() {
        return Ok(());
    }
    let mut candidates = Vec::new();
    for id in &device.enabled_profiles {
        if !profiles::valid_id(id) {
            continue;
        }
        let owner = profiles::ProfileStore::for_profile(id);
        for (key, policy) in read_policies(&owner)? {
            if !policy.enabled {
                continue;
            }
            let key = serde_json::to_string(&(id, key)).unwrap();
            let last = device.metadata_refreshes.get(&key).copied().unwrap_or(0);
            if now_secs().saturating_sub(last) >= 30 * 60 {
                candidates.push((last, key, owner.clone(), policy));
            }
        }
    }
    candidates.sort_by_key(|(last, key, _, _)| (*last, key.clone()));
    for (_, key, owner, policy) in candidates.into_iter().take(4) {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        {
            let _serial = device_guard();
            let mut latest = read_device()?;
            latest.metadata_refreshes.insert(key, now_secs());
            save_device(&latest)?;
        }
        if let Err(error) =
            headless_profile::refresh_metadata(&owner, &policy.media_type, &policy.title_id, cancel)
        {
            tracing::debug!(%error,"offline cached metadata retained");
        }
    }
    Ok(())
}

/// Import holds this before core-store/profile/storage locks so a stale local
/// device RMW can never re-enable execution after its atomic disable commit.
pub(crate) fn device_guard() -> std::sync::MutexGuard<'static, ()> {
    DEVICE_SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
