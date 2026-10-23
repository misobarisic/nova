use super::*;
use serde_json::json;

fn payload(rows: &[(&str, serde_json::Value)]) -> Payload {
    Payload {
        version: 1,
        records: rows
            .iter()
            .map(|(key, value)| ((*key).into(), value.clone()))
            .collect(),
    }
}

#[test]
fn recovery_order_uses_local_sequence_across_clock_rollback_and_decimal_boundaries() {
    let mut keys = vec![
        format!("{RECOVERY_PREFIX}2000-9"),
        format!("{RECOVERY_PREFIX}1000-10"),
        format!("{RECOVERY_PREFIX}3000-8"),
    ];
    keys.sort_by_key(|key| recovery_sequence(key));
    assert!(keys.last().unwrap().ends_with("1000-10"));
    assert_eq!(
        keys.iter()
            .map(|key| recovery_sequence(key))
            .collect::<Vec<_>>(),
        vec![8, 9, 10]
    );
}
fn manifest() -> archive::Manifest {
    archive::Manifest {
        format: "nova-backup".into(),
        version: 1,
        created_at: 1,
        app_version: "test".into(),
        archive_id: "archive".into(),
        packages: Vec::new(),
        profiles: vec![archive::Profile {
            archive_profile_id: "p1".into(),
            source_profile_id: "default".into(),
            name: "Default".into(),
            components: vec![archive::Component {
                name: "policy".into(),
                schema_version: 1,
                path: "profiles/p1/policy.json".into(),
                item_count: 0,
                sha256: String::new(),
            }],
        }],
    }
}

#[test]
fn plaintext_pin_and_selected_credentials_round_trip_without_device_identity() {
    let policy = payload(&[(
        "profile_policy:v1\0value",
        json!({"version":1,"pin":"0012","restricted":true,"require_pin_on_entry":true,"allowed_titles":[],"allowed_addons":[],"future":true}),
    )]);
    let mut manifest = manifest();
    manifest.profiles[0].components.push(archive::Component {
        name: "integrations".into(),
        schema_version: 1,
        path: "profiles/p1/integrations.json".into(),
        item_count: 0,
        sha256: String::new(),
    });
    let integrations = payload(&[(
        "@stremio\0connection",
        json!({"auth_key":"plain-token","email":"name@example.test"}),
    )]);
    let payloads = BTreeMap::from([
        (("p1".into(), "policy".into()), policy.clone()),
        (("p1".into(), "integrations".into()), integrations.clone()),
    ]);
    let bytes = archive::encode(manifest, &payloads).unwrap();
    let decoded = archive::decode(&bytes).unwrap();
    assert_eq!(
        decoded.payloads[&("p1".into(), "policy".into())].records,
        policy.records
    );
    assert_eq!(
        decoded.payloads[&("p1".into(), "integrations".into())].records,
        integrations.records
    );
    assert!(
        !decoded
            .payloads
            .keys()
            .any(|(_, component)| component.contains("identity"))
    );
}

#[test]
fn corrupt_missing_unsupported_and_duplicate_archive_entries_are_distinct() {
    use std::io::{Cursor, Write};
    let policy = payload(&[(
        "profile_policy:v1\0value",
        json!({"version":1,"pin":"","restricted":false,"require_pin_on_entry":false,"allowed_titles":[],"allowed_addons":[]}),
    )]);
    let payloads = BTreeMap::from([(("p1".into(), "policy".into()), policy)]);
    let bytes = archive::encode(manifest(), &payloads).unwrap();
    let mut zip = zip::ZipArchive::new(Cursor::new(&bytes)).unwrap();
    let mut manifest_bytes = String::new();
    use std::io::Read;
    zip.by_name("manifest.json")
        .unwrap()
        .read_to_string(&mut manifest_bytes)
        .unwrap();
    let original: serde_json::Value = serde_json::from_str(&manifest_bytes).unwrap();
    let build = |manifest: serde_json::Value, component: Option<&[u8]>| {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("manifest.json", options).unwrap();
        zip.write_all(manifest.to_string().as_bytes()).unwrap();
        if let Some(component) = component {
            zip.start_file("profiles/p1/policy.json", options).unwrap();
            zip.write_all(component).unwrap();
        }
        zip.finish().unwrap().into_inner()
    };
    assert!(
        archive::decode(&build(original.clone(), None))
            .err()
            .unwrap()
            .contains("Missing backup component")
    );
    assert!(
        archive::decode(&build(original.clone(), Some(b"{}")))
            .err()
            .unwrap()
            .contains("Checksum")
    );
    let mut newer = original.clone();
    newer["version"] = json!(2);
    assert!(
        archive::decode(&build(newer, None))
            .err()
            .unwrap()
            .contains("Unsupported backup")
    );
    let mut duplicate = original;
    let component = duplicate["profiles"][0]["components"][0].clone();
    duplicate["profiles"][0]["components"]
        .as_array_mut()
        .unwrap()
        .push(component);
    assert!(archive::decode(&build(duplicate, None)).is_err());
}

