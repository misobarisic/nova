//! Device-local first-run eligibility. Probe before startup writes any defaults.
use super::*;

const PENDING_KEY: &str = "onboarding:pending:v1";
const COMPLETED_KEY: &str = "onboarding:completed:v1";

/// A database which already existed may belong to an older version, even when
/// all its user state is empty. Only a pending marker created by this version
/// permits resuming that database's onboarding; unreadable state always wins.
fn eligible(database_existed: bool, rows: Result<Vec<(String, String)>, storage::Error>) -> bool {
    let Ok(rows) = rows else { return false };
    if rows.iter().any(|(key, _)| key == COMPLETED_KEY) {
        return false;
    }
    if let Some((_, pending)) = rows.iter().find(|(key, _)| key == PENDING_KEY) {
        // Pending setup may already contain preferences or explicitly added
        // addons. Retain them, but never offer defaults over malformed state.
        let readable = !rows
            .iter()
            .any(|(key, _)| key.starts_with("sync:records.corrupt."))
            && rows.iter().all(|(key, value)| match key.as_str() {
                "settings" => serde_json::from_str::<serde_json::Value>(value)
                    .ok()
                    .and_then(settings::recover_settings)
                    .is_some(),
                "addons" => serde_json::from_str::<Vec<AddonStore>>(value).is_ok(),
                "library" => serde_json::from_str::<Vec<LibraryEntry>>(value).is_ok(),
                "sync:records" => serde_json::from_str::<serde_json::Value>(value).is_ok(),
                "sync:records:hlc" => serde_json::from_str::<nova_sync::Hlc>(value).is_ok(),
                _ if key.starts_with("srec:") => {
                    serde_json::from_str::<nova_sync::Record>(value).is_ok()
                }
                _ => true,
            });
        return pending == "1" && readable;
    }
    !database_existed && rows.is_empty()
}

pub(super) fn begin(database_existed: bool) -> bool {
    // Ordinary upgrades/completed installs need one small read, rather than
    // decoding every cached metadata row just to decide not to show setup.
    if database_existed
        && !matches!(storage::try_get_str(PENDING_KEY), Ok(Some(value)) if value == "1")
    {
        return false;
    }
    if !eligible(database_existed, storage::try_scan_prefix("")) {
        return false;
    }
    // A failed durable pending write must never present a setup UI which
    // might overwrite storage the app could not read reliably.
    match storage::try_set_str(PENDING_KEY, "1") {
        Ok(()) => true,
        Err(error) => {
            storage::report(error);
            false
        }
    }
}

fn complete() -> Result<(), storage::Error> {
    storage::try_write_batch(&[
        (COMPLETED_KEY.into(), Some("1".into())),
        (PENDING_KEY.into(), None),
    ])
}

/// New onboarding installs must retain explicit empty addon choices on later
/// boots, while older installs keep their existing source migration behavior.
pub(super) fn owns_addon_setup() -> bool {
    [PENDING_KEY, COMPLETED_KEY]
        .iter()
        .any(|key| matches!(storage::try_get_str(key), Ok(Some(_))))
}

impl Bridge {
    pub(super) fn finish_onboarding(&self) {
        let Some(app) = self.app() else { return };
        if !writable_key("settings") {
            app.set_onboarding_error(
                text::tr("Could not save your preferences. Please try again.").into(),
            );
            return;
        }
        let failures = write_failures();
        self.capture_settings();
        self.persist_settings();
        if failures != write_failures() {
            app.set_onboarding_error(
                text::tr("Could not save your preferences. Please try again.").into(),
            );
            return;
        }
        // Completion is deliberately outside the setting/sync snapshot path.
        match complete() {
            Ok(()) => app.set_onboarding_visible(false),
            Err(error) => {
                storage::report(error);
                app.set_onboarding_error(
                    text::tr("Could not save your preferences. Please try again.").into(),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(values: &[(&str, &str)]) -> Result<Vec<(String, String)>, storage::Error> {
        Ok(values
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect())
    }

    #[test]
    fn fresh_and_upgrade_empty_databases_are_distinct() {
        assert!(eligible(false, rows(&[])));
        assert!(!eligible(true, rows(&[])));
        for key in [
            "settings",
            "addons",
            "library",
            "sync:records",
            "unknown:future",
        ] {
            assert!(!eligible(false, rows(&[(key, "{}")])));
        }
    }

    #[test]
    fn pending_resumes_real_settings_but_completion_and_corruption_win() {
        assert!(eligible(
            true,
            rows(&[(PENDING_KEY, "1"), ("settings", "{}")])
        ));
        assert!(!eligible(
            true,
            rows(&[(PENDING_KEY, "1"), (COMPLETED_KEY, "1")])
        ));
        assert!(!eligible(true, rows(&[(PENDING_KEY, "garbled")])));
        for key in ["settings", "addons", "library"] {
            assert!(!eligible(
                true,
                rows(&[(PENDING_KEY, "1"), (key, "broken")])
            ));
        }
        assert!(!eligible(
            false,
            Err(storage::Error::new(storage::ErrorKind::Schema, "corrupt"))
        ));
    }

    #[test]
    fn pending_requires_readable_current_sync_records_and_clock() {
        let clock = serde_json::to_string(&nova_sync::Hlc::new(1234, 2)).unwrap();
        let record = serde_json::to_string(&nova_sync::Record {
            value: Some("{}".into()),
            version: nova_sync::Version::new(1234, 2, 7, false),
        })
        .unwrap();
        assert!(eligible(
            true,
            rows(&[
                (PENDING_KEY, "1"),
                ("srec:8:settingsgeneral", &record),
                ("sync:records:hlc", &clock),
            ])
        ));
        for key in ["srec:8:settingsgeneral", "sync:records:hlc"] {
            assert!(!eligible(true, rows(&[(PENDING_KEY, "1"), (key, "{}")])));
            assert!(!eligible(
                true,
                rows(&[(PENDING_KEY, "1"), (key, "broken")])
            ));
        }
    }

    #[test]
    fn completion_is_durable_and_pending_choices_resume() {
        const CHILD: &str = "NOVA_ONBOARDING_STORAGE_TEST";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "app::onboarding::tests::completion_is_durable_and_pending_choices_resume",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        let directory =
            std::env::temp_dir().join(format!("nova-onboarding-test-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        storage::init_at(&directory);
        assert!(begin(false));
        assert!(owns_addon_setup());
        storage::try_set_str(
            "settings",
            &serde_json::to_string(&CacheSettings {
                default_audio_language: "en,ja".into(),
                default_subtitle_language: "hr,en".into(),
                ..Default::default()
            })
            .unwrap(),
        )
        .unwrap();
        assert!(begin(true));
        complete().unwrap();
        assert_eq!(
            storage::try_get_str(COMPLETED_KEY).unwrap().as_deref(),
            Some("1")
        );
        assert_eq!(storage::try_get_str(PENDING_KEY).unwrap(), None);
        assert!(!begin(true));
        assert!(owns_addon_setup());
        let saved: CacheSettings =
            serde_json::from_str(&storage::try_get_str("settings").unwrap().unwrap()).unwrap();
        assert_eq!(saved.default_audio_language, "en,ja");
        assert_eq!(saved.default_subtitle_language, "hr,en");
    }
}
