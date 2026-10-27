//! Per-device setting overrides. Account records remain authoritative even
//! while a device uses a different effective value.
use super::*;
use serde_json::Value;

impl Bridge {
    pub(super) fn bind_setting_sync(&self) {
        let Some(app) = self.app() else {
            return;
        };
        let weak = app.as_weak();
        app.global::<crate::SettingsSync>().on_lookup(move |key| {
            weak.upgrade()
                .map(|app| {
                    app.global::<crate::SettingsSync>()
                        .get_rows()
                        .iter()
                        .position(|row| row.key == key)
                        .map(|i| i as i32)
                        .unwrap_or(-1)
                })
                .unwrap_or(-1)
        });
        let b = self.clone();
        app.global::<crate::SettingsSync>()
            .on_requested(move || b.refresh_setting_sync());
        let b = self.clone();
        app.global::<crate::SettingsSync>()
            .on_override_changed(move |key, enabled| {
                b.set_setting_override(key.as_str(), enabled);
            });
        self.refresh_setting_sync();
    }

    pub(super) fn refresh_setting_sync(&self) {
        let Some(app) = self.app() else {
            return;
        };
        let mut settings = self.shared.lock().unwrap().cache_settings.clone();
        stream_filters::migrate_overrides(&mut settings);
        let values = stream_filters::settings_value(&settings);
        let records = sync::account_setting_records();
        let mut rows = Vec::new();
        if let Some(fields) = values.as_object() {
            for (key, value) in fields {
                if matches!(
                    key.as_str(),
                    "sync_overrides"
                        | "categories"
                        | "home_recommendations_migrated"
                        | "home_recommendations_hidden"
                ) {
                    continue;
                }
                let synced = sync::setting_can_override(key);
                let shared = sync::shared_setting_value(&records, key);
                let restore_value = shared.as_ref().or_else(|| settings.sync_overrides.get(key));
                let shared_supported = restore_value.is_none_or(|v| {
                    let mut trial = values.clone();
                    trial[key] = v.clone();
                    stream_filters::settings_from_value(trial).is_ok()
                });
                rows.push(crate::SettingSyncInfo {
                    key: key.into(),
                    status: if !synced {
                        0
                    } else if settings.sync_overrides.contains_key(key) {
                        2
                    } else {
                        1
                    },
                    local_value: text::setting_sync_value(key, value).into(),
                    shared_value: shared
                        .as_ref()
                        .map(|v| text::setting_sync_value(key, v))
                        .unwrap_or_else(|| text::tr("Not received yet").into())
                        .into(),
                    can_override: synced,
                    has_shared: shared.is_some(),
                    shared_supported,
                });
            }
        }
        // P2P, sync configuration and offline-download policy already have
        // independent local stores; they cannot be switched into account sync.
        let local = [
            ("torrent_enabled", Value::Bool(app.get_torrent_enabled())),
            (
                "torrent_dir",
                Value::String(app.get_torrent_dir().to_string()),
            ),
            ("torrent_max_mb", Value::from(app.get_torrent_max_mb())),
            (
                "torrent_down_limit",
                Value::from(app.get_torrent_down_limit()),
            ),
            ("torrent_no_cache", Value::Bool(app.get_torrent_no_cache())),
            (
                "torrent_ram_buffer_mb",
                Value::from(app.get_torrent_ram_buffer_mb()),
            ),
            ("sync_enabled", Value::Bool(app.get_sync_enabled())),
            (
                "sync_device_name",
                Value::String(app.get_sync_device_name().to_string()),
            ),
            ("sync_interval", Value::from(app.get_sync_interval_index())),
            ("sync_background", Value::Bool(app.get_sync_background())),
            (
                "sync_local_discovery",
                Value::Bool(app.get_sync_local_discovery()),
            ),
            (
                "sync_pair_confirm",
                Value::Bool(app.get_sync_pair_confirm()),
            ),
            (
                "download_auto_delete_watched",
                Value::Bool(app.get_download_auto_delete_watched()),
            ),
        ];
        rows.extend(
            local
                .into_iter()
                .map(|(key, value)| crate::SettingSyncInfo {
                    key: key.into(),
                    status: 0,
                    local_value: text::setting_sync_value(key, &value).into(),
                    ..Default::default()
                }),
        );
        for account in app.get_tracking_accounts().iter() {
            let suffix = account.service.to_string();
            rows.push(crate::SettingSyncInfo {
                key: format!("tracking_automatic_{suffix}").into(),
                status: 1,
                can_override: false,
                local_value: text::setting_sync_value("automatic", &Value::Bool(account.automatic))
                    .into(),
                shared_value: text::setting_sync_value(
                    "automatic",
                    &Value::Bool(account.automatic),
                )
                .into(),
                has_shared: true,
                shared_supported: true,
            });
            for (name, value) in [
                ("client", account.client_id),
                ("redirect", account.redirect_uri),
            ] {
                rows.push(crate::SettingSyncInfo {
                    key: format!("tracking_{name}_{suffix}").into(),
                    status: 0,
                    local_value: value,
                    ..Default::default()
                });
            }
        }
        app.global::<crate::SettingsSync>()
            .set_rows(Rc::new(VecModel::from(rows)).into());
    }

    pub(super) fn set_setting_override(&self, field: &str, enabled: bool) {
        if !self.require_profile_management() {
            return;
        }
        if !sync::setting_can_override(field) || !writable_key("settings") {
            return;
        }
        // Flush any pending editor value before changing policy. Enabling an
        // override changes policy only: it must never stamp a shared record.
        let _guard = sync::ApplyingGuard::new();
        self.capture_settings();
        let mut settings = self.shared.lock().unwrap().cache_settings.clone();
        stream_filters::migrate_overrides(&mut settings);
        let values = stream_filters::settings_value(&settings);
        let records = sync::account_setting_records();
        let shared = sync::shared_setting_value(&records, field);
        if enabled {
            let Some(baseline) = shared.or_else(|| values.get(field).cloned()) else {
                return;
            };
            settings
                .sync_overrides
                .entry(field.into())
                .or_insert(baseline);
        } else {
            let Some(fallback) = settings.sync_overrides.get(field).cloned() else {
                return;
            };
            let mut values = values;
            values[field] = shared.unwrap_or(fallback);
            if field == "home_rows" {
                values["home_recommendations_migrated"] =
                    sync::shared_setting_value(&records, "home_recommendations_migrated")
                        .unwrap_or(serde_json::Value::Bool(false));
            }
            let Ok(mut restored) = stream_filters::settings_from_value(values) else {
                return;
            };
            restored.sync_overrides.remove(field);
            settings = restored;
        }
        self.shared.lock().unwrap().cache_settings = settings.clone();
        set_active_cache_settings(settings.clone());
        // This is a local policy change, even though publication is guarded.
        self.player.update_playback_defaults(&settings);
        self.refresh_effective_speed();
        write_settings(&settings);
        self.settings_to_ui();
        self.refresh_setting_sync();
        self.apply_stream_filter();
    }
}