#[test]
fn library_typed_matching_and_default_merge_counts_do_not_use_raw_ids() {
    let local = payload(&[(
        "library\0[\"movie\",\"same\"]",
        json!({"type_":"movie","id":"same","name":"Local","categories":["A"]}),
    )]);
    let backup = payload(&[
        (
            "library\0[\"movie\",\"same\"]",
            json!({"type_":"movie","id":"same","name":"Backup","categories":["B"]}),
        ),
        (
            "library\0[\"series\",\"same\"]",
            json!({"type_":"series","id":"same","name":"Series","categories":[]}),
        ),
    ]);
    let merged = adapters::merge("library", &local, &backup, false, false).unwrap();
    assert_eq!(merged.records.len(), 2);
    assert_eq!(
        merged.records["library\0[\"movie\",\"same\"]"]["name"],
        "Local"
    );
    assert_eq!(
        merged.records["library\0[\"movie\",\"same\"]"]["categories"],
        json!(["A", "B"])
    );
    let counts = count(&local, &backup, &merged);
    assert_eq!(
        (
            counts.archived,
            counts.added,
            counts.conflicts,
            counts.updated
        ),
        (2, 1, 1, 1)
    );
    let replacement = adapters::merge(
        "library",
        &local,
        &Payload {
            version: 1,
            ..Default::default()
        },
        true,
        false,
    )
    .unwrap();
    assert!(replacement.records.is_empty());
    assert_eq!(count(&local, &Payload::default(), &replacement).deleted, 1);
}

#[test]
fn missing_queue_merge_keeps_archived_order_cursor_and_autoplay_without_starting_a_run() {
    let key = json!(["series", "show", "ep"]).to_string();
    let value = json!({"version":2,"entries":[{"type_":"series","series_id":"show","episode_id":"ep","title":"Show","poster_url":"","episode_title":"Episode","season":1,"episode":1}],"order":[key],"cursor":{"item_key":key,"phase":"completed","last_completed_key":key,"origin_device":7},"autoplay":true});
    let source = adapters::capture(
        "queue",
        &ProfileStore::for_profile("test"),
        &BTreeMap::from([(app::watch_queue::KEY.into(), value.to_string())]),
        &nova_sync::Store::default(),
    )
    .unwrap();
    let merged = adapters::normalize(
        "queue",
        adapters::merge(
            "queue",
            &Payload {
                version: 1,
                ..Default::default()
            },
            &source,
            false,
            false,
        )
        .unwrap(),
    )
    .unwrap();
    let edits = adapters::materialize("queue", &merged, &BTreeMap::new()).unwrap();
    let restored: serde_json::Value =
        serde_json::from_str(edits[app::watch_queue::KEY].as_ref().unwrap()).unwrap();
    assert_eq!(restored["order"], value["order"]);
    assert_eq!(restored["cursor"], value["cursor"]);
    assert_eq!(restored["autoplay"], true);
    assert!(restored.get("run_id").is_none());
}

#[test]
fn selected_settings_never_overwrite_unselected_collections_categories_or_paths() {
    let before=BTreeMap::from([("settings".into(),json!({"categories":["Local"],"smart_collections":[{"id":"retained"}],"screenshot_folder":"/local/path","future_unknown":{"x":1}}).to_string())]);
    let backup = payload(&[("settings\0auto_next_episode", json!(true))]);
    let edits = adapters::materialize("settings", &backup, &before).unwrap();
    let after: serde_json::Value =
        serde_json::from_str(edits["settings"].as_ref().unwrap()).unwrap();
    assert_eq!(after["categories"], json!(["Local"]));
    assert_eq!(after["smart_collections"][0]["id"], "retained");
    assert_eq!(after["screenshot_folder"], "/local/path");
    assert_eq!(after["future_unknown"], json!({"x":1}));
    assert_eq!(after["auto_next_episode"], true);
    assert!(
        adapters::validate(
            "settings",
            &payload(&[("settings\0screenshot_folder", json!("/import/path"))])
        )
        .is_err()
    );
}

