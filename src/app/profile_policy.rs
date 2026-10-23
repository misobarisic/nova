//! Plaintext profile policy. Durable checks are shared by UI and headless jobs.
use super::*;
use std::collections::BTreeMap;

pub(super) const KEY: &str = "profile_policy:v1";
pub(super) const DOMAIN: &str = "profile_policy";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct AllowedTitle {
    pub media_type: String,
    pub title_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct ProfilePolicy {
    pub version: u32,
    pub pin: String,
    pub require_pin_on_entry: bool,
    pub restricted: bool,
    pub allowed_titles: Vec<AllowedTitle>,
    pub allowed_addons: Vec<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}
impl Default for ProfilePolicy {
    fn default() -> Self {
        Self {
            version: 1,
            pin: String::new(),
            require_pin_on_entry: false,
            restricted: false,
            allowed_titles: Vec::new(),
            allowed_addons: Vec::new(),
            extra: BTreeMap::new(),
        }
    }
}
impl ProfilePolicy {
    pub(super) fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err("Unsupported profile policy.".into());
        }
        if !self.pin.is_empty() && !(4..=8).contains(&self.pin.len()) {
            return Err("Enter 4–8 digits.".into());
        }
        if !self.pin.bytes().all(|b| b.is_ascii_digit()) {
            return Err("Enter 4–8 digits.".into());
        }
        if (self.restricted || self.require_pin_on_entry) && self.pin.is_empty() {
            return Err("Set a PIN first.".into());
        }
        Ok(())
    }
    pub(super) fn protected(&self) -> bool {
        !self.pin.is_empty()
    }
    fn permits_title(&self, media_type: &str, title_id: &str, in_library: bool) -> bool {
        !self.restricted
            || (in_library
                && self
                    .allowed_titles
                    .iter()
                    .any(|t| t.media_type == media_type && t.title_id == title_id))
    }
}

pub(super) fn load(owner: &profiles::ProfileStore) -> Result<ProfilePolicy, String> {
    if owner.retired().map_err(|e| e.to_string())? {
        return Err("This profile is unavailable.".into());
    }
    let value: ProfilePolicy = owner
        .read(KEY)
        .map_err(|e| e.to_string())?
        .map(|raw| serde_json::from_str(&raw).map_err(|e| e.to_string()))
        .transpose()?
        .unwrap_or_default();
    value.validate()?;
    Ok(value)
}
pub(super) fn allows_title(
    owner: &profiles::ProfileStore,
    media_type: &str,
    title_id: &str,
) -> bool {
    if !owner.is_scoped() {
        return true;
    }
    let Ok(policy) = load(owner) else {
        return false;
    };
    if !policy.restricted {
        return true;
    }
    let entries: Vec<LibraryEntry> = owner
        .read("library")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    policy.permits_title(
        media_type,
        title_id,
        entries
            .iter()
            .any(|e| e.type_ == media_type && e.id == title_id),
    )
}
pub(super) fn allows_addon(owner: &profiles::ProfileStore, addon: &str) -> bool {
    !owner.is_scoped()
        || load(owner).is_ok_and(|p| !p.restricted || p.allowed_addons.iter().any(|a| a == addon))
}
/// A value receipt is stronger than a process counter: inactive sync/import also
/// invalidates work, without needing a live session or callback.
pub(super) fn revision(owner: &profiles::ProfileStore) -> String {
    if !owner.is_scoped() {
        return String::new();
    }
    owner.read(KEY).ok().flatten().unwrap_or_default()
}

