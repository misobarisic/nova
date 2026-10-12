//! App-owned plugin preferences share the atomic sync snapshot transaction.
use super::*;
use nova_providers::plugin_settings::{self as service, PluginSettingsBackend, SettingsMap};
pub(super) const KEY: &str = "plugin_settings";
const CACHE_KEY: &str = "plugin_cache:v1";
pub(super) struct Backend;
impl PluginSettingsBackend for Backend {
    fn load_settings(&self) -> Result<Option<String>, nova_providers::ProviderHostError> {
        storage::try_get_str(KEY).map_err(provider_error)
    }
    fn save_settings(&self, raw: &str) -> Result<(), nova_providers::ProviderHostError> {
        persist_sync_snapshot(KEY, raw, false).map_err(provider_error)
    }
    fn load_cache(&self) -> Result<Option<String>, nova_providers::ProviderHostError> {
        storage::try_get_str(CACHE_KEY).map_err(provider_error)
    }
    fn save_cache(&self, raw: &str) -> Result<(), nova_providers::ProviderHostError> {
        storage::try_set_cached_str(CACHE_KEY, raw).map_err(provider_error)
    }
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
        .any(|domain| domain == sync::DOMAIN_PLUGIN_SETTINGS)
    {
        return Ok(());
    }
    let _guard = sync::ApplyingGuard::new();
    service::apply_settings_projection(|| {
        let values = project(owner.lock().unwrap().records(sync::DOMAIN_PLUGIN_SETTINGS))?;
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
                Ok(schema
                    .into_iter()
                    .map(|field| {
                        let value = snapshot.values.get(&field.key).unwrap_or(&field.default);
                        nova_ui::PluginSettingRow {
                            key: field.key.clone().into(),
                            label: text::tr(&field.label).into(),
                            kind: field.kind.into(),
                            value: value
                                .as_str()
                                .map(str::to_owned)
                                .unwrap_or_else(|| value.to_string())
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
                        }
                    })
                    .collect())
            })();
        match rows {
            Ok(rows) => app.set_plugin_setting_rows(Rc::new(VecModel::from(rows)).into()),
            Err(error) => app.set_plugin_settings_notice(error.to_string().into()),
        }
    }
    pub(super) fn change_plugin_setting(&self, url: &str, key: &str, raw: &str) {
        let result = (|| {
            let plugin = plugin_id(url).ok_or_else(|| provider_error("invalid provider URL"))?;
            let field = nova_providers::provider_settings_schema(url)
                .unwrap_or_default()
                .into_iter()
                .find(|f| f.key == key)
                .ok_or_else(|| provider_error("unknown configuration field"))?;
            let value = if matches!(field.kind.as_str(), "string" | "secret") {
                serde_json::Value::String(raw.into())
            } else {
                serde_json::from_str(raw).map_err(provider_error)?
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
        service::configure_plugin_settings(Arc::new(Backend)).unwrap();
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
    }
}