#[test]
fn absent_settings_and_policy_are_missing_and_integration_keys_have_independent_selection() {
    let owner = ProfileStore::for_profile("test");
    let store = nova_sync::Store::default();
    for name in ["settings", "policy", "categories", "collections"] {
        assert!(
            adapters::capture(name, &owner, &BTreeMap::new(), &store)
                .unwrap()
                .records
                .is_empty(),
            "{name} defaults are not local user records"
        );
    }
    let rows = BTreeMap::from([(
        "settings".into(),
        json!({"tmdb_api_key":"plain-api-key","auto_next_episode":true}).to_string(),
    )]);
    let settings = adapters::capture("settings", &owner, &rows, &store).unwrap();
    assert!(!settings.records.contains_key("settings\0tmdb_api_key"));
    let integrations = adapters::capture("integrations", &owner, &rows, &store).unwrap();
    assert_eq!(
        integrations.records["settings\0tmdb_api_key"],
        "plain-api-key"
    );
    let before = BTreeMap::from([(
        "settings".into(),
        json!({"tmdb_api_key":"local-key","auto_next_episode":false}).to_string(),
    )]);
    let edits = adapters::materialize("integrations", &integrations, &before).unwrap();
    let restored: serde_json::Value =
        serde_json::from_str(edits["settings"].as_ref().unwrap()).unwrap();
    assert_eq!(restored["tmdb_api_key"], "plain-api-key");
    assert_eq!(restored["auto_next_episode"], false);
    assert!(
        !adapters::integration_domains(&integrations)
            .unwrap()
            .contains_key("settings")
    );
}

#[test]
fn explicit_empty_and_omitted_components_preserve_selection_independence() {
    let before = BTreeMap::from([
        (
            "episode_progress".into(),
            "{\"native\":{\"watched\":true}}".into(),
        ),
        ("continue_hidden".into(), "{\"show\":7}".into()),
        ("watch_history_hidden:v1".into(), "{\"show\":8}".into()),
    ]);
    let edits = adapters::materialize(
        "continue_hidden",
        &Payload {
            version: 1,
            ..Default::default()
        },
        &before,
    )
    .unwrap();
    assert_eq!(edits.len(), 1);
    assert_eq!(edits["continue_hidden"].as_deref(), Some("{}"));
    assert!(!edits.contains_key("episode_progress"));
    assert!(!edits.contains_key("watch_history_hidden:v1"));
}

#[test]
fn failed_staged_commit_cannot_escape_through_a_later_save() {
    let mut live = nova_sync::Store::default();
    live.set("profile:p:library", "local", Some("{}".into()), 0, 1);
    let before = live.digest();
    let mut staged = live.clone();
    staged.set("profile:p:library", "imported", Some("{}".into()), 0, 1);
    staged.queue_extra("receipt", Some("import".into()));
    let result = publish_staged(&mut live, staged, |_| Err("disk full".into()));
    assert!(result.is_err());
    assert_eq!(live.digest(), before);
    assert!(live.record("profile:p:library", "imported").is_none());
    live.set("profile:p:library", "later", Some("{}".into()), 0, 1);
    assert!(live.record("profile:p:library", "imported").is_none());
}

#[test]
fn undo_reverts_unchanged_imports_preserves_later_edits_and_unrelated_records() {
    let before = payload(&[("progress\0old", json!(1)), ("progress\0edit", json!(1))]);
    let imported = payload(&[
        ("progress\0old", json!(2)),
        ("progress\0edit", json!(2)),
        ("progress\0new", json!(2)),
    ]);
    let current = payload(&[
        ("progress\0old", json!(2)),
        ("progress\0edit", json!(3)),
        ("progress\0new", json!(2)),
        ("progress\0later", json!(4)),
    ]);
    let undone = undo_merge(&current, &before, &imported, false);
    assert_eq!(undone.records["progress\0old"], 1);
    assert_eq!(undone.records["progress\0edit"], 3);
    assert!(!undone.records.contains_key("progress\0new"));
    assert_eq!(undone.records["progress\0later"], 4);
    assert_eq!(
        undo_merge(&current, &before, &imported, true).records["progress\0edit"],
        1
    );
}

