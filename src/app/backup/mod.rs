//! Selective personal-state archives and atomic, compensating local imports.
mod adapters;
mod archive;
mod ui;

use crate::app::{self, profiles, profiles::ProfileStore};
use archive::{Archive, Payload};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const RECOVERY_PREFIX: &str = "backup:recovery:v1:";
const ACTIVATION_KEY: &str = "backup:activation:v1";
fn recovery_sequence(key: &str) -> u64 {
    key.rsplit_once('-')
        .and_then(|(_, sequence)| sequence.parse().ok())
        .unwrap_or(0)
}

#[derive(Clone, Serialize, Deserialize)]
struct Choice {
    pub source: String,
    pub target: String,
    pub name: String,
    pub create: bool,
    pub components: BTreeSet<String>,
    pub replace: bool,
    pub use_backup: BTreeSet<String>,
}
#[derive(Clone, Default)]
struct Counts {
    pub archived: usize,
    pub added: usize,
    pub identical: usize,
    pub conflicts: usize,
    pub updated: usize,
    pub preserved: usize,
    pub deleted: usize,
    pub skipped: usize,
    pub examples: Vec<String>,
}
#[derive(Clone)]
struct PreviewRow {
    pub profile: String,
    pub component: String,
    pub counts: Counts,
    pub notes: Vec<String>,
}
#[derive(Clone)]
struct Preview {
    pub archive: Archive,
    pub choices: Vec<Choice>,
    pub rows: Vec<PreviewRow>,
    revisions: BTreeMap<String, String>,
    edits: BTreeMap<String, Option<String>>,
    domains: BTreeMap<String, Vec<(String, String, u64)>>,
    before: BTreeMap<(String, String), Payload>,
    after: BTreeMap<(String, String), Payload>,
    local_expected: BTreeMap<String, Option<String>>,
}
#[derive(Clone, Serialize, Deserialize)]
struct RecoveryComponent {
    target: String,
    name: String,
    before: Payload,
    after: Payload,
}
#[derive(Clone, Serialize, Deserialize)]
struct Receipt {
    id: String,
    checksum: String,
    created_at: u64,
    choices: Vec<Choice>,
    components: Vec<RecoveryComponent>,
    after_revisions: BTreeMap<String, String>,
    domains: Vec<String>,
    #[serde(default)]
    local_before: BTreeMap<String, Option<String>>,
    #[serde(default)]
    local_after: BTreeMap<String, Option<String>>,
}

fn profile_rows(id: &str, all: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let prefix = format!("profile:{id}:");
    all.range(prefix.clone()..)
        .take_while(|(key, _)| key.starts_with(&prefix))
        .map(|(key, value)| (key[prefix.len()..].to_owned(), value.clone()))
        .collect()
}
fn capture_rows(ids: impl Iterator<Item = String>) -> Result<BTreeMap<String, String>, String> {
    let mut prefixes = ids.map(|id| format!("profile:{id}:")).collect::<Vec<_>>();
    prefixes.extend([
        RECOVERY_PREFIX.into(),
        ACTIVATION_KEY.into(),
        "smart_downloads:device:v1".into(),
    ]);
    nova_storage::try_scan_prefixes(&prefixes)
        .map(|rows| rows.into_iter().collect())
        .map_err(|e| e.to_string())
}
fn revision(id: &str, rows: &BTreeMap<String, String>, store: &nova_sync::Store) -> String {
    let digest = store
        .digest()
        .into_iter()
        .filter(|(domain, _)| {
            nova_sync::parse_profile_domain(domain).is_some_and(|(owner, _)| owner == id)
                || domain == "profiles"
                || domain == "profile_deletions"
        })
        .collect::<BTreeMap<_, _>>();
    archive::digest(serde_json::to_string(&(rows, digest)).unwrap().as_bytes())
}
fn check_owner(id: &str, store: &nova_sync::Store, create: bool) -> Result<(), String> {
    if !profiles::valid_id(id)
        || store
            .record("profile_deletions", id)
            .is_some_and(|row| !row.is_deleted())
    {
        return Err("The target profile was deleted".into());
    }
    let exists = store
        .record("profiles", id)
        .is_some_and(|row| !row.is_deleted());
    if exists == create {
        return Err("The target profile changed; rebuild the preview".into());
    }
    if !create && !app::profile_policy::management_allowed(&ProfileStore::for_profile(id)) {
        return Err("Unlock the target profile before importing".into());
    }
    Ok(())
}