#[derive(Clone)]
struct Grant {
    profile: profiles::Context,
    revision: String,
}
// UI activation authority is separate from durable/background ownership.
// An entry-locked startup installs no personal state/provider inventory until
// the keypad authorizes an actual selection.
static ENTRY_BLOCKED: AtomicBool = AtomicBool::new(false);
pub(super) fn entry_blocked() -> bool {
    ENTRY_BLOCKED.load(Ordering::Acquire)
}
pub(super) fn set_entry_blocked(blocked: bool) {
    ENTRY_BLOCKED.store(blocked, Ordering::Release);
}
pub(super) fn initialize_entry_gate(owner: &profiles::ProfileStore) -> bool {
    let blocked = load(owner).map_or(true, |p| p.require_pin_on_entry);
    set_entry_blocked(blocked);
    blocked
}
static ENTRY_BASIS: LazyLock<Mutex<HashMap<String, (String, bool)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
pub(super) fn remember_entry(owner: &profiles::ProfileStore) {
    if let Ok(policy) = load(owner) {
        ENTRY_BASIS
            .lock()
            .unwrap()
            .insert(owner.id().into(), (policy.pin, policy.require_pin_on_entry));
    }
}
static GRANTS: LazyLock<Mutex<HashMap<String, Grant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
pub(super) fn restricted() -> bool {
    let owner = profiles::ProfileStore::capture();
    owner.is_scoped() && load(&owner).map_or(true, |p| p.restricted)
}
#[cfg(any(target_os = "android", test))]
pub(super) fn suspend() {
    clear_grants();
    PENDING.lock().unwrap().take();
}
pub(super) fn clear_grants() {
    BROWSING.store(false, Ordering::Release);
    GRANTS.lock().unwrap().clear();
}
pub(super) fn management_allowed(owner: &profiles::ProfileStore) -> bool {
    if !owner.is_scoped() {
        return true;
    }
    let active = profiles::ProfileStore::for_profile(&profiles::active_context().id);
    let active_restricted = load(&active).map_or(true, |p| p.restricted);
    let own_protected = load(owner).map_or(true, |p| p.protected());
    let granted = |owner: &profiles::ProfileStore| {
        GRANTS
            .lock()
            .unwrap()
            .get(owner.id())
            .is_some_and(|g| profiles::is_current(&g.profile) && g.revision == revision(owner))
    };
    (!active_restricted || granted(&active)) && (!own_protected || granted(owner))
}
pub(super) fn content_management_allowed(owner: &profiles::ProfileStore) -> bool {
    if !owner.is_scoped() {
        return true;
    }
    load(owner).is_ok_and(|p| !p.restricted) || management_allowed(owner)
}
pub(super) fn save(owner: &profiles::ProfileStore, policy: &ProfilePolicy) -> Result<(), String> {
    if !management_allowed(owner) {
        return Err("Enter PIN to continue.".into());
    }
    policy.validate()?;
    remember_entry(owner);
    owner
        .persist_snapshot(
            KEY,
            &serde_json::to_string(policy).map_err(|e| e.to_string())?,
            false,
        )
        .map_err(|e| e.to_string())?;
    clear_grants();
    Ok(())
}
/// Merge never removes a local PIN merely because an archive omits it.
pub(super) fn merge(
    local: &ProfilePolicy,
    archived: &ProfilePolicy,
    replace: bool,
) -> ProfilePolicy {
    let mut result = archived.clone();
    if !replace && result.pin.is_empty() && !local.pin.is_empty() {
        result.pin = local.pin.clone();
        result.require_pin_on_entry = local.require_pin_on_entry;
    }
    result
}