#[test]
fn selected_service_data_never_exports_pending_service_jobs() {
    let owner = ProfileStore::for_profile("test");
    let mut store = nova_sync::Store::default();
    for (key, value) in [
        ("connection:0", json!({"token":"plain"})),
        ("account:[0,\"a\"]:automatic", json!(true)),
        ("selected:[0,\"a\",\"movie\",\"same\"]", json!(true)),
        ("lease:old", json!("job")),
        ("outbox:delete", json!("delete")),
    ] {
        store.set(
            &owner.domain("list_services"),
            key,
            Some(value.to_string()),
            0,
            1,
        );
    }
    let exported = adapters::capture("integrations", &owner, &BTreeMap::new(), &store).unwrap();
    assert_eq!(exported.records.len(), 3);
    assert!(
        exported
            .records
            .values()
            .any(|v| v.get("token") == Some(&json!("plain")))
    );
    assert!(
        !exported
            .records
            .keys()
            .any(|key| key.contains("outbox") || key.contains("lease"))
    );
}

#[test]
fn import_commits_snapshots_baselines_fresh_records_receipt_and_safe_undo_together() {
    const ROOT: &str = "NOVA_BACKUP_ATOMIC_TEST_ROOT";
    let Some(root) = std::env::var_os(ROOT) else {
        let root = std::env::temp_dir().join(format!(
            "nova-backup-test-{}-{}",
            std::process::id(),
            nova_config::now_ms()
        ));
        let output=std::process::Command::new(std::env::current_exe().unwrap()).env(ROOT,&root).env("XDG_DATA_HOME",root.join("data")).env("XDG_CACHE_HOME",root.join("cache")).args(["--exact","app::backup::tests::import_commits_snapshots_baselines_fresh_records_receipt_and_safe_undo_together","--nocapture"]).output().unwrap();
        let _ = std::fs::remove_dir_all(root);
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    };
    nova_storage::init_at(&std::path::PathBuf::from(root));
    profiles::initialize().unwrap();
    let mut selective_manifest = manifest();
    selective_manifest.profiles[0].components[0] = archive::Component {
        name: "library".into(),
        schema_version: 1,
        path: "profiles/p1/library.json".into(),
        item_count: 0,
        sha256: String::new(),
    };
    let bytes = archive::encode(
        selective_manifest,
        &BTreeMap::from([(
            ("p1".into(), "library".into()),
            Payload {
                version: 1,
                ..Default::default()
            },
        )]),
    )
    .unwrap();
    let selective = archive::decode(&bytes).unwrap();
    for preset in [0, 1] {
        let mut choices = super::choices(&selective).unwrap();
        choices[0].components = ui::components_for_preset(preset, Some(&selective), "p1");
        assert_eq!(choices[0].components, BTreeSet::from(["library".into()]));
        assert!(preview(selective.clone(), choices).is_ok());
    }
    let owner = ProfileStore::for_profile("default");
    owner
        .persist_snapshot("continue_hidden", r#"{"original":3}"#, false)
        .unwrap();
    owner
        .persist_snapshot("watch_history_hidden:v1", r#"{"independent":7}"#, false)
        .unwrap();
    let bytes = super::export(&BTreeMap::from([(
        "default".into(),
        BTreeSet::from(["continue_hidden".into()]),
    )]))
    .unwrap();
    owner
        .persist_snapshot("continue_hidden", r#"{"original":4,"added":5}"#, false)
        .unwrap();
    let archive = archive::decode(&bytes).unwrap();
    let mut choices = super::choices(&archive).unwrap();
    choices[0].target = "default".into();
    choices[0].create = false;
    let merged = preview(archive.clone(), choices.clone()).unwrap();
    assert_eq!(
        (
            merged.rows[0].counts.conflicts,
            merged.rows[0].counts.updated
        ),
        (1, 0)
    );
    owner
        .persist_snapshot("continue_hidden", r#"{"original":10,"added":5}"#, false)
        .unwrap();
    assert!(apply(&merged).unwrap_err().contains("Target changed"));
    assert!(
        nova_storage::try_scan_prefix(RECOVERY_PREFIX)
            .unwrap()
            .is_empty()
    );
    choices[0].replace = true;
    choices[0].use_backup.insert("continue_hidden".into());
    let replacement = preview(archive, choices).unwrap();
    assert_eq!(replacement.rows[0].counts.deleted, 1);
    apply(&replacement).unwrap();
    assert_eq!(
        owner.read("continue_hidden").unwrap().as_deref(),
        Some(r#"{"original":3}"#)
    );
    assert_eq!(
        owner.read("watch_history_hidden:v1").unwrap().as_deref(),
        Some(r#"{"independent":7}"#)
    );
    let persisted = nova_sync::Store::try_load().unwrap();
    let domain = owner.domain("continue_hidden");
    assert_eq!(
        persisted
            .record(&domain, "original")
            .unwrap()
            .value
            .as_deref(),
        Some("3")
    );
    assert!(persisted.record(&domain, "added").unwrap().is_deleted());
    assert_eq!(
        nova_storage::try_get_str(&format!("sync:baseline:{domain}"))
            .unwrap()
            .as_deref(),
        Some(r#"{"original":"3"}"#)
    );
    assert_eq!(
        nova_storage::try_scan_prefix(RECOVERY_PREFIX)
            .unwrap()
            .len(),
        1
    );
    assert!(nova_storage::try_get_str(ACTIVATION_KEY).unwrap().is_some());
    owner
        .persist_snapshot("continue_hidden", r#"{"original":11,"later":9}"#, false)
        .unwrap();
    let undo = undo_preview().unwrap();
    assert!(undo.choices[0].use_backup.is_empty());
    apply(&undo).unwrap();
    let restored: serde_json::Value =
        serde_json::from_str(&owner.read("continue_hidden").unwrap().unwrap()).unwrap();
    assert_eq!(restored, json!({"original":11,"added":5,"later":9}));
    assert_eq!(
        owner.read("watch_history_hidden:v1").unwrap().as_deref(),
        Some(r#"{"independent":7}"#)
    );
    let mut manifest = manifest();
    manifest.profiles[0].components.push(archive::Component {
        name: "settings".into(),
        schema_version: 1,
        path: "profiles/p1/settings.json".into(),
        item_count: 0,
        sha256: String::new(),
    });
    let pin = payload(&[(
        "profile_policy:v1\0value",
        json!({"version":1,"pin":"0012","restricted":true,"require_pin_on_entry":true,"allowed_titles":[],"allowed_addons":[]}),
    )]);
    let settings = payload(&[("settings\0auto_next_episode", json!(true))]);
    let bytes = archive::encode(
        manifest,
        &BTreeMap::from([
            (("p1".into(), "policy".into()), pin),
            (("p1".into(), "settings".into()), settings),
        ]),
    )
    .unwrap();
    let archive = archive::decode(&bytes).unwrap();
    let choices = super::choices(&archive).unwrap();
    let new_id = choices[0].target.clone();
    assert!(choices[0].create && !choices[0].replace);
    let preview = preview(archive, choices).unwrap();
    apply(&preview).unwrap();
    let restored = app::profile_policy::load(&ProfileStore::for_profile(&new_id)).unwrap();
    assert_eq!(restored.pin, "0012");
    assert!(restored.restricted && restored.require_pin_on_entry);
    let restored: serde_json::Value = serde_json::from_str(
        &ProfileStore::for_profile(&new_id)
            .read("settings")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(restored["auto_next_episode"], true);
    assert_eq!(profiles::current_id(), "default");
}

// Storage and sync globals are process-owned; keep these regressions isolated
// from each other and from the larger atomic import scenario above.
fn isolated_backup_regression(name: &str) -> bool {
    const ROOT: &str = "NOVA_BACKUP_FOCUSED_TEST_ROOT";
    let Some(root) = std::env::var_os(ROOT) else {
        let root = std::env::temp_dir().join(format!(
            "nova-backup-{name}-{}-{}",
            std::process::id(),
            nova_config::now_ms()
        ));
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .env(ROOT, &root)
            .env("XDG_DATA_HOME", root.join("data"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .args([
                "--exact",
                &format!("{}::{name}", module_path!()),
                "--nocapture",
            ])
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(root);
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return false;
    };
    nova_storage::init_at(&std::path::PathBuf::from(root));
    profiles::initialize().unwrap();
    true
}

#[test]
fn skipped_profiles_preserve_choices_and_never_create_or_mutate_targets() {
    if !isolated_backup_regression(
        "skipped_profiles_preserve_choices_and_never_create_or_mutate_targets",
    ) {
        return;
    }
    let mut manifest = manifest();
    manifest.profiles[0].components[0] = archive::Component {
        name: "continue_hidden".into(),
        schema_version: 1,
        path: "profiles/p1/continue_hidden.json".into(),
        item_count: 0,
        sha256: String::new(),
    };
    let mut skipped = manifest.profiles[0].clone();
    skipped.archive_profile_id = "p2".into();
    skipped.name = "Skipped household".into();
    skipped.components[0].path = "profiles/p2/continue_hidden.json".into();
    manifest.profiles.push(skipped);
    let bytes = archive::encode(
        manifest,
        &BTreeMap::from([
            (
                ("p1".into(), "continue_hidden".into()),
                payload(&[("shared", json!(1))]),
            ),
            (
                ("p2".into(), "continue_hidden".into()),
                payload(&[("skipped", json!(2))]),
            ),
        ]),
    )
    .unwrap();
    let archive = archive::decode(&bytes).unwrap();
    let owner = ProfileStore::for_profile("default");
    owner
        .persist_snapshot("continue_hidden", r#"{"shared":9,"local":7}"#, false)
        .unwrap();
    let mut choices = super::choices(&archive).unwrap();
    choices[0].target = "default".into();
    choices[0].create = false;
    choices[1].replace = true;
    choices[1].use_backup.insert("continue_hidden".into());
    let saved = choices[1].clone();
    choices[1].included = false;
    let merged = preview(archive.clone(), choices.clone()).unwrap();
    assert_eq!(merged.choices.len(), 1);
    assert_eq!(merged.rows.len(), 1);
    assert_eq!(merged.rows[0].counts.conflicts, 1);
    assert_eq!(merged.rows[0].counts.updated, 0);
    assert_eq!(merged.selection, choices);
    apply(&merged).unwrap();
    assert_eq!(
        owner.read("continue_hidden").unwrap().as_deref(),
        Some(r#"{"local":7,"shared":9}"#)
    );
    assert!(
        !profiles::list()
            .unwrap()
            .iter()
            .any(|profile| profile.id == saved.target)
    );
    assert!(
        nova_storage::try_scan_prefix(&format!("profile:{}:", saved.target))
            .unwrap()
            .is_empty()
    );
    choices[1].included = true;
    // Re-including a source restores every choice, including Replace/conflicts.
    assert!(choices[1] == saved);
    choices[1].included = false;
    choices[1].target = "default".into();
    choices[1].create = false;
    assert!(
        preview(archive.clone(), choices.clone()).is_ok(),
        "skipped sources do not reserve a target"
    );
    choices[0].replace = true;
    choices[0].use_backup.insert("continue_hidden".into());
    let replacement = preview(archive, choices).unwrap();
    assert_eq!(replacement.rows[0].counts.deleted, 1);
    let mut stale = replacement.clone();
    stale.selection[1].included = true;
    assert!(apply(&stale).unwrap_err().contains("Selection changed"));
    apply(&replacement).unwrap();
    let restored: serde_json::Value =
        serde_json::from_str(&owner.read("continue_hidden").unwrap().unwrap()).unwrap();
    assert_eq!(restored, json!({"shared":1}));
    let raw: serde_json::Value = serde_json::to_value(&saved).unwrap();
    let mut legacy = raw.as_object().unwrap().clone();
    legacy.remove("included");
    assert!(
        serde_json::from_value::<Choice>(legacy.into())
            .unwrap()
            .included,
        "old recovery receipts keep their selected profiles"
    );
}

#[test]
fn recovery_deletion_requires_the_exact_reviewed_inventory() {
    if !isolated_backup_regression("recovery_deletion_requires_the_exact_reviewed_inventory") {
        return;
    }
    let first = format!("{RECOVERY_PREFIX}1000-1");
    let second = format!("{RECOVERY_PREFIX}2000-2");
    nova_storage::try_write_batch(&[(first.clone(), Some("first".into()))]).unwrap();
    let reviewed = recovery_inventory().unwrap();
    assert_eq!(reviewed.len(), 1);
    assert!(delete_reviewed_recovery(&[]).is_err());
    // Requesting, reviewing and abandoning confirmation performs no writes.
    assert_eq!(recovery_inventory().unwrap(), reviewed);
    nova_storage::try_write_batch(&[(second.clone(), Some("second".into()))]).unwrap();
    assert!(
        delete_reviewed_recovery(&reviewed)
            .unwrap_err()
            .contains("changed")
    );
    assert_eq!(recovery_inventory().unwrap().len(), 2);
    let reviewed = recovery_inventory().unwrap();
    nova_storage::try_write_batch(&[(first, Some("modified".into()))]).unwrap();
    assert!(
        delete_reviewed_recovery(&reviewed).is_err(),
        "even changes to an existing copy require review"
    );
    let reviewed = recovery_inventory().unwrap();
    delete_reviewed_recovery(&reviewed).unwrap();
    assert!(recovery_inventory().unwrap().is_empty());
}