fn export(selected: &BTreeMap<String, BTreeSet<String>>) -> Result<Vec<u8>, String> {
    app::persistence::flush()?;
    if let Some(result) = app::bridge::with_global_bridge(|bridge| {
        bridge
            .last_stream
            .flush_families(&selected.keys().cloned().collect::<Vec<_>>())
    }) {
        result?;
    }
    let live = profiles::list()?;
    let handle = nova_sync::local_store().map_err(|e| e.to_string())?;
    let store = handle.lock().unwrap();
    // Existing lock order: record owner -> profile writer -> storage writer.
    let _serial = profiles::WRITE_SERIAL.lock().unwrap();
    let all = capture_rows(selected.keys().cloned())?;
    let mut manifest = archive::Manifest {
        format: "nova-backup".into(),
        version: 1,
        created_at: nova_config::now_secs(),
        app_version: env!("CARGO_PKG_VERSION").into(),
        archive_id: format!(
            "{}-{}",
            nova_config::now_ms(),
            nova_storage::try_allocate_local_sequence("backup", 1).map_err(|e| e.to_string())?
        ),
        profiles: Vec::new(),
        packages: Vec::new(),
    };
    let mut payloads = BTreeMap::new();
    for (index, (id, components)) in selected.iter().enumerate() {
        let profile = live
            .iter()
            .find(|profile| &profile.id == id)
            .ok_or("Profile changed during export; retry")?;
        check_owner(id, &store, false)?;
        let owner = ProfileStore::for_profile(id);
        let rows = profile_rows(id, &all);
        let archive_id = format!("p{index}");
        let mut descriptions = Vec::new();
        for name in components {
            let payload = adapters::capture(name, &owner, &rows, &store)?;
            descriptions.push(archive::Component {
                name: name.clone(),
                schema_version: 1,
                path: format!("profiles/{archive_id}/{name}.json"),
                item_count: payload.records.len(),
                sha256: String::new(),
            });
            payloads.insert((archive_id.clone(), name.clone()), payload);
        }
        manifest.profiles.push(archive::Profile {
            archive_profile_id: archive_id,
            source_profile_id: id.clone(),
            name: profile.name.clone(),
            components: descriptions,
        });
    }
    drop(_serial);
    drop(store);
    let mut packages = BTreeMap::new();
    for ((_, name), payload) in &payloads {
        if name != "packages" {
            continue;
        }
        for value in payload.records.values() {
            let hash = value["sha256"]
                .as_str()
                .ok_or("Invalid addon package descriptor")?;
            if packages.contains_key(hash) {
                continue;
            }
            let bytes = app::addon_packages::backup_archive(hash)?;
            let parsed = nova_providers::AddonPackage::parse(&bytes).map_err(|e| e.to_string())?;
            if parsed.sha256() != hash
                || Some(parsed.id()) != value["id"].as_str()
                || Some(parsed.version()) != value["version"].as_str()
            {
                return Err("Addon package identity mismatch".into());
            }
            manifest.packages.push(archive::Package {
                id: parsed.id().into(),
                version: parsed.version().into(),
                path: format!("packages/{hash}.zip"),
                sha256: hash.into(),
            });
            packages.insert(hash.into(), bytes);
        }
    }
    archive::encode_with_packages(manifest, &payloads, &packages)
}

