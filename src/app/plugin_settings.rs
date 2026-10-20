//! App-owned plugin preferences share the atomic sync snapshot transaction.
use super::*;
use nova_providers::plugin_settings::{self as service, PluginSettingsBackend, SettingsMap};
pub(super) const KEY: &str = "plugin_settings";
struct LiveStreamPreferences {
    owner: String,
    profile: String,
    epoch: u64,
    rules: nova_config::StreamFilterRules,
}
static LIVE_STREAM_PREFERENCES: std::sync::LazyLock<Mutex<Option<LiveStreamPreferences>>> =
    std::sync::LazyLock::new(|| Mutex::new(None));

/// Publish the selected owner's in-memory policy before a debounced save or
/// provider dispatch. Epoch and durable-owner checks prevent another profile's
/// settings from leaking into a retained worker's invocation.
pub(super) fn publish_stream_preferences(rules: &nova_config::StreamFilterRules) {
    let context = profiles::capture();
    if !profiles::is_current(&context) {
        return;
    }
    *LIVE_STREAM_PREFERENCES.lock().unwrap() = Some(LiveStreamPreferences {
        owner: profiles::ProfileStore::capture().domain(KEY),
        profile: context.id,
        epoch: context.epoch,
        rules: rules.clone(),
    });
}
pub(super) struct Backend {
    owner: profiles::ProfileStore,
}
impl Backend {
    pub(super) fn new() -> Self {
        Self::for_store(profiles::ProfileStore::capture())
    }
    pub(super) fn for_store(owner: profiles::ProfileStore) -> Self {
        Self { owner }
    }
}
impl PluginSettingsBackend for Backend {
    fn owner_id(&self) -> Option<String> {
        Some(self.owner.domain("plugin_settings"))
    }
    fn stream_preferences(
        &self,
    ) -> Result<
        Option<nova_config::stream_filters::StreamFilterRules>,
        nova_providers::ProviderHostError,
    > {
        if self.owner.retired().map_err(provider_error)? {
            return Err(provider_error("provider owner retired"));
        }
        let active = profiles::active_context();
        let live = LIVE_STREAM_PREFERENCES
            .lock()
            .unwrap()
            .as_ref()
            .filter(|snapshot| {
                snapshot.owner == self.owner.domain(KEY)
                    && snapshot.profile == active.id
                    && snapshot.epoch == active.epoch
            })
            .map(|snapshot| snapshot.rules.clone());
        if let Some(rules) = live
            && profiles::is_current(&active)
        {
            return Ok(Some(rules));
        }
        let settings = self
            .owner
            .read("settings")
            .map_err(provider_error)?
            .map(|raw| {
                serde_json::from_str(&raw)
                    .ok()
                    .and_then(settings::recover_settings)
                    .ok_or_else(|| provider_error("unreadable provider stream preferences"))
            })
            .transpose()?
            .unwrap_or_default();
        Ok(Some(settings.stream_filters.resolved_with_subtitles(
            &settings.default_audio_language,
            &settings.default_subtitle_language,
        )))
    }
    fn provider_revision(
        &self,
        url: &str,
    ) -> Result<Option<String>, nova_providers::ProviderHostError> {
        if self.owner.retired().map_err(provider_error)? {
            return Ok(None);
        }
        let addons: Vec<AddonStore> = self
            .owner
            .read("addons")
            .map_err(provider_error)?
            .map(|raw| serde_json::from_str(&raw))
            .transpose()
            .map_err(provider_error)?
            .unwrap_or_default();
        let Some(addon) = addons
            .iter()
            .find(|addon| addon.url == url && addon.enabled)
        else {
            return Ok(None);
        };
        let Some(plugin) = plugin_id(url) else {
            return Ok(None);
        };
        let packages: serde_json::Value = self
            .owner
            .read(addon_packages::KEY)
            .map_err(provider_error)?
            .map(|raw| serde_json::from_str(&raw))
            .transpose()
            .map_err(provider_error)?
            .unwrap_or_default();
        let settings = self
            .owner
            .read(KEY)
            .map_err(provider_error)?
            .map(|raw| service::parse_settings(&raw))
            .transpose()?
            .unwrap_or_default();
        // Values alone would revive an old resolver after an inactive remote
        // remove/re-add or credential clear/restore. Include durable clocks,
        // including deleted fields, without cloning unrelated profile history.
        let revisions = {
            let store = nova_sync::local_store().map_err(provider_error)?;
            let store = store.lock().unwrap();
            (
                store
                    .record(&self.owner.domain(sync::DOMAIN_ADDONS), url)
                    .map(|record| record.version),
                store
                    .record(&self.owner.domain(sync::DOMAIN_PACKAGES), plugin)
                    .map(|record| record.version),
                store.versions_with_prefix(
                    &self.owner.domain(sync::DOMAIN_PLUGIN_SETTINGS),
                    &format!("{plugin}/"),
                ),
            )
        };
        serde_json::to_string(&(
            addon.capabilities,
            &packages[plugin],
            settings.get(plugin),
            revisions,
        ))
        .map(Some)
        .map_err(provider_error)
    }
    fn load_settings(&self) -> Result<Option<String>, nova_providers::ProviderHostError> {
        self.owner.read(KEY).map_err(provider_error)
    }
    fn save_settings(&self, raw: &str) -> Result<(), nova_providers::ProviderHostError> {
        self.owner
            .persist_snapshot(KEY, raw, false)
            .map_err(provider_error)
    }
    fn load_cache(&self) -> Result<Option<String>, nova_providers::ProviderHostError> {
        plugin_cache::load()
    }
    fn save_cache(&self, raw: &str) -> Result<(), nova_providers::ProviderHostError> {
        plugin_cache::save(raw)
    }
}
/// Recover the incoming owner's retained projection before publishing any
/// provider globals; unreadable credentials leave the outgoing owner intact.
pub(super) fn prepare_backend(
    owner: profiles::ProfileStore,
) -> Result<service::PreparedPluginSettings, String> {
    if owner
        .read(KEY)
        .map_err(|error| error.to_string())?
        .is_none()
    {
        let records = {
            let store = nova_sync::local_store().map_err(|error| error.to_string())?;
            let store = store.lock().unwrap();
            let domain = owner.domain(sync::DOMAIN_PLUGIN_SETTINGS);
            store
                .domains()
                .iter()
                .any(|candidate| candidate == &domain)
                .then(|| store.records(&domain))
        };
        if let Some(records) = records {
            let values = project(records).map_err(|error| error.to_string())?;
            let raw = serde_json::to_string(&values).map_err(|error| error.to_string())?;
            let _applying = sync::ApplyingGuard::new();
            owner
                .persist_snapshot(KEY, &raw, false)
                .map_err(|error| error.to_string())?;
        }
    }
    service::prepare_plugin_settings(Arc::new(Backend::for_store(owner)))
        .map_err(|error| error.to_string())
}