/// The returned owner supplies the single PIN needed for this selection.
pub(super) fn selection_pin(
    current: &ProfilePolicy,
    target: &ProfilePolicy,
    same: bool,
) -> Option<bool> {
    if target.require_pin_on_entry {
        Some(true)
    } else if current.restricted && !target.restricted && !same {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cold_explicit_owner_enforces_policy_without_runtime_bootstrap() {
        const ROOT: &str = "NOVA_COLD_PROFILE_POLICY_TEST_ROOT";
        let Some(root) = std::env::var_os(ROOT) else {
            let root = std::env::temp_dir().join(format!(
                "nova-cold-policy-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output=std::process::Command::new(std::env::current_exe().unwrap()).env(ROOT,&root)
                .args(["--exact","app::profile_policy::tests::cold_explicit_owner_enforces_policy_without_runtime_bootstrap","--nocapture"]).output().unwrap();
            let complete = root.join("complete").is_file();
            let _ = std::fs::remove_dir_all(&root);
            assert!(
                output.status.success() && complete,
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        let root = PathBuf::from(root);
        nova_storage::init_at(&root);
        assert!(!profiles::is_initialized());
        let legacy = profiles::ProfileStore::capture();
        assert!(!legacy.is_scoped());
        let owner = profiles::ProfileStore::for_profile("cold-profile");
        assert!(owner.is_scoped());
        let mut policy = ProfilePolicy {
            pin: "0012".into(),
            restricted: true,
            ..Default::default()
        };
        owner
            .write(KEY, &serde_json::to_string(&policy).unwrap())
            .unwrap();
        owner
            .write(
                "library",
                r#"[{"id":"same","type_":"series","name":"Allowed series"}]"#,
            )
            .unwrap();
        assert!(!allows_title(&owner, "series", "same"));
        assert!(!allows_addon(&owner, "https://source/exact?config=1"));
        assert!(!management_allowed(&owner));
        assert!(allows_title(&legacy, "series", "same"));
        assert!(allows_addon(&legacy, "any-source"));
        policy.allowed_titles.push(AllowedTitle {
            media_type: "series".into(),
            title_id: "same".into(),
        });
        policy
            .allowed_addons
            .push("https://source/exact?config=1".into());
        let before = revision(&owner);
        owner
            .write(KEY, &serde_json::to_string(&policy).unwrap())
            .unwrap();
        assert_ne!(revision(&owner), before);
        assert!(allows_title(&owner, "series", "same"));
        assert!(!allows_title(&owner, "movie", "same"));
        assert!(allows_addon(&owner, "https://source/exact?config=1"));
        assert!(!allows_addon(&owner, "https://source/exact?config=2"));
        owner.run(|| assert!(restricted()));
        owner.write("library", "[]").unwrap();
        assert!(!allows_title(&owner, "series", "same"));
        owner.write(KEY, "{").unwrap();
        assert!(!allows_addon(&owner, "https://source/exact?config=1"));
        assert!(!profiles::is_initialized());
        std::fs::write(root.join("complete"), "ok").unwrap();
    }
    #[test]
    fn leading_zero_pin_and_unknown_fields_round_trip() {
        let raw = r#"{"pin":"0012","restricted":true,"future":{"keep":1}}"#;
        let p: ProfilePolicy = serde_json::from_str(raw).unwrap();
        p.validate().unwrap();
        assert_eq!(p.pin, "0012");
        assert_eq!(
            serde_json::from_str::<ProfilePolicy>(&serde_json::to_string(&p).unwrap()).unwrap(),
            p
        );
        for pin in ["123", "123456789", "12a4", "１２３４"] {
            assert!(
                ProfilePolicy {
                    pin: pin.into(),
                    ..p.clone()
                }
                .validate()
                .is_err()
            );
        }
    }
    #[test]
    fn restricted_allowlist_is_typed_empty_denies_and_library_removal_pauses() {
        let mut p = ProfilePolicy {
            restricted: true,
            pin: "0012".into(),
            ..Default::default()
        };
        assert!(!p.permits_title("movie", "same", true));
        p.allowed_titles.push(AllowedTitle {
            media_type: "series".into(),
            title_id: "same".into(),
        });
        assert!(!p.permits_title("movie", "same", true));
        assert!(p.permits_title("series", "same", true));
        assert!(!p.permits_title("series", "same", false));
    }
    #[test]
    fn selection_table_and_import_pin_preservation() {
        let normal = ProfilePolicy::default();
        let restricted = ProfilePolicy {
            pin: "0001".into(),
            restricted: true,
            ..Default::default()
        };
        let locked = ProfilePolicy {
            pin: "0002".into(),
            require_pin_on_entry: true,
            ..Default::default()
        };
        assert_eq!(selection_pin(&normal, &normal, false), None);
        assert_eq!(selection_pin(&normal, &restricted, false), None);
        assert_eq!(selection_pin(&restricted, &restricted, false), None);
        assert_eq!(selection_pin(&restricted, &normal, false), Some(false));
        assert_eq!(selection_pin(&restricted, &locked, false), Some(true));
        assert_eq!(merge(&locked, &normal, false).pin, "0002");
        assert!(merge(&locked, &normal, true).pin.is_empty());
    }
}

#[derive(Clone)]
enum Action {
    Switch(String),
    Create(String),
    Rename(String, String),
    Delete(String),
    Edit(String),
    Management,
    OpenSettings(bool),
    OpenBackup,
    OpenOffline,
    ToggleOffline,
}
struct Pending {
    action: Action,
    pin_owner: String,
    revision: String,
    target_revision: Option<(String, String)>,
    context: profiles::Context,
}
static PENDING: LazyLock<Mutex<Option<Pending>>> = LazyLock::new(|| Mutex::new(None));
static EDITOR: LazyLock<Mutex<Option<(String, ProfilePolicy)>>> =
    LazyLock::new(|| Mutex::new(None));

impl Bridge {
    fn open_pin_gate(
        &self,
        owner: &profiles::ProfileStore,
        action: Action,
        target: Option<&profiles::ProfileStore>,
    ) {
        *PENDING.lock().unwrap() = Some(Pending {
            action,
            pin_owner: owner.id().into(),
            revision: revision(owner),
            target_revision: target.map(|t| (t.id().into(), revision(t))),
            context: profiles::active_context(),
        });
        if let Some(app) = self.app() {
            let ui = app.global::<crate::MoreBridge>();
            ui.set_pin_error("".into());
            ui.set_pin_input("".into());
            ui.set_pin_length(0);
            ui.set_pin_open(true);
        }
    }
    fn dismiss_profile_pin(&self) {
        PENDING.lock().unwrap().take();
        if let Some(app) = self.app() {
            let ui = app.global::<crate::MoreBridge>();
            ui.set_pin_open(false);
            ui.set_pin_input("".into());
            ui.set_pin_length(0);
            ui.set_pin_error("".into());
        }
    }
    pub(super) fn cancel_profile_pin(&self) {
        clear_grants();
        if PENDING.lock().unwrap().is_some()
            || self
                .app()
                .is_some_and(|app| app.global::<crate::MoreBridge>().get_pin_open())
        {
            // A successful selection already dismissed its keypad. Normal
            // Settings teardown must retain the accepted notification tap.
            release_reminders::cancel_pending_notification_tap();
            #[cfg(target_os = "android")]
            release_reminders_android::cancel_pending_tap();
        }
        self.dismiss_profile_pin();
        if let Some(app) = self.app() {
            app.recover_back_focus();
        }
    }
    pub(super) fn submit_profile_pin(&self, pin: &str) {
        let pending = PENDING.lock().unwrap().take();
        let Some(pending) = pending else {
            return;
        };
        let owner = profiles::ProfileStore::for_profile(&pending.pin_owner);
        if pending.context != profiles::active_context()
            || pending.revision != revision(&owner)
            || pending
                .target_revision
                .as_ref()
                .is_some_and(|(id, raw)| revision(&profiles::ProfileStore::for_profile(id)) != *raw)
            || load(&owner).is_err()
        {
            self.cancel_profile_pin();
            return;
        }
        if !load(&owner).is_ok_and(|p| p.pin == pin && !pin.is_empty()) {
            *PENDING.lock().unwrap() = Some(pending);
            if let Some(app) = self.app() {
                app.global::<crate::MoreBridge>()
                    .set_pin_error(text::tr("Incorrect PIN. Try again.").into());
            }
            return;
        }
        let action = pending.action.clone();
        if !matches!(action, Action::Switch(_)) {
            GRANTS.lock().unwrap().insert(
                owner.id().into(),
                Grant {
                    profile: profiles::active_context(),
                    revision: revision(&owner),
                },
            );
        }
        self.dismiss_profile_pin();
        match action {
            Action::Switch(id) => self.activate_profile_authorized(&id),
            Action::Create(name) => self.create_profile(&name),
            Action::Rename(id, name) => self.rename_profile(&id, &name),
            Action::Delete(id) => self.delete_profile(&id),
            Action::Edit(id) => self.edit_profile_policy(&id),
            Action::OpenOffline => {
                if let Some(app) = self.app() {
                    app.global::<nova_ui_shared::OfflineBridge>()
                        .set_opened(true);
                }
                self.refresh_smart_offline();
            }
            Action::ToggleOffline => self.toggle_detail_offline(),
            Action::OpenBackup => {
                if let Some(app) = self.app() {
                    app.global::<crate::BackupBridge>().invoke_opened();
                }
            }
            Action::Management => {}
            Action::OpenSettings(sync) => {
                if let Some(app) = self.app() {
                    app.set_sync_page(sync);
                }
                self.show_settings_page();
            }
        }
    }
    pub(super) fn gate_profile_selection(&self, id: &str) -> bool {
        let current = profiles::ProfileStore::for_profile(&profiles::active_context().id);
        let target = profiles::ProfileStore::for_profile(id);
        let current_policy = match load(&current) {
            Ok(policy) => policy,
            Err(_) if current.retired().unwrap_or(false) => ProfilePolicy::default(),
            Err(_) => return false,
        };
        let Ok(target_policy) = load(&target) else {
            return false;
        };
        if let Some(destination) =
            selection_pin(&current_policy, &target_policy, current.id() == id)
        {
            self.open_pin_gate(
                if destination { &target } else { &current },
                Action::Switch(id.into()),
                Some(&target),
            );
            false
        } else {
            true
        }
    }
    fn gate_management_action(&self, owner: &profiles::ProfileStore, action: Action) -> bool {
        if management_allowed(owner) {
            return true;
        }
        let current = profiles::ProfileStore::for_profile(&profiles::active_context().id);
        let gate = if !content_management_allowed(&current) {
            &current
        } else {
            owner
        };
        self.open_pin_gate(gate, action, Some(owner));
        false
    }
    pub(super) fn require_target_profile_management(&self, id: &str) -> bool {
        profiles::valid_id(id)
            && self.gate_management_action(
                &profiles::ProfileStore::for_profile(id),
                Action::Management,
            )
    }
    pub(super) fn request_offline_management(&self) -> bool {
        if entry_blocked() {
            return false;
        }
        let owner = profiles::ProfileStore::for_profile(&profiles::active_context().id);
        if content_management_allowed(&owner) {
            true
        } else {
            self.open_pin_gate(&owner, Action::OpenOffline, None);
            false
        }
    }
    pub(super) fn request_offline_detail_management(&self) -> bool {
        if entry_blocked() {
            return false;
        }
        let owner = profiles::ProfileStore::for_profile(&profiles::active_context().id);
        if content_management_allowed(&owner) {
            true
        } else {
            self.open_pin_gate(&owner, Action::ToggleOffline, None);
            false
        }
    }
    pub(super) fn request_backup_management(&self) -> bool {
        if entry_blocked() {
            return false;
        }
        let owner = profiles::ProfileStore::for_profile(&profiles::active_context().id);
        if content_management_allowed(&owner) {
            true
        } else {
            self.open_pin_gate(&owner, Action::OpenBackup, None);
            false
        }
    }
    pub(super) fn request_settings_management(&self) -> bool {
        if entry_blocked() {
            return false;
        }
        let owner = profiles::ProfileStore::for_profile(&profiles::active_context().id);
        if content_management_allowed(&owner) {
            return true;
        }
        let sync = self.app().is_some_and(|app| app.get_sync_page());
        self.open_pin_gate(&owner, Action::OpenSettings(sync), None);
        if let Some(app) = self.app() {
            app.set_show_settings(false);
            app.set_more_open(true);
        }
        false
    }
    pub(super) fn suspend_profile_management(&self) {
        self.close_profile_management();
        if restricted()
            && let Some(app) = self.app()
            && app.get_show_settings()
        {
            app.set_show_settings(false);
            app.set_more_open(true);
        }
    }
    pub(super) fn require_profile_management(&self) -> bool {
        if entry_blocked() {
            return false;
        }
        let owner = profiles::ProfileStore::for_profile(&profiles::active_context().id);
        if content_management_allowed(&owner) {
            true
        } else {
            self.open_pin_gate(&owner, Action::Management, None);
            false
        }
    }
    pub(super) fn gate_profile_create(&self, name: &str) -> bool {
        let owner = profiles::ProfileStore::for_profile(&profiles::active_context().id);
        if content_management_allowed(&owner) {
            true
        } else {
            self.open_pin_gate(&owner, Action::Create(name.into()), None);
            false
        }
    }
    pub(super) fn gate_profile_rename(&self, id: &str, name: &str) -> bool {
        self.gate_management_action(
            &profiles::ProfileStore::for_profile(id),
            Action::Rename(id.into(), name.into()),
        )
    }
    pub(super) fn gate_profile_delete(&self, id: &str) -> bool {
        self.gate_management_action(
            &profiles::ProfileStore::for_profile(id),
            Action::Delete(id.into()),
        )
    }
    pub(super) fn edit_profile_policy(&self, id: &str) {
        let owner = profiles::ProfileStore::for_profile(id);
        if !self.gate_management_action(&owner, Action::Edit(id.into())) {
            return;
        }
        let Ok(policy) = load(&owner) else {
            return;
        };
        *EDITOR.lock().unwrap() = Some((id.into(), policy));
        self.publish_profile_policy_editor();
        if let Some(app) = self.app() {
            app.global::<crate::MoreBridge>().set_policy_open(true);
        }
    }
    fn capture_policy_draft_fields(&self) {
        if let Some(app) = self.app() {
            let ui = app.global::<crate::MoreBridge>();
            if let Some((_, policy)) = EDITOR.lock().unwrap().as_mut() {
                policy.pin = ui.get_policy_pin().to_string();
                policy.require_pin_on_entry = ui.get_policy_entry();
                policy.restricted = ui.get_policy_restricted();
            }
        }
    }
    fn publish_profile_policy_editor(&self) {
        let guard = EDITOR.lock().unwrap();
        let Some((id, policy)) = guard.as_ref() else {
            return;
        };
        let owner = profiles::ProfileStore::for_profile(id);
        let entries: Vec<LibraryEntry> = owner
            .read("library")
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let addons: Vec<AddonStore> = owner
            .read("addons")
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        if let Some(app) = self.app() {
            let ui = app.global::<crate::MoreBridge>();
            ui.set_policy_pin(policy.pin.clone().into());
            ui.set_policy_entry(policy.require_pin_on_entry);
            ui.set_policy_restricted(policy.restricted);
            ui.set_policy_titles(
                Rc::new(VecModel::from(
                    entries
                        .iter()
                        .map(|e| SharedString::from(e.name.clone()))
                        .collect::<Vec<_>>(),
                ))
                .into(),
            );
            ui.set_policy_title_keys(
                Rc::new(VecModel::from(
                    entries
                        .iter()
                        .map(|e| {
                            SharedString::from(serde_json::to_string(&(&e.type_, &e.id)).unwrap())
                        })
                        .collect::<Vec<_>>(),
                ))
                .into(),
            );
            ui.set_policy_title_checks(
                Rc::new(VecModel::from(
                    entries
                        .iter()
                        .map(|e| {
                            policy
                                .allowed_titles
                                .iter()
                                .any(|t| t.media_type == e.type_ && t.title_id == e.id)
                        })
                        .collect::<Vec<_>>(),
                ))
                .into(),
            );
            let mut labels: Vec<SharedString> =
                addons.iter().map(|a| a.label.clone().into()).collect();
            labels.push(text::tr("Local files with unknown source").into());
            let mut checks: Vec<bool> = addons
                .iter()
                .map(|a| policy.allowed_addons.contains(&a.url))
                .collect();
            checks.push(policy.allowed_addons.iter().any(|a| a == "local"));
            ui.set_policy_addons(Rc::new(VecModel::from(labels)).into());
            ui.set_policy_addon_checks(Rc::new(VecModel::from(checks)).into());
            let mut keys: Vec<SharedString> = addons.iter().map(|a| a.url.clone().into()).collect();
            keys.push("local".into());
            ui.set_policy_addon_keys(Rc::new(VecModel::from(keys)).into());
        }
    }
    pub(super) fn toggle_profile_policy_title(&self, key: &str) {
        let Ok((kind, title_id)) = serde_json::from_str::<(String, String)>(key) else {
            return;
        };
        self.capture_policy_draft_fields();
        {
            let mut guard = EDITOR.lock().unwrap();
            let Some((id, p)) = guard.as_mut() else {
                return;
            };
            let owner = profiles::ProfileStore::for_profile(id);
            let entries: Vec<LibraryEntry> = owner
                .read("library")
                .ok()
                .flatten()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            if !management_allowed(&owner) {
                return;
            }
            if let Some(e) = entries.iter().find(|e| e.type_ == kind && e.id == title_id) {
                let t = AllowedTitle {
                    media_type: e.type_.clone(),
                    title_id: e.id.clone(),
                };
                if p.allowed_titles.contains(&t) {
                    p.allowed_titles.retain(|a| a != &t);
                } else {
                    p.allowed_titles.push(t);
                }
            }
        }
        self.publish_profile_policy_editor();
    }
    pub(super) fn toggle_profile_policy_addon(&self, key: &str) {
        self.capture_policy_draft_fields();
        {
            let mut guard = EDITOR.lock().unwrap();
            let Some((id, p)) = guard.as_mut() else {
                return;
            };
            let owner = profiles::ProfileStore::for_profile(id);
            let addons: Vec<AddonStore> = owner
                .read("addons")
                .ok()
                .flatten()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            if !management_allowed(&owner)
                || (key != "local" && !addons.iter().any(|a| a.url == key))
            {
                return;
            }
            let identity = key.to_string();
            if p.allowed_addons.contains(&identity) {
                p.allowed_addons.retain(|a| a != &identity);
            } else {
                p.allowed_addons.push(identity);
            }
        }
        self.publish_profile_policy_editor();
    }
    pub(super) fn save_profile_policy_editor(&self) {
        let Some(app) = self.app() else {
            return;
        };
        let ui = app.global::<crate::MoreBridge>();
        let Some((id, mut policy)) = EDITOR.lock().unwrap().clone() else {
            return;
        };
        policy.pin = ui.get_policy_pin().to_string();
        policy.require_pin_on_entry = ui.get_policy_entry();
        policy.restricted = ui.get_policy_restricted();
        match save(&profiles::ProfileStore::for_profile(&id), &policy) {
            Ok(()) => {
                self.close_profile_management();
                if profiles::active_context().id == id {
                    self.profile_policy_changed();
                } else {
                    self.refresh_profiles();
                }
            }
            Err(error) => ui.set_pin_error(text::tr(&error).into()),
        }
    }
    pub(super) fn profile_import_prepare(&self, ids: &[String]) -> Result<(), String> {
        if ids.iter().any(|id| {
            !profiles::valid_id(id) || !management_allowed(&profiles::ProfileStore::for_profile(id))
        }) {
            return Err("Enter PIN to continue.".into());
        }
        if ids.contains(&profiles::active_context().id) {
            self.persist_outgoing_playback();
            self.flush_playback_preferences(true);
            self.persist_settings();
            persistence::flush()?;
            self.cancel_pending_playback();
            self.cancel_sleep_timer();
            self.player.close();
            self.finish_playback_request();
            self.shutdown_tracking();
            self.shutdown_stremio();
            self.shutdown_list_services();
        }
        Ok(())
    }
    pub(super) fn profile_import_committed(&self, ids: &[String]) {
        let id = profiles::active_context().id;
        if ids.contains(&id) {
            self.profile_loaded_epoch.store(0, Ordering::Release);
            let required = load(&profiles::ProfileStore::for_profile(&id))
                .map_or(true, |p| p.require_pin_on_entry);
            if let Err(error) = self.activate_profile(&id, required) {
                tracing::warn!(%error,"reload imported profile");
            }
        }
        self.close_profile_management();
        self.refresh_profiles();
    }
    pub(super) fn finish_profile_management_for_selection(&self) {
        clear_grants();
        EDITOR.lock().unwrap().take();
        self.dismiss_profile_pin();
        if let Some(app) = self.app() {
            let ui = app.global::<crate::MoreBridge>();
            ui.set_policy_open(false);
            ui.set_policy_browse(false);
            ui.set_policy_pin("".into());
        }
    }
    pub(super) fn close_profile_management(&self) {
        clear_grants();
        EDITOR.lock().unwrap().take();
        self.cancel_profile_pin();
        if let Some(app) = self.app() {
            let ui = app.global::<crate::MoreBridge>();
            ui.set_policy_browse(false);
            ui.set_policy_open(false);
            ui.set_policy_pin("".into());
            app.global::<nova_ui_shared::OfflineBridge>()
                .set_opened(false);
            let backup = app.global::<crate::BackupBridge>();
            if !backup.get_busy() {
                backup.set_open(false);
            }
        }
        if BROWSE_DIRTY.swap(false, Ordering::AcqRel) && restricted() {
            if let Some(app) = self.app() {
                app.set_modal_visible(false);
                app.set_stream_selector_open(false);
            }
            self.close_search_results();
            self.load_catalog();
        }
    }
    pub(super) fn enforce_profile_content_policy(&self) {
        let owner = profiles::ProfileStore::capture();
        let denied = self
            .current_playback_identity()
            .is_some_and(|m| !allows_title(&owner, &m.media_type, &m.title_id))
            || !self.playback_policy_source_allowed();
        if denied {
            self.persist_outgoing_playback();
            self.cancel_pending_playback();
            self.cancel_sleep_timer();
            self.player.close();
            self.finish_playback_request();
        }
    }
    pub(super) fn profile_policy_changed(&self) {
        self.cancel_queue_run();
        self.clear_playback_end_menu();
        self.close_profile_management();
        self.cancel_profile_pin();
        self.cancel_pending_playback();
        let owner = profiles::ProfileStore::capture();
        if let Some(media) = self.current_playback_identity()
            && !allows_title(&owner, &media.media_type, &media.title_id)
        {
            self.persist_outgoing_playback();
            self.cancel_sleep_timer();
            self.player.close();
            self.finish_playback_request();
        }
        if let Some(app) = self.app() {
            app.set_modal_visible(false);
            app.set_stream_selector_open(false);
        }
        self.enforce_profile_content_policy();
        self.apply_library_to_ui();
        self.refresh_watch_queue();
        self.refresh_bookmarks();
        self.refresh_viewing_journal();
        self.rebuild_continue_list();
        self.rebuild_upcoming_list();
        self.rebuild_watch_history_list();
        self.apply_home_to_ui();
        self.close_search_results();
        self.load_catalog();
        self.refresh_home_showcase();
        self.refresh_home_catalog_rows();
        self.reconcile_release_reminders();
        let entry_changed = load(&owner).map_or(true, |p| {
            let current = (p.pin, p.require_pin_on_entry);
            let previous = ENTRY_BASIS
                .lock()
                .unwrap()
                .insert(owner.id().into(), current.clone());
            current.1 && previous.as_ref() != Some(&current)
        });
        if entry_changed {
            set_entry_blocked(true);
            if let Some(app) = self.app() {
                app.set_profiles_require_selection(true);
                app.set_profiles_open(true);
                app.set_more_open(true);
            }
        }
    }
}

static BROWSING: AtomicBool = AtomicBool::new(false);
static BROWSE_DIRTY: AtomicBool = AtomicBool::new(false);
pub(super) fn management_browsing() -> bool {
    BROWSING.load(Ordering::Acquire)
        && EDITOR
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|(id, _)| management_allowed(&profiles::ProfileStore::for_profile(id)))
}
pub(super) fn allows_browse_title(owner: &profiles::ProfileStore, kind: &str, id: &str) -> bool {
    !entry_blocked() && (management_browsing() || allows_title(owner, kind, id))
}
impl Bridge {
    pub(super) fn policy_search_titles(&self) {
        self.capture_policy_draft_fields();
        let Some((id, _)) = EDITOR.lock().unwrap().clone() else {
            return;
        };
        if !self.require_target_profile_management(&id) {
            return;
        }
        BROWSING.store(true, Ordering::Release);
        BROWSE_DIRTY.store(true, Ordering::Release);
        if let Some(app) = self.app() {
            let ui = app.global::<crate::MoreBridge>();
            ui.set_policy_open(false);
            ui.set_policy_browse(true);
            app.set_profiles_open(false);
            app.set_more_open(false);
        }
        self.show_discover_page();
        self.load_catalog();
    }
    pub(super) fn policy_finish_title_search(&self) {
        BROWSING.store(false, Ordering::Release);
        if let Some(app) = self.app() {
            let ui = app.global::<crate::MoreBridge>();
            ui.set_policy_browse(false);
            ui.set_policy_open(true);
            app.set_modal_visible(false);
            app.set_stream_selector_open(false);
        }
        self.publish_profile_policy_editor();
    }
    pub(super) fn policy_allow_selected_title(&self) {
        if !management_browsing() {
            return;
        }
        let Some((id, _)) = EDITOR.lock().unwrap().clone() else {
            return;
        };
        let owner = profiles::ProfileStore::for_profile(&id);
        let entry = {
            let state = self.shared.lock().unwrap();
            let Some(m) = state.modal_item.as_ref() else {
                return;
            };
            LibraryEntry {
                id: m.id.clone(),
                type_: m.type_.clone(),
                name: m.name.clone(),
                year: m.year.clone(),
                poster_url: m.poster_url.clone(),
                background_url: m.background_url.clone(),
                genres: m.genres.clone(),
                description: m.description.clone(),
                categories: Vec::new(),
                watch_status: WatchStatus::Auto,
                imported_tracking_status: None,
                added_at_secs: now_secs(),
            }
        };
        let mut entries: Vec<LibraryEntry> = owner
            .read("library")
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let title = AllowedTitle {
            media_type: entry.type_.clone(),
            title_id: entry.id.clone(),
        };
        library::upsert_library(&mut entries, entry);
        let result = serde_json::to_string(&entries)
            .map_err(|e| e.to_string())
            .and_then(|raw| {
                owner
                    .persist_snapshot("library", &raw, false)
                    .map_err(|e| e.to_string())
            });
        if let Err(error) = result {
            tracing::warn!(%error,"add allowed library title");
            return;
        }
        if profiles::active_context().id == id {
            self.shared.lock().unwrap().entries = entries;
        }
        if let Some((_, p)) = EDITOR.lock().unwrap().as_mut()
            && !p.allowed_titles.contains(&title)
        {
            p.allowed_titles.push(title);
        }
        self.policy_finish_title_search();
    }
}

pub(super) fn local_previews(query: &str) -> Vec<MetaPreview> {
    let owner = profiles::ProfileStore::capture();
    let entries: Vec<LibraryEntry> = owner
        .read("library")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    let needle = query.to_lowercase();
    entries
        .into_iter()
        .filter(|e| {
            allows_title(&owner, &e.type_, &e.id) && e.name.to_lowercase().contains(&needle)
        })
        .map(|e| MetaPreview {
            id: e.id,
            type_: e.type_,
            name: e.name,
            poster: Some(e.poster_url),
            background: Some(e.background_url),
            description: Some(e.description),
            genres: e.genres,
            ..Default::default()
        })
        .collect()
}