fn choices(archive: &Archive) -> Result<Vec<Choice>, String> {
    let device = nova_sync::local_device().map_err(|e| e.to_string())?;
    archive
        .manifest
        .profiles
        .iter()
        .map(|profile| {
            let sequence = nova_storage::try_allocate_local_sequence("profiles", 1)
                .map_err(|e| e.to_string())?;
            Ok(Choice {
                source: profile.archive_profile_id.clone(),
                target: format!("p{device:x}_{sequence:x}"),
                name: profile.name.clone(),
                create: true,
                components: profile
                    .components
                    .iter()
                    .filter(|component| {
                        archive.payloads.contains_key(&(
                            profile.archive_profile_id.clone(),
                            component.name.clone(),
                        )) && component.name != "device"
                            && component.name != "packages"
                    })
                    .map(|component| component.name.clone())
                    .collect(),
                replace: false,
                use_backup: BTreeSet::new(),
            })
        })
        .collect()
}

fn count(local: &Payload, backup: &Payload, result: &Payload) -> Counts {
    let mut counts = Counts {
        archived: backup.records.len(),
        ..Counts::default()
    };
    for (key, value) in &backup.records {
        if !result.records.contains_key(key) {
            counts.skipped += 1;
            continue;
        }
        match local.records.get(key) {
            None => counts.added += 1,
            Some(old) if old == value => counts.identical += 1,
            Some(_) => {
                counts.conflicts += 1;
                if counts.examples.len() < 3 {
                    counts.examples.push(key.replace('\0', " / "));
                }
            }
        }
    }
    for (key, old) in &local.records {
        match result.records.get(key) {
            None => counts.deleted += 1,
            Some(new) if new != old => counts.updated += 1,
            _ => counts.preserved += 1,
        }
    }
    counts
}