fn provider_error(error: impl std::fmt::Display) -> nova_providers::ProviderHostError {
    nova_providers::ProviderHostError(error.to_string())
}
pub(super) fn records(raw: &str) -> nova_sync::Result<Vec<(String, String, u64)>> {
    let values = service::parse_settings(raw)
        .map_err(|e| storage::Error::new(storage::ErrorKind::Schema, e))?;
    values
        .into_iter()
        .flat_map(|(plugin, fields)| {
            fields
                .into_iter()
                .map(move |(key, value)| (format!("{plugin}/{key}"), value))
        })
        .map(|(key, value)| Ok((key, serde_json::to_string(&value)?, 0)))
        .collect()
}
pub(super) fn project(
    records: Vec<(String, String)>,
) -> Result<SettingsMap, nova_providers::ProviderHostError> {
    let mut values = SettingsMap::new();
    for (record, raw) in records {
        let (plugin, key) = record
            .split_once('/')
            .ok_or_else(|| provider_error("invalid plugin settings record"))?;
        service::validate_plugin(plugin)?;
        service::validate_key(key)?;
        let value = serde_json::from_str(&raw).map_err(provider_error)?;
        values
            .entry(plugin.into())
            .or_default()
            .insert(key.into(), value);
    }
    let raw = serde_json::to_string(&values).map_err(provider_error)?;
    service::parse_settings(&raw)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn per_field_records_retain_unknown_values_and_tombstones() {
        let original = r#"{"example":{"token":"secret","future":{"x":1}}}"#;
        let records = records(original).unwrap();
        assert_eq!(records.len(), 2);
        let projected = project(
            records
                .into_iter()
                .filter(|(k, _, _)| k != "example/token")
                .map(|(k, v, _)| (k, v))
                .collect(),
        )
        .unwrap();
        assert_eq!(projected["example"]["future"], serde_json::json!({"x":1}));
        assert!(!projected["example"].contains_key("token"));
        assert!(project(vec![("../token".into(), "true".into())]).is_err());
    }

    #[test]
    fn provider_policy_uses_unsaved_live_owner_and_rejects_other_owners_and_epochs() {
        const TEST_DIR: &str = "NOVA_PROVIDER_POLICY_TEST_DIR";
        let Some(root) = std::env::var_os(TEST_DIR) else {
            let root = std::env::temp_dir().join(format!(
                "nova-provider-policy-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(TEST_DIR, &root)
                .args(["--exact", "app::plugin_settings::tests::provider_policy_uses_unsaved_live_owner_and_rejects_other_owners_and_epochs", "--nocapture"])
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
        storage::init_at(&PathBuf::from(root));
        profiles::initialize().unwrap();
        let alice = profiles::ProfileStore::capture();
        let bob = profiles::ProfileStore::for_profile("policy-bob");
        let mut alice_settings = CacheSettings {
            default_audio_language: "en".into(),
            default_subtitle_language: "hr,en".into(),
            ..Default::default()
        };
        alice_settings.stream_filters.enabled = true;
        alice_settings.stream_filters.max_height = Some(1080);
        let mut bob_settings = CacheSettings {
            default_audio_language: "ja".into(),
            default_subtitle_language: "fr".into(),
            ..Default::default()
        };
        bob_settings.stream_filters.enabled = true;
        bob_settings.stream_filters.max_height = Some(2160);
        alice
            .write("settings", &serde_json::to_string(&alice_settings).unwrap())
            .unwrap();
        bob.write("settings", &serde_json::to_string(&bob_settings).unwrap())
            .unwrap();
        let alice_backend = Backend::for_store(alice.clone());
        let bob_backend = Backend::for_store(bob.clone());
        assert_eq!(
            alice_backend
                .stream_preferences()
                .unwrap()
                .unwrap()
                .audio_languages,
            ["en"]
        );
        assert_eq!(
            alice_backend
                .stream_preferences()
                .unwrap()
                .unwrap()
                .subtitle_languages,
            ["hr", "en"]
        );
        let mut live = alice_settings
            .stream_filters
            .resolved_with_subtitles("hr", "de");
        live.max_height = Some(720);
        publish_stream_preferences(&live);
        assert_eq!(alice_backend.stream_preferences().unwrap(), Some(live));
        assert_eq!(
            serde_json::from_str::<CacheSettings>(&alice.read("settings").unwrap().unwrap())
                .unwrap()
                .stream_filters
                .max_height,
            Some(1080),
            "live policy must be available before persistence"
        );
        let bob_policy = bob_backend.stream_preferences().unwrap().unwrap();
        assert_eq!(bob_policy.audio_languages, ["ja"]);
        assert_eq!(bob_policy.subtitle_languages, ["fr"]);
        assert_eq!(bob_policy.max_height, Some(2160));
        bob_settings.stream_filters.max_height = Some(480);
        bob.write("settings", &serde_json::to_string(&bob_settings).unwrap())
            .unwrap();
        assert_eq!(
            bob_backend
                .stream_preferences()
                .unwrap()
                .unwrap()
                .max_height,
            Some(480),
            "inactive owner reads its own latest persisted policy"
        );
        LIVE_STREAM_PREFERENCES
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .epoch += 1;
        assert_eq!(
            alice_backend
                .stream_preferences()
                .unwrap()
                .unwrap()
                .max_height,
            Some(1080),
            "a snapshot from another activation cannot replace durable policy"
        );
    }
}

/// Older applications retain unfamiliar record domains but acknowledge their
/// projection. Recover such records before startup seeds an empty baseline.
pub(super) fn restore_retained_settings() -> nova_sync::Result<()> {
    if storage::try_get_str(KEY)?.is_some() {
        return Ok(());
    }
    let owner = nova_sync::local_store()?;
    if !owner
        .lock()
        .unwrap()
        .domains()
        .iter()
        .any(|domain| domain == &sync::profile_domain(sync::DOMAIN_PLUGIN_SETTINGS))
    {
        return Ok(());
    }
    let _guard = sync::ApplyingGuard::new();
    service::apply_settings_projection(|| {
        let values = project(
            owner
                .lock()
                .unwrap()
                .records(&sync::profile_domain(sync::DOMAIN_PLUGIN_SETTINGS)),
        )?;
        serde_json::to_string(&values).map_err(provider_error)
    })
    .map_err(|error| storage::Error::new(storage::ErrorKind::Schema, error))?;
    Ok(())
}

fn plugin_id(url: &str) -> Option<&str> {
    url.strip_prefix("nova-provider://")
        .map(|id| id.trim_end_matches('/'))
}

impl Bridge {
    pub(super) fn refresh_plugin_settings_ui(&self, url: &str) {
        let Some(app) = self.app() else {
            return;
        };
        let rows =
            (|| -> Result<Vec<nova_ui::PluginSettingRow>, nova_providers::ProviderHostError> {
                let Some(plugin) = plugin_id(url) else {
                    return Ok(Vec::new());
                };
                let schema = nova_providers::provider_settings_schema(url).unwrap_or_default();
                let snapshot = service::settings_snapshot(plugin, &schema)?;
                let managed =
                    plugin == "supernova" && nova_providers::provider_uses_stream_preferences(url);
                let value_for = |key: &str| {
                    schema
                        .iter()
                        .find(|f| f.key == key)
                        .map(|f| snapshot.values.get(key).unwrap_or(&f.default).clone())
                        .unwrap_or(serde_json::Value::Null)
                };
                let configured = |field: &nova_providers::plugin_settings::SettingField| match field
                    .item_role
                    .as_deref()
                {
                    Some("member") => value_for(&field.key).as_bool().unwrap_or(false),
                    Some("enabled") => {
                        value_for(&field.key).as_bool().unwrap_or(false)
                            || schema.iter().any(|f| {
                                f.item == field.item
                                    && f.collection == field.collection
                                    && f.item_role.as_deref() == Some("secret")
                                    && !value_for(&f.key).as_str().unwrap_or_default().is_empty()
                            })
                    }
                    _ => false,
                };
                let make_row = |field: &nova_providers::plugin_settings::SettingField| {
                    let value = snapshot.values.get(&field.key).unwrap_or(&field.default);
                    nova_ui::PluginSettingRow {
                        key: field.key.clone().into(),
                        label: text::tr(&field.label).into(),
                        kind: field.kind.clone().into(),
                        value: value
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| value.to_string())
                            .into(),
                        // Presentation summaries use declared option labels, never secret values.
                        summary: if field.kind == "choice" {
                            field
                                .options
                                .iter()
                                .find(|option| option.value == *value)
                                .map(|option| text::tr(&option.label).to_owned())
                                .unwrap_or_default()
                        } else if field.kind == "ordered-choices" || field.kind == "sort-order" {
                            value
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(|selected| {
                                    let selected = if field.kind == "sort-order" {
                                        &selected["key"]
                                    } else {
                                        selected
                                    };
                                    field
                                        .options
                                        .iter()
                                        .find(|option| option.value == *selected)
                                        .map(|option| text::tr(&option.label))
                                })
                                .collect::<Vec<_>>()
                                .join(", ")
                        } else if field.kind == "integer" {
                            value
                                .as_i64()
                                .map(|value| value.to_string())
                                .unwrap_or_default()
                        } else {
                            String::new()
                        }
                        .into(),
                        checked: value.as_bool().unwrap_or(false),
                        selected: field
                            .options
                            .iter()
                            .position(|option| option.value == *value)
                            .map(|n| n as i32)
                            .unwrap_or(-1),
                        options: Rc::new(VecModel::from(
                            field
                                .options
                                .iter()
                                .map(|o| SharedString::from(o.value.to_string()))
                                .collect::<Vec<_>>(),
                        ))
                        .into(),
                        labels: Rc::new(VecModel::from(
                            field
                                .options
                                .iter()
                                .map(|o| SharedString::from(text::tr(&o.label)))
                                .collect::<Vec<_>>(),
                        ))
                        .into(),
                        collection: field.collection.clone().unwrap_or_default().into(),
                        collection_label: field
                            .collection_label
                            .as_deref()
                            .map(text::tr)
                            .unwrap_or_default()
                            .into(),
                        collection_add_label: field
                            .collection_add_label
                            .as_deref()
                            .map(text::tr)
                            .unwrap_or_default()
                            .into(),
                        item: field.item.clone().unwrap_or_default().into(),
                        item_role: field.item_role.clone().unwrap_or_default().into(),
                        item_controller: field.item_controller.clone().unwrap_or_default().into(),
                        configured: configured(field),
                        has_editor: schema.iter().any(|candidate| {
                            candidate.collection == field.collection
                                && candidate.item_controller.as_deref() == Some(&field.key)
                                && matches!(candidate.item_role.as_deref(), Some("secret" | "url"))
                        }),
                    }
                };
                let mut rows = Vec::new();
                let mut groups = Vec::<String>::new();
                for field in &schema {
                    if let Some(group) = &field.collection
                        && !groups.contains(group)
                    {
                        groups.push(group.clone());
                    }
                }
                for group in groups {
                    let fields = schema
                        .iter()
                        .filter(|f| f.collection.as_deref() == Some(&group))
                        .collect::<Vec<_>>();
                    let Some(first) = fields.first() else {
                        continue;
                    };
                    // Structural rows must not inherit the first item's identity or state.
                    let header = nova_ui::PluginSettingRow {
                        kind: "collection-header".into(),
                        collection: group.clone().into(),
                        collection_label: text::tr(first.collection_label.as_deref().unwrap_or(""))
                            .into(),
                        ..Default::default()
                    };
                    rows.push(header);
                    for field in &fields {
                        if matches!(field.item_role.as_deref(), Some("member" | "enabled"))
                            && configured(field)
                        {
                            let mut row = make_row(field);
                            // Account removal clears the declared credential before disabling.
                            // Source rows keep their associated URL when removed.
                            if field.item_role.as_deref() == Some("enabled") {
                                row.options = Rc::new(VecModel::from(
                                    fields
                                        .iter()
                                        .filter(|candidate| {
                                            candidate.item == field.item
                                                && candidate.item_role.as_deref() == Some("secret")
                                        })
                                        .map(|candidate| SharedString::from(candidate.key.clone()))
                                        .collect::<Vec<_>>(),
                                ))
                                .into();
                            }
                            rows.push(row);
                        }
                    }
                    for field in &fields {
                        if matches!(field.item_role.as_deref(), Some("secret" | "url")) {
                            rows.push(make_row(field));
                        }
                    }
                    let mut keys = Vec::new();
                    let mut labels = Vec::new();
                    for field in &fields {
                        if matches!(field.item_role.as_deref(), Some("member" | "enabled"))
                            && !configured(field)
                        {
                            keys.push(SharedString::from(field.key.clone()));
                            labels.push(SharedString::from(text::tr(&field.label)));
                        }
                    }
                    if !keys.is_empty() {
                        let mut picker = nova_ui::PluginSettingRow {
                            kind: "collection-picker".into(),
                            collection: group.clone().into(),
                            label: text::tr(first.collection_add_label.as_deref().unwrap_or("Add"))
                                .into(),
                            ..Default::default()
                        };
                        picker.options = Rc::new(VecModel::from(keys)).into();
                        picker.labels = Rc::new(VecModel::from(labels)).into();
                        picker.selected = -1;
                        rows.push(picker);
                    }
                }
                if schema.iter().any(|field| field.collection.is_none()) {
                    rows.push(nova_ui::PluginSettingRow {
                        kind: "preferences-header".into(),
                        ..Default::default()
                    });
                }
                if managed {
                    rows.push(nova_ui::PluginSettingRow {
                        key: "nova.streamPreferences".into(),
                        kind: "host-preferences".into(),
                        label: text::tr("Profile and device stream preferences").into(),
                        summary: format!(
                            "{} {}",
                            text::tr("Managed in Settings → Player → Stream filters."),
                            text::stream_filter_summary(&self.effective_stream_filter_rules())
                        )
                        .into(),
                        ..Default::default()
                    });
                }
                for field in schema.iter().filter(|field| field.collection.is_none()) {
                    if managed
                        && matches!(
                            field.key.as_str(),
                            "quality.preferred"
                                | "language.prefs"
                                | "codec.prefs"
                                | "subtitle.prefs"
                                | "sort.order"
                        )
                    {
                        continue;
                    }
                    if field.kind == "ordered-choices" {
                        let value = snapshot.values.get(&field.key).unwrap_or(&field.default);
                        let chosen = value.as_array().cloned().unwrap_or_default();
                        let mut header = make_row(field);
                        header.kind = "priority-header".into();
                        rows.push(header);
                        for (index, choice) in chosen.iter().enumerate() {
                            let Some(option) =
                                field.options.iter().find(|option| option.value == *choice)
                            else {
                                continue;
                            };
                            let mut row = make_row(field);
                            row.kind = "priority-criterion".into();
                            row.item = choice.as_str().unwrap_or_default().into();
                            row.label = text::tr(&option.label).into();
                            row.selected = index as i32;
                            row.labels = Rc::new(VecModel::from(
                                chosen
                                    .iter()
                                    .map(|choice| SharedString::from(choice.to_string()))
                                    .collect::<Vec<_>>(),
                            ))
                            .into();
                            row.options = Rc::new(VecModel::from(["up", "down", "remove"].into_iter().map(|action| SharedString::from(serde_json::json!({"priorityAction":action,"choice":choice}).to_string())).collect::<Vec<_>>())).into();
                            rows.push(row);
                        }
                        let available = field
                            .options
                            .iter()
                            .filter(|option| !chosen.contains(&option.value))
                            .collect::<Vec<_>>();
                        if !available.is_empty() {
                            let mut picker = make_row(field);
                            picker.kind = "priority-picker".into();
                            picker.label = text::tr("Add preference").into();
                            picker.selected = -1;
                            picker.options = Rc::new(VecModel::from(available.iter().map(|option| SharedString::from(serde_json::json!({"priorityAction":"add","choice":option.value}).to_string())).collect::<Vec<_>>())).into();
                            picker.labels = Rc::new(VecModel::from(
                                available
                                    .iter()
                                    .map(|option| SharedString::from(text::tr(&option.label)))
                                    .collect::<Vec<_>>(),
                            ))
                            .into();
                            rows.push(picker);
                        }
                        continue;
                    }
                    if field.kind != "sort-order" {
                        rows.push(make_row(field));
                        continue;
                    }
                    let mut header = make_row(field);
                    header.kind = "sort-header".into();
                    rows.push(header);
                    let value = snapshot.values.get(&field.key).unwrap_or(&field.default);
                    for (index, criterion) in value.as_array().into_iter().flatten().enumerate() {
                        let key = criterion["key"].as_str().unwrap_or_default();
                        let Some(option) = field
                            .options
                            .iter()
                            .find(|option| option.value.as_str() == Some(key))
                        else {
                            continue;
                        };
                        let mut row = make_row(field);
                        row.kind = "sort-criterion".into();
                        row.item = key.into();
                        row.label = text::tr(&option.label).into();
                        row.selected = index as i32;
                        row.checked = criterion["direction"] == "desc";
                        row.options = Rc::new(VecModel::from(
                            ["up", "down", "toggle"]
                                .into_iter()
                                .map(|action| {
                                    SharedString::from(
                                        serde_json::json!({"sortAction":action,"criterion":key})
                                            .to_string(),
                                    )
                                })
                                .collect::<Vec<_>>(),
                        ))
                        .into();
                        rows.push(row);
                    }
                }
                Ok(rows)
            })();
        match rows {
            Ok(rows) => app.set_plugin_setting_rows(Rc::new(VecModel::from(rows)).into()),
            Err(error) => app.set_plugin_settings_notice(error.to_string().into()),
        }
    }
    pub(super) fn change_plugin_setting(&self, url: &str, key: &str, raw: &str) {
        let result = (|| {
            let plugin = plugin_id(url).ok_or_else(|| provider_error("invalid provider URL"))?;
            let schema = nova_providers::provider_settings_schema(url).unwrap_or_default();
            let field = schema
                .iter()
                .find(|f| f.key == key)
                .ok_or_else(|| provider_error("unknown configuration field"))?;
            let value = if matches!(field.kind.as_str(), "string" | "secret") {
                serde_json::Value::String(raw.into())
            } else {
                let parsed: serde_json::Value =
                    serde_json::from_str(raw).map_err(provider_error)?;
                if field.kind == "sort-order" && parsed.is_object() {
                    let snapshot = service::settings_snapshot(plugin, &schema)?;
                    field.apply_sort_action(
                        snapshot.values.get(key).unwrap_or(&field.default),
                        &parsed,
                    )?
                } else if field.kind == "ordered-choices" && parsed.is_object() {
                    let snapshot = service::settings_snapshot(plugin, &schema)?;
                    field.apply_priority_action(
                        snapshot.values.get(key).unwrap_or(&field.default),
                        &parsed,
                    )?
                } else {
                    parsed
                }
            };
            field.validate(&value)?;
            service::set_setting(plugin, key, value)
        })();
        self.plugin_settings_result(url, result);
    }
    pub(super) fn clear_plugin_settings(&self, url: &str, cache_only: bool) {
        let result = plugin_id(url)
            .ok_or_else(|| provider_error("invalid provider URL"))
            .and_then(|plugin| {
                if cache_only {
                    service::cache_clear(plugin)
                } else {
                    service::clear_settings(plugin)
                }
            });
        self.plugin_settings_result(url, result);
    }
    fn plugin_settings_result(
        &self,
        url: &str,
        result: Result<(), nova_providers::ProviderHostError>,
    ) {
        if let Some(app) = self.app() {
            app.set_plugin_settings_notice(
                result
                    .err()
                    .map(|e| e.to_string())
                    .unwrap_or_default()
                    .into(),
            );
        }
        self.refresh_plugin_settings_ui(url);
    }
}

#[cfg(test)]
mod convergence_tests {
    use super::*;
    fn exchange(a: &mut nova_sync::Store, b: &mut nova_sync::Store) {
        let outgoing_a = a.outbound(&b.digest());
        let outgoing_b = b.outbound(&a.digest());
        for item in outgoing_a {
            b.apply(&item.domain, &item.key, item.record);
        }
        for item in outgoing_b {
            a.apply(&item.domain, &item.key, item.record);
        }
    }
    #[test]
    fn two_peer_hlc_edits_merge_fields_and_deletion_does_not_resurrect() {
        let domain = sync::DOMAIN_PLUGIN_SETTINGS;
        let mut a = nova_sync::Store::default();
        let mut b = nova_sync::Store::default();
        for (key, value, _) in
            records(r#"{"example":{"enabled":false,"token":"old","future":{"x":1}}}"#).unwrap()
        {
            a.set(domain, &key, Some(value), 0, 1);
        }
        exchange(&mut a, &mut b);
        // Concurrent edits target independent record keys, preserving both.
        a.set(domain, "example/enabled", Some("true".into()), 0, 1);
        b.set(domain, "example/token", Some("\"new\"".into()), 0, 2);
        exchange(&mut a, &mut b);
        let values = project(a.records(domain)).unwrap();
        assert_eq!(values["example"]["enabled"], true);
        assert_eq!(values["example"]["token"], "new");
        assert_eq!(values["example"]["future"], serde_json::json!({"x":1}));
        assert_eq!(project(b.records(domain)).unwrap(), values);
        // Keep a stale replica, then explicitly clear every known/unknown field.
        let stale = b.clone();
        for (key, _) in a.records(domain) {
            a.set(domain, &key, None, 0, 1);
        }
        exchange(&mut a, &mut b);
        assert!(project(a.records(domain)).unwrap().is_empty());
        assert!(project(b.records(domain)).unwrap().is_empty());
        for item in stale.outbound(&Default::default()) {
            assert!(!a.apply(&item.domain, &item.key, item.record));
        }
        assert!(project(a.records(domain)).unwrap().is_empty());
        assert!(a.record(domain, "example/future").unwrap().is_deleted());
    }
}

#[cfg(test)]
mod startup_tests {
    use super::*;
    #[test]
    fn retained_unfamiliar_domain_restores_before_seed_without_echo() {
        const ROOT: &str = "NOVA_PLUGIN_SETTINGS_RESTORE_TEST_ROOT";
        let Some(root) = std::env::var_os(ROOT) else {
            let root = std::env::temp_dir().join(format!(
                "nova-plugin-restore-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(ROOT,&root)
                .env("XDG_CACHE_HOME", root.join("cache"))
                .env("LOCALAPPDATA", root.join("cache"))
                .env("NOVA_CACHE_DIR", root.join("cache"))
                .args(["--exact","app::plugin_settings::startup_tests::retained_unfamiliar_domain_restores_before_seed_without_echo","--nocapture"])
                .output().unwrap();
            let _ = std::fs::remove_dir_all(root);
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        storage::init_at(&PathBuf::from(root));
        let domain = sync::DOMAIN_PLUGIN_SETTINGS;
        let owner = nova_sync::local_store().unwrap();
        let basis = {
            let mut store = owner.lock().unwrap();
            store.set(
                domain,
                "futureplugin/future",
                Some(r#"{"x":1}"#.into()),
                0,
                2,
            );
            store.set(domain, "futureplugin/deleted", Some("true".into()), 0, 2);
            store.set(domain, "futureplugin/deleted", None, 0, 2);
            store.save().unwrap();
            let basis = store.digest()[domain].clone();
            // This models an older application's unknown-domain acknowledgement.
            store.mark_projected(domain, &basis).unwrap();
            assert!(!store.pending_domains().contains(&domain.to_string()));
            basis
        };
        assert!(storage::try_get_str(KEY).unwrap().is_none());
        service::configure_plugin_settings(Arc::new(Backend::new())).unwrap();
        restore_retained_settings().unwrap();
        let raw = storage::try_get_str(KEY).unwrap().unwrap();
        let values = service::parse_settings(&raw).unwrap();
        assert_eq!(values["futureplugin"]["future"], serde_json::json!({"x":1}));
        assert!(!values["futureplugin"].contains_key("deleted"));
        assert!(
            service::settings_snapshot("futureplugin", &[])
                .unwrap()
                .values
                .is_empty()
        );
        assert!(
            storage::try_get_str(&format!("sync:baseline:{domain}"))
                .unwrap()
                .is_some()
        );
        // The immediately following startup seed cannot manufacture local edits.
        persist_sync_snapshot(KEY, &raw, true).unwrap();
        assert_eq!(owner.lock().unwrap().digest()[domain], basis);
        assert!(
            owner
                .lock()
                .unwrap()
                .record(domain, "futureplugin/deleted")
                .unwrap()
                .is_deleted()
        );
        let generation = service::settings_generation();
        restore_retained_settings().unwrap();
        assert_eq!(service::settings_generation(), generation);

        let incoming = profiles::ProfileStore::for_profile("incoming");
        let incoming_domain = incoming.domain(sync::DOMAIN_PLUGIN_SETTINGS);
        {
            let mut store = owner.lock().unwrap();
            store.set(
                &incoming_domain,
                "futureplugin/token",
                Some(r#""incoming-token""#.into()),
                0,
                3,
            );
            store.save().unwrap();
        }
        let _prepared = prepare_backend(incoming.clone()).unwrap();
        assert_eq!(
            service::settings_generation(),
            generation,
            "preflight never replaces the active provider owner"
        );
        let recovered = service::parse_settings(&incoming.read(KEY).unwrap().unwrap()).unwrap();
        assert_eq!(recovered["futureplugin"]["token"], "incoming-token");
        assert!(!service::parse_settings(&storage::try_get_str(KEY).unwrap().unwrap()).unwrap()["futureplugin"].contains_key("token"));

        incoming
            .write(
                "addons",
                r#"[{"url":"nova-provider://futureplugin","enabled":true,"label":"First"}]"#,
            )
            .unwrap();
        let backend = Backend::for_store(incoming.clone());
        let original = backend
            .provider_revision("nova-provider://futureplugin")
            .unwrap()
            .unwrap();
        incoming
            .write(
                "addons",
                r#"[{"url":"nova-provider://futureplugin","enabled":true,"label":"Renamed"}]"#,
            )
            .unwrap();
        assert_eq!(
            backend
                .provider_revision("nova-provider://futureplugin")
                .unwrap()
                .as_deref(),
            Some(original.as_str())
        );
        // A remote cycle can restore identical projected bytes between two
        // authority checks. Its record clocks must still invalidate the old
        // capability for addon membership, package and credentials separately.
        let mut original = original;
        for (domain, key, value) in [
            (
                sync::DOMAIN_ADDONS,
                "nova-provider://futureplugin",
                r#"{"enabled":true}"#,
            ),
            (sync::DOMAIN_PACKAGES, "futureplugin", "{}"),
            (
                sync::DOMAIN_PLUGIN_SETTINGS,
                "futureplugin/token",
                r#""incoming-token""#,
            ),
        ] {
            {
                let mut store = owner.lock().unwrap();
                let domain = incoming.domain(domain);
                store.set(&domain, key, Some(value.into()), 0, 3);
                store.set(&domain, key, None, 0, 3);
                store.set(&domain, key, Some(value.into()), 0, 3);
                store.save().unwrap();
            }
            let revised = backend
                .provider_revision("nova-provider://futureplugin")
                .unwrap()
                .unwrap();
            assert_ne!(
                revised, original,
                "{domain} removal must revoke identical re-adds"
            );
            original = revised;
        }
        {
            let mut store = owner.lock().unwrap();
            store.set(
                &incoming.domain(sync::DOMAIN_PLUGIN_SETTINGS),
                "otherplugin/token",
                Some("true".into()),
                0,
                3,
            );
        }
        assert_eq!(
            backend
                .provider_revision("nova-provider://futureplugin")
                .unwrap()
                .as_deref(),
            Some(original.as_str())
        );
        incoming
            .write(KEY, r#"{"futureplugin":{"token":"revoked-token"}}"#)
            .unwrap();
        assert_ne!(
            backend
                .provider_revision("nova-provider://futureplugin")
                .unwrap()
                .as_deref(),
            Some(original.as_str())
        );
        incoming.write("addons", "[]").unwrap();
        assert!(
            backend
                .provider_revision("nova-provider://futureplugin")
                .unwrap()
                .is_none()
        );
    }
}