fn preview(archive: Archive, choices: Vec<Choice>) -> Result<Preview, String> {
    app::persistence::flush()?;
    let choices = choices
        .into_iter()
        .filter(|choice| !choice.components.is_empty())
        .collect::<Vec<_>>();
    let mut targets = BTreeSet::new();
    let mut sources = BTreeSet::new();
    if choices.is_empty() {
        return Err("Choose at least one profile".into());
    }
    let handle = nova_sync::local_store().map_err(|e| e.to_string())?;
    let store = handle.lock().unwrap();
    let _serial = profiles::WRITE_SERIAL.lock().unwrap();
    let all = capture_rows(choices.iter().map(|choice| choice.target.clone()))?;
    let mut result = Preview {
        archive: archive.clone(),
        choices: choices.clone(),
        rows: Vec::new(),
        revisions: BTreeMap::new(),
        edits: BTreeMap::new(),
        domains: BTreeMap::new(),
        before: BTreeMap::new(),
        after: BTreeMap::new(),
        local_expected: BTreeMap::new(),
    };
    let mut prepared = Vec::new();
    for choice in &choices {
        if !targets.insert(&choice.target) || !sources.insert(&choice.source) {
            return Err("Choose one distinct target for each backup profile".into());
        }
        check_owner(&choice.target, &store, choice.create)?;
        if choice.name.trim().is_empty()
            || choice.name.chars().count() > 64
            || choice.name.chars().any(char::is_control)
        {
            return Err("Invalid profile name".into());
        }
        let owner = ProfileStore::for_profile(&choice.target);
        let before = profile_rows(&choice.target, &all);
        result.revisions.insert(
            choice.target.clone(),
            revision(&choice.target, &before, &store),
        );
        let mut rows = before.clone();
        for name in &choice.components {
            let backup = archive
                .payloads
                .get(&(choice.source.clone(), name.clone()))
                .ok_or("Selected backup component is missing")?;
            // A new identity has no user-authored local records. Synthesized
            // settings/policy defaults must not win Merge against its archive.
            let local = if choice.create {
                Payload {
                    version: 1,
                    ..Payload::default()
                }
            } else {
                adapters::capture(name, &owner, &before, &store)?
            };
            let mut after = if let Some(imported) = archive
                .undo_after
                .as_ref()
                .and_then(|after| after.get(&(choice.target.clone(), name.clone())))
            {
                undo_merge(&local, backup, imported, choice.use_backup.contains(name))
            } else {
                adapters::merge(
                    name,
                    &local,
                    backup,
                    choice.replace,
                    choice.use_backup.contains(name),
                )?
            };
            after = adapters::normalize(name, after)?;
            let mut notes = Vec::new();
            if name == "library" {
                let categories = rows
                    .get("settings")
                    .map(|raw| serde_json::from_str::<serde_json::Value>(raw))
                    .transpose()
                    .map_err(|e| e.to_string())?
                    .unwrap_or_default();
                let allowed = categories["categories"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                let mut omitted = 0;
                for (key, value) in &mut after.records {
                    if let Some(categories) = value
                        .get_mut("categories")
                        .and_then(serde_json::Value::as_array_mut)
                    {
                        let before = categories.len();
                        categories.retain(|category| {
                            allowed.contains(category)
                                || !choice.replace
                                    && local
                                        .records
                                        .get(key)
                                        .and_then(|value| value["categories"].as_array())
                                        .is_some_and(|existing| existing.contains(category))
                        });
                        omitted += before - categories.len();
                    }
                }
                if omitted > 0 {
                    notes.push(format!(
                        "{}: {omitted}",
                        nova_ui::backend_text::tr(
                            "Category assignments omitted because their categories are unavailable"
                        )
                    ));
                }
            }
            if name == "addons" || name == "packages" {
                for (key, value) in &after.records {
                    if !key.starts_with("addon_packages:v1\0")
                        && !key.starts_with("package_files\0")
                    {
                        continue;
                    }
                    if let Some(hash) = value["sha256"].as_str() {
                        let status = if archive.packages.contains_key(hash)
                            && choice.components.contains("packages")
                        {
                            "Included"
                        } else if app::addon_packages::backup_archive(hash)
                            .is_ok_and(|bytes| archive::digest(&bytes) == hash)
                        {
                            "Already installed"
                        } else {
                            "Reinstall needed"
                        };
                        notes.push(format!(
                            "{}: {}",
                            value["id"].as_str().unwrap_or(""),
                            nova_ui::backend_text::tr(status)
                        ));
                    }
                }
            }
            let counts = count(&local, backup, &after);
            if counts.skipped > 0 {
                notes.push(format!(
                    "{}: {}",
                    nova_ui::backend_text::tr("Skipped expired or unsupported values"),
                    counts.skipped
                ));
            }
            result.rows.push(PreviewRow {
                profile: choice.name.clone(),
                component: name.clone(),
                counts,
                notes,
            });
            let edits = adapters::materialize(name, &after, &rows)?;
            for (key, value) in edits {
                match &value {
                    Some(raw) => {
                        rows.insert(key.clone(), raw.clone());
                    }
                    None => {
                        rows.remove(&key);
                    }
                }
                result.edits.insert(owner.key(&key), value);
            }
            if name == "integrations" {
                for (key, value) in adapters::integration_local_edits(&after, &rows)? {
                    match &value {
                        Some(raw) => {
                            rows.insert(key.clone(), raw.clone());
                        }
                        None => {
                            rows.remove(&key);
                        }
                    }
                    result.edits.insert(owner.key(&key), value);
                }
                for (domain, records) in adapters::integration_domains(&after)? {
                    // Only durable configuration keys participate; runtime jobs
                    // and unrelated records are never deletion authority.
                    result.domains.insert(owner.domain(&domain), records);
                }
            }
            result
                .before
                .insert((choice.target.clone(), name.clone()), local);
            result
                .after
                .insert((choice.target.clone(), name.clone()), after);
        }
        prepared.push((owner, rows));
    }
    drop(_serial);
    drop(store);
    // Snapshot decomposition can consult profile-aware settings defaults; do
    // this before the record-owner lock, then bind the result to its revision.
    for (owner, rows) in prepared {
        for (key, raw) in rows {
            if !result.edits.contains_key(&owner.key(&key)) {
                continue;
            }
            let domains = owner
                .run(|| app::sync::snapshot_domains(&key, &raw))
                .map_err(|e| e.to_string())?;
            for (domain, records) in domains {
                result.domains.insert(owner.domain(domain), records);
            }
        }
        if choices
            .iter()
            .any(|choice| choice.target == owner.id() && choice.components.contains("journal"))
        {
            let mut sessions = Vec::new();
            for ((id, name), payload) in &result.after {
                if id == owner.id() && name == "journal" {
                    for (key, value) in &payload.records {
                        let (_, id) = key.split_once('\0').ok_or("Invalid journal identity")?;
                        sessions.push((id.into(), value.to_string(), 0));
                    }
                }
            }
            result
                .domains
                .insert(owner.domain("viewing_sessions"), sessions);
        }
    }
    let mut device_edits = BTreeMap::new();
    if let Some((before, imported)) = &archive.undo_local {
        for (key, value) in before {
            if all.get(key).cloned() == imported.get(key).cloned().flatten() {
                device_edits.insert(key.clone(), value.clone());
            }
        }
    } else {
        let offline = choices
            .iter()
            .filter(|choice| choice.components.contains("offline"))
            .map(|choice| choice.target.clone())
            .collect::<Vec<_>>();
        if !offline.is_empty() {
            device_edits
                .extend(app::smart_offline::portable_import_device_edits_for_profiles(&offline)?);
        }
        for choice in choices
            .iter()
            .filter(|choice| choice.components.contains("reminders"))
        {
            let owner = ProfileStore::for_profile(&choice.target);
            let (key, value) = app::release_reminders::import_delivery_edit(&owner)?;
            device_edits.insert(owner.key(&key), value);
        }
    }
    for (key, value) in device_edits {
        result
            .local_expected
            .insert(key.clone(), all.get(&key).cloned());
        result.edits.insert(key, value);
    }
    Ok(result)
}

fn apply(preview: &Preview) -> Result<Vec<String>, String> {
    // Delivery's read-modify-write owner precedes record/profile locks. This
    // prevents an in-flight digest from restoring activation after import.
    let _delivery = app::release_reminders::delivery_guard();
    let _offline = app::smart_offline::device_guard();
    app::persistence::flush()?;
    // Unreferenced staged immutable files are harmless after a failed commit.
    for ((_, name), payload) in &preview.after {
        if name != "packages" {
            continue;
        }
        for value in payload.records.values() {
            let hash = value["sha256"]
                .as_str()
                .ok_or("Invalid addon package descriptor")?;
            let bytes = preview
                .archive
                .packages
                .get(hash)
                .ok_or("Selected addon package file is missing")?;
            app::addon_packages::stage_backup_archive(bytes)?;
        }
    }
    let handle = nova_sync::local_store().map_err(|e| e.to_string())?;
    let device = nova_sync::local_device().map_err(|e| e.to_string())?;
    let mut live = handle.lock().unwrap();
    let _serial = profiles::WRITE_SERIAL.lock().unwrap();
    let all = capture_rows(preview.choices.iter().map(|choice| choice.target.clone()))?;
    for choice in &preview.choices {
        check_owner(&choice.target, &live, choice.create)?;
        if preview.revisions.get(&choice.target)
            != Some(&revision(
                &choice.target,
                &profile_rows(&choice.target, &all),
                &live,
            ))
        {
            return Err(
                "Target changed while preview was open. Rebuild the preview before importing."
                    .into(),
            );
        }
    }
    for (key, expected) in &preview.local_expected {
        if all.get(key) != expected.as_ref() {
            return Err("Device preferences changed while preview was open. Rebuild the preview before importing.".into());
        }
    }
    let mut staged = live.clone();
    for choice in &preview.choices {
        if choice.create {
            staged.set(
                "profiles",
                &choice.target,
                Some(serde_json::json!({"name":choice.name.trim()}).to_string()),
                0,
                device,
            );
        }
    }
    for (domain, records) in &preview.domains {
        // Seed the deletion baseline from current live records, including
        // records received while this profile was inactive.
        let selected = if nova_sync::parse_profile_domain(domain)
            .is_some_and(|(_, base)| matches!(base, "tracking" | "stremio" | "list_services"))
        {
            let existing = staged.records(domain);
            let wanted: BTreeSet<_> = records.iter().map(|(key, _, _)| key.as_str()).collect();
            let component = preview.before.iter().find(|((id, name), _)| {
                name == "integrations"
                    && nova_sync::parse_profile_domain(domain).is_some_and(|(owner, _)| owner == id)
            });
            let before = component
                .map(|(_, payload)| adapters::integration_domains(payload))
                .transpose()?
                .and_then(|mut domains| {
                    domains.remove(nova_sync::parse_profile_domain(domain).unwrap().1)
                })
                .unwrap_or_default();
            let removable: BTreeSet<_> = before.iter().map(|(key, _, _)| key.as_str()).collect();
            for (key, _) in &existing {
                if removable.contains(key.as_str()) && !wanted.contains(key.as_str()) {
                    staged.set(domain, key, None, 0, device);
                }
            }
            for (key, raw, _) in records {
                staged.set(domain, key, Some(raw.clone()), 0, device);
            }
            existing
                .into_iter()
                .filter(|(key, _)| !removable.contains(key.as_str()))
                .map(|(key, raw)| (key, raw, 0))
                .chain(records.clone())
                .collect::<Vec<_>>()
        } else {
            let baseline = staged
                .records(domain)
                .into_iter()
                .collect::<BTreeMap<_, _>>();
            staged.queue_extra(
                &format!("sync:baseline:{domain}"),
                Some(serde_json::to_string(&baseline).map_err(|e| e.to_string())?),
            );
            records.clone()
        };
        nova_sync::prepare_snapshot(&mut staged, device, domain, &selected, None, false, false)
            .map_err(|e| e.to_string())?;
    }
    for (key, value) in &preview.edits {
        staged.queue_extra(key, value.clone());
    }
    for choice in &preview.choices {
        if choice.components.contains("integrations") {
            for domain in ["tracking", "stremio", "list_services"] {
                staged.mark_pending(&ProfileStore::for_profile(&choice.target).domain(domain));
            }
        }
    }
    let domains = preview
        .domains
        .keys()
        .cloned()
        .chain(
            preview
                .choices
                .iter()
                .any(|choice| choice.create)
                .then(|| "profiles".into()),
        )
        .collect::<Vec<_>>();
    let mut after_revisions = BTreeMap::new();
    let mut after_all = all.clone();
    for (key, value) in &preview.edits {
        match value {
            Some(raw) => {
                after_all.insert(key.clone(), raw.clone());
            }
            None => {
                after_all.remove(key);
            }
        }
    }
    for choice in &preview.choices {
        after_revisions.insert(
            choice.target.clone(),
            revision(
                &choice.target,
                &profile_rows(&choice.target, &after_all),
                &staged,
            ),
        );
    }
    let id = format!(
        "{}-{}",
        nova_config::now_ms(),
        nova_storage::try_allocate_local_sequence("backup_receipt", 1).map_err(|e| e.to_string())?
    );
    let receipt = Receipt {
        id: id.clone(),
        checksum: preview.archive.checksum.clone(),
        created_at: nova_config::now_secs(),
        choices: preview.choices.clone(),
        components: preview
            .before
            .iter()
            .map(|((target, name), before)| RecoveryComponent {
                target: target.clone(),
                name: name.clone(),
                before: before.clone(),
                after: preview.after[&(target.clone(), name.clone())].clone(),
            })
            .collect(),
        after_revisions,
        domains: domains.clone(),
        local_before: preview.local_expected.clone(),
        local_after: preview
            .local_expected
            .keys()
            .map(|key| (key.clone(), preview.edits.get(key).cloned().flatten()))
            .collect(),
    };
    staged.queue_extra(
        &format!("{RECOVERY_PREFIX}{id}"),
        Some(serde_json::to_string(&receipt).map_err(|e| e.to_string())?),
    );
    staged.queue_extra(
        ACTIVATION_KEY,
        Some(serde_json::to_string(&domains).unwrap()),
    );
    let mut old = all
        .keys()
        .filter(|key| key.starts_with(RECOVERY_PREFIX))
        .cloned()
        .collect::<Vec<_>>();
    old.sort_by_key(|key| recovery_sequence(key));
    while old.len() > 2 {
        staged.queue_extra(&old.remove(0), None);
    }
    // On failure the tentative clone is discarded. An unrelated later save
    // cannot leak any failed import into the live mutation owner.
    publish_staged(&mut live, staged, |staged| {
        staged.save().map_err(|e| e.to_string())
    })?;
    drop(_serial);
    drop(live);
    nova_sync::request_local_sync();
    Ok(domains)
}

fn publish_staged(
    live: &mut nova_sync::Store,
    mut staged: nova_sync::Store,
    commit: impl FnOnce(&mut nova_sync::Store) -> Result<(), String>,
) -> Result<(), String> {
    commit(&mut staged)?;
    *live = staged;
    Ok(())
}

fn undo_preview() -> Result<Preview, String> {
    let rows = nova_storage::try_scan_prefix(RECOVERY_PREFIX).map_err(|e| e.to_string())?;
    let (_, raw) = rows
        .iter()
        .max_by_key(|(key, _)| recovery_sequence(key))
        .ok_or("There is no local import to undo")?;
    let receipt: Receipt = serde_json::from_str(raw).map_err(|e| e.to_string())?;
    let mut payloads = BTreeMap::new();
    let mut profiles = Vec::new();
    let mut choices = receipt.choices.clone();
    for choice in &mut choices {
        choice.source = choice.target.clone();
        choice.create = false;
        choice.replace = true;
        // Import conflict grants are not Undo grants. Every compensation gets
        // a fresh review and preserves later edits until explicitly overridden.
        choice.use_backup.clear();
        choice.components.remove("packages");
        profiles.push(archive::Profile {
            archive_profile_id: choice.target.clone(),
            source_profile_id: choice.target.clone(),
            name: choice.name.clone(),
            components: receipt
                .components
                .iter()
                .filter(|component| {
                    component.target == choice.target && component.name != "packages"
                })
                .map(|component| archive::Component {
                    name: component.name.clone(),
                    schema_version: 1,
                    path: format!("profiles/{}/{}.json", choice.target, component.name),
                    item_count: component.before.records.len(),
                    sha256: String::new(),
                })
                .collect(),
        });
    }
    let mut undo_after = BTreeMap::new();
    for component in receipt.components {
        undo_after.insert(
            (component.target.clone(), component.name.clone()),
            component.after,
        );
        payloads.insert((component.target, component.name), component.before);
    }
    let archive = Archive {
        manifest: archive::Manifest {
            format: "nova-backup".into(),
            version: 1,
            created_at: receipt.created_at,
            app_version: env!("CARGO_PKG_VERSION").into(),
            archive_id: format!("undo-{}", receipt.id),
            profiles,
            packages: Vec::new(),
        },
        payloads,
        checksum: receipt.checksum,
        unsupported: Vec::new(),
        undo_after: Some(undo_after),
        packages: BTreeMap::new(),
        undo_local: Some((receipt.local_before, receipt.local_after)),
    };
    // Undo always gets a fresh exact preview. Intervening edits are visible
    // here and remain bound to the new target revisions until Apply.
    preview(archive, choices)
}
fn undo_targets() -> Result<Vec<String>, String> {
    let rows = nova_storage::try_scan_prefix(RECOVERY_PREFIX).map_err(|e| e.to_string())?;
    let (_, raw) = rows
        .iter()
        .max_by_key(|(key, _)| recovery_sequence(key))
        .ok_or("There is no local import to undo")?;
    let receipt: Receipt = serde_json::from_str(raw).map_err(|e| e.to_string())?;
    Ok(receipt
        .choices
        .into_iter()
        .map(|choice| choice.target)
        .collect())
}

/// Revert only fields still equal to our receipt. Later edits are independent
/// conflicts and remain local unless the review explicitly chooses backup.
fn undo_merge(
    current: &Payload,
    before: &Payload,
    imported: &Payload,
    use_backup: bool,
) -> Payload {
    let mut restored = current.clone();
    for key in before
        .records
        .keys()
        .chain(imported.records.keys())
        .collect::<BTreeSet<_>>()
    {
        if use_backup || current.records.get(key) == imported.records.get(key) {
            match before.records.get(key) {
                Some(value) => {
                    restored.records.insert(key.clone(), value.clone());
                }
                None => {
                    restored.records.remove(key);
                }
            }
        }
    }
    restored
}

pub(crate) fn install(app: &crate::AppWindow) {
    ui::install(app);
}

#[cfg(test)]
mod tests;
