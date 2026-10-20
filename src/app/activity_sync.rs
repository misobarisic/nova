//! Per-scene and per-session sync records keep independent device edits separate.
use super::*;
use bookmarks::{Bookmark, Bookmarks};
use std::sync::mpsc;

pub(super) const BOOKMARKS: &str = "scene_bookmarks";
pub(super) const STATS: &str = "viewing_sessions";

#[derive(Serialize, Deserialize)]
struct Scene {
    title: String,
    bookmark: Bookmark,
}

pub(super) fn bookmark_records(raw: &str) -> nova_sync::Result<Vec<(String, String, u64)>> {
    let values: Bookmarks = serde_json::from_str(raw)?;
    values
        .into_iter()
        .flat_map(|(title, rows)| {
            rows.into_iter().map(move |bookmark| {
                let id = bookmarks::scene_id(&title, &bookmark);
                let at = bookmark.added_at / 1000;
                Ok((
                    id,
                    serde_json::to_string(&Scene {
                        title: title.clone(),
                        bookmark,
                    })?,
                    at,
                ))
            })
        })
        .collect()
}

pub(super) fn bookmark_snapshot(records: &[(String, String)]) -> nova_sync::Result<String> {
    let mut values = Bookmarks::new();
    for (id, raw) in records {
        let scene: Scene = serde_json::from_str(raw)?;
        if bookmarks::scene_id(&scene.title, &scene.bookmark) != *id
            || !scene.bookmark.position.is_finite()
            || scene.bookmark.position < 0.0
        {
            return Err(storage::Error::new(
                storage::ErrorKind::Schema,
                "invalid synced scene identity",
            )
            .into());
        }
        values.entry(scene.title).or_default().push(scene.bookmark);
    }
    for rows in values.values_mut() {
        rows.sort_by(|a, b| {
            a.position
                .total_cmp(&b.position)
                .then_with(|| bookmarks::bookmark_id(a).cmp(&bookmarks::bookmark_id(b)))
        });
    }
    Ok(serde_json::to_string(&values)?)
}

/// A checkpoint changes only its own session; it must never delete sessions
/// created on another device. The record and local row commit together.
pub(super) fn persist_session(
    owner: &profiles::ProfileStore,
    key: &str,
    raw: &str,
) -> nova_sync::Result<()> {
    let id = viewing_journal::validate_session(key, raw)?;
    if owner.retired()? {
        return Err(
            storage::Error::new(storage::ErrorKind::Schema, "profile has been deleted").into(),
        );
    }
    let device = nova_sync::local_device()?;
    let handle = nova_sync::local_store()?;
    let domain = owner.domain(STATS);
    let mut store = handle.lock().unwrap();
    if store
        .record("profile_deletions", owner.id())
        .is_some_and(|record| !record.is_deleted())
    {
        return Err(
            storage::Error::new(storage::ErrorKind::Schema, "profile has been deleted").into(),
        );
    }
    let dirty = store.has_dirty_records();
    // A failed transaction can leave a newer checkpoint in the mutation
    // owner while disk still shows the older row. Consult that pending value
    // too, otherwise a delayed worker could overwrite its final duration.
    if let Some(record) = store.record(&domain, &id) {
        let keep = record
            .value
            .as_deref()
            .map(|old| viewing_journal::session_can_replace(old, raw))
            .transpose()?;
        if keep != Some(true) {
            let retained = record.value.clone();
            store.queue_extra(&owner.key(key), retained);
            store.save()?;
            drop(store);
            if dirty {
                nova_sync::request_local_sync();
            }
            return Ok(());
        }
    }
    let changed = if store
        .record(&domain, &id)
        .and_then(|record| record.value.as_deref())
        != Some(raw)
    {
        let raw = nova_sync::preserve_unknown(
            store
                .record(&domain, &id)
                .and_then(|record| record.value.as_deref()),
            raw,
        );
        store.set(&domain, &id, Some(raw), 0, device)
    } else {
        false
    };
    let materialized = store
        .record(&domain, &id)
        .and_then(|record| record.value.clone());
    store.queue_extra(&owner.key(key), materialized);
    store.save()?;
    drop(store);
    if changed || dirty {
        nova_sync::request_local_sync();
    }
    Ok(())
}

/// Successful legacy scans run once per process/owner. A process-only marker
/// deliberately permits a fresh scan after a recovery/restart instead of
/// permanently trusting a marker from a quarantined sync store.
/// Failed scans are retryable on the next startup/activation request.
struct MigrationQueue {
    queued: Arc<Mutex<HashSet<String>>>,
    sender: mpsc::Sender<profiles::ProfileStore>,
}

impl MigrationQueue {
    fn new(
        run: impl Fn(&profiles::ProfileStore) -> nova_sync::Result<()> + Send + 'static,
    ) -> Self {
        let (sender, receiver) = mpsc::channel::<profiles::ProfileStore>();
        let queued = Arc::new(Mutex::new(HashSet::new()));
        let pending = queued.clone();
        std::thread::Builder::new()
            .name("viewing-session-migration".into())
            .spawn(move || {
                for owner in receiver {
                    if let Err(error) = run(&owner) {
                        pending.lock().unwrap().remove(&owner.domain(STATS));
                        storage::report(storage::Error::new(
                            storage::ErrorKind::Transaction,
                            error,
                        ));
                    }
                }
            })
            .expect("spawn viewing session migration");
        Self { queued, sender }
    }

    fn enqueue(&self, owner: profiles::ProfileStore) {
        let key = owner.domain(STATS);
        let inserted = self.queued.lock().unwrap().insert(key.clone());
        if inserted && self.sender.send(owner).is_err() {
            self.queued.lock().unwrap().remove(&key);
        }
    }
}

static MIGRATIONS: LazyLock<MigrationQueue> = LazyLock::new(|| MigrationQueue::new(seed_sessions));

pub(super) fn seed_sessions_background(owner: profiles::ProfileStore) {
    MIGRATIONS.enqueue(owner);
}

fn seed_sessions(owner: &profiles::ProfileStore) -> nova_sync::Result<()> {
    seed_sessions_after_scan(owner, || {})
}

fn seed_sessions_after_scan(
    owner: &profiles::ProfileStore,
    after_scan: impl FnOnce(),
) -> nova_sync::Result<()> {
    if owner.retired()? {
        return Ok(());
    }
    // Decode old archives before taking any writer/store mutex. An in-flight
    // checkpoint wins below because migration only creates absent records.
    let rows = owner
        .scan(viewing_journal::PREFIX)?
        .into_iter()
        .filter_map(|(key, raw)| {
            viewing_journal::validate_session(&key, &raw)
                .ok()
                .map(|id| (id, raw))
        })
        .collect::<Vec<_>>();
    after_scan();
    let device = nova_sync::local_device()?;
    let handle = nova_sync::local_store()?;
    let mut store = handle.lock().unwrap();
    if store
        .record("profile_deletions", owner.id())
        .is_some_and(|record| !record.is_deleted())
    {
        return Ok(());
    }
    let domain = owner.domain(STATS);
    let mut changed = store.has_dirty_records();
    for (id, raw) in rows {
        // Existing checkpoints and tombstones are authoritative during migration.
        if store.record(&domain, &id).is_none() {
            changed |= store.set(&domain, &id, Some(raw), 0, device);
        }
    }
    store.save()?;
    drop(store);
    if changed {
        nova_sync::request_local_sync();
    }
    Ok(())
}

pub(super) fn project_sessions(
    owner: &profiles::ProfileStore,
) -> nova_sync::Result<HashSet<String>> {
    project_sessions_before_commit(owner, |_| {})
}

fn project_sessions_before_commit(
    owner: &profiles::ProfileStore,
    mut before_commit: impl FnMut(usize),
) -> nova_sync::Result<HashSet<String>> {
    let handle = nova_sync::local_store()?;
    let domain = owner.domain(STATS);
    loop {
        let (rows, versions, deleted) = {
            let store = handle.lock().unwrap();
            if store
                .record("profile_deletions", owner.id())
                .is_some_and(|record| !record.is_deleted())
            {
                return Ok(HashSet::new());
            }
            let versions = store.versions_with_prefix(&domain, "");
            let deleted: HashSet<String> = versions
                .iter()
                .filter(|(id, _)| {
                    store
                        .record(&domain, id)
                        .is_some_and(|record| record.is_deleted())
                })
                .map(|(id, _)| id.clone())
                .collect();
            (store.records(&domain), versions, deleted)
        };
        // Decode and compare the archive outside both mutation locks. Final
        // checkpoints remain durable without waiting for full-history parsing.
        let materialized: HashMap<_, _> =
            owner.scan(viewing_journal::PREFIX)?.into_iter().collect();
        let mut changes = Vec::new();
        for (id, raw) in rows {
            let key = format!("{}{id}", viewing_journal::PREFIX);
            viewing_journal::validate_session(&key, &raw)?;
            if materialized.get(&key) != Some(&raw) {
                changes.push((owner.key(&key), Some(raw)));
            }
        }
        for id in &deleted {
            let key = format!("{}{id}", viewing_journal::PREFIX);
            if materialized.contains_key(&key) {
                changes.push((owner.key(&key), None));
            }
        }
        before_commit(changes.len());
        let writer = viewing_journal::writer(owner);
        let _serial = writer.lock().unwrap();
        let mut store = handle.lock().unwrap();
        if store
            .record("profile_deletions", owner.id())
            .is_some_and(|record| !record.is_deleted())
        {
            return Ok(HashSet::new());
        }
        // A receive or local checkpoint may have changed while decoding. Retry
        // before publishing any stale materialized row or acknowledgement.
        if store.versions_with_prefix(&domain, "") != versions {
            continue;
        }
        for (key, raw) in changes {
            store.queue_extra(&key, raw);
        }
        if versions.is_empty() {
            store.save()?;
        } else {
            store.mark_projected(&domain, &versions.into_iter().collect())?;
        }
        return Ok(deleted);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn session(id: &str, seconds: f64) -> String {
        json!({"id":id,"media_type":"movie","title_id":"film","episode_id":"film",
            "title":"Film","episode":"","started_at":1767312000u64,"updated_at":1767312000u64,
            "ended_at":1767312300u64,"watched_seconds":seconds,"months":{"2026-01":seconds},
            "days":{"20455":seconds},"completed_at":null,"title_completed_at":null,"rewatch":false})
        .to_string()
    }
    fn exchange(a: &mut nova_sync::Store, b: &mut nova_sync::Store) {
        let aa = a.outbound(&b.digest());
        let bb = b.outbound(&a.digest());
        for item in aa {
            b.apply(&item.domain, &item.key, item.record);
        }
        for item in bb {
            a.apply(&item.domain, &item.key, item.record);
        }
    }
    #[test]
    fn background_migration_and_projection_preserve_owner_and_concurrent_checkpoints() {
        const ENV: &str = "NOVA_ACTIVITY_WORKER_TEST_ROOT";
        let Some(root) = std::env::var_os(ENV) else {
            let root = std::env::temp_dir().join(format!(
                "nova-activity-worker-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(ENV, &root)
                .args(["--exact", "app::activity_sync::tests::background_migration_and_projection_preserve_owner_and_concurrent_checkpoints", "--nocapture"])
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
        let bob = profiles::ProfileStore::for_profile(&profiles::create("Bob").unwrap());
        let key = format!("{}legacy", viewing_journal::PREFIX);
        alice.write(&key, &session("legacy", 100.0)).unwrap();
        bob.write(&key, &session("legacy", 777.0)).unwrap();
        let (release, gate) = mpsc::channel();
        let (done, completed) = mpsc::channel();
        let first = alice.domain(STATS);
        let first_run = AtomicBool::new(true);
        let queue = MigrationQueue::new(move |owner| {
            if owner.domain(STATS) == first && first_run.swap(false, Ordering::SeqCst) {
                gate.recv().unwrap();
            }
            seed_sessions(owner)?;
            done.send(owner.domain(STATS)).unwrap();
            Ok(())
        });
        // Calls return while the first worker is deliberately held. Repeated
        // startup/activation requests do not enqueue duplicate archive scans.
        queue.enqueue(alice.clone());
        queue.enqueue(alice.clone());
        queue.enqueue(bob.clone());
        queue.enqueue(alice.clone());
        release.send(()).unwrap();
        let first_owner = completed.recv_timeout(Duration::from_secs(10)).unwrap();
        queue.enqueue(alice.clone()); // A completed scan is deduplicated too.
        drop(queue);
        let owners = std::iter::once(first_owner)
            .chain(completed)
            .collect::<Vec<_>>();
        assert_eq!(owners, [alice.domain(STATS), bob.domain(STATS)]);
        let handle = nova_sync::local_store().unwrap();
        assert_eq!(
            handle
                .lock()
                .unwrap()
                .record(&alice.domain(STATS), "legacy")
                .unwrap()
                .value
                .as_deref(),
            Some(session("legacy", 100.0).as_str())
        );
        assert_eq!(
            handle
                .lock()
                .unwrap()
                .record(&bob.domain(STATS), "legacy")
                .unwrap()
                .value
                .as_deref(),
            Some(session("legacy", 777.0).as_str())
        );

        let race_key = format!("{}migration-race", viewing_journal::PREFIX);
        alice
            .write(&race_key, &session("migration-race", 10.0))
            .unwrap();
        seed_sessions_after_scan(&alice, || {
            persist_session(&alice, &race_key, &session("migration-race", 20.0)).unwrap();
        })
        .unwrap();
        assert_eq!(
            alice.read(&race_key).unwrap().as_deref(),
            Some(session("migration-race", 20.0).as_str())
        );
        assert_eq!(
            handle
                .lock()
                .unwrap()
                .record(&alice.domain(STATS), "migration-race")
                .unwrap()
                .value
                .as_deref(),
            Some(session("migration-race", 20.0).as_str())
        );

        let tombstone_key = format!("{}removed", viewing_journal::PREFIX);
        alice
            .write(&tombstone_key, &session("removed", 1.0))
            .unwrap();
        seed_sessions_after_scan(&alice, || {
            let mut store = handle.lock().unwrap();
            store.set(&alice.domain(STATS), "removed", None, 0, 9001);
            store.save().unwrap();
        })
        .unwrap();
        assert!(
            handle
                .lock()
                .unwrap()
                .record(&alice.domain(STATS), "removed")
                .unwrap()
                .is_deleted()
        );

        // Hold Alice's writer and prove Bob has an independent checkpoint lane.
        let alice_writer = viewing_journal::writer(&alice);
        let _alice_guard = alice_writer.lock().unwrap();
        let bob_writer = viewing_journal::writer(&bob);
        assert!(bob_writer.try_lock().is_ok());
        drop(_alice_guard);
        {
            let mut store = handle.lock().unwrap();
            store.set(
                &alice.domain(STATS),
                "remote",
                Some(session("remote", 40.0)),
                0,
                9001,
            );
            store.save().unwrap();
        }
        let mut first = true;
        let mut changed_rows = Vec::new();
        let deleted = project_sessions_before_commit(&alice, |changes| {
            changed_rows.push(changes);
            if std::mem::take(&mut first) {
                // Full validation runs without either mutation lock. A final
                // checkpoint here forces a fresh projection/version snapshot.
                let writer = viewing_journal::writer(&alice);
                let _serial = writer
                    .try_lock()
                    .expect("projection must not hold owner writer while parsing");
                persist_session(&alice, &race_key, &session("migration-race", 30.0)).unwrap();
            }
        })
        .unwrap();
        assert_eq!(
            changed_rows,
            [2, 2],
            "only the remote addition and removal need materialization, including after a checkpoint race"
        );
        assert!(deleted.contains("removed"));
        assert!(
            alice
                .read(&format!("{}remote", viewing_journal::PREFIX))
                .unwrap()
                .is_some()
        );
        assert!(alice.read(&tombstone_key).unwrap().is_none());
        assert_eq!(
            alice.read(&race_key).unwrap().as_deref(),
            Some(session("migration-race", 30.0).as_str())
        );
        assert_eq!(
            bob.read(&key).unwrap().as_deref(),
            Some(session("legacy", 777.0).as_str())
        );
        let digest = handle.lock().unwrap().digest();
        project_sessions_before_commit(&alice, |changes| {
            assert_eq!(
                changes, 0,
                "unchanged sessions must not be queued for rewriting"
            )
        })
        .unwrap();
        assert_eq!(
            handle.lock().unwrap().digest(),
            digest,
            "an unchanged archive projection never mints revisions"
        );

        let retired_key = format!("{}retired", viewing_journal::PREFIX);
        bob.write(&retired_key, &session("retired", 10.0)).unwrap();
        seed_sessions_after_scan(&bob, || {
            let mut store = handle.lock().unwrap();
            store.set("profile_deletions", bob.id(), Some("1".into()), 0, 9001);
            store.save().unwrap();
        })
        .unwrap();
        assert!(
            handle
                .lock()
                .unwrap()
                .record(&bob.domain(STATS), "retired")
                .is_none()
        );
        assert!(project_sessions(&bob).unwrap().is_empty());
    }

    #[test]
    fn scenes_and_sessions_merge_without_duplication_or_profile_leaks() {
        const ENV: &str = "NOVA_ACTIVITY_SYNC_TEST_ROOT";
        let Some(root) = std::env::var_os(ENV) else {
            let root = std::env::temp_dir().join(format!(
                "nova-activity-sync-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(ENV,&root).args(["--exact","app::activity_sync::tests::scenes_and_sessions_merge_without_duplication_or_profile_leaks","--nocapture"]).output().unwrap();
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
        let bob_id = profiles::create("Bob").unwrap();
        let bob = profiles::ProfileStore::for_profile(&bob_id);
        let media = nova_playback_api::MediaIdentity {
            media_type: "movie".into(),
            title_id: "film".into(),
            episode_id: "film".into(),
            title: "Film".into(),
            episode: String::new(),
        };
        let title = bookmarks::identity_key(&media);
        let first = bookmarks::new_bookmark(10.0, "first", &media, "");
        let second = bookmarks::new_bookmark(20.0, "second", &media, "");
        let first_id = bookmarks::scene_id(&title, &first);
        let second_id = bookmarks::scene_id(&title, &second);
        let local = Bookmarks::from([(title.clone(), vec![first.clone()])]);
        bookmarks::save_bookmarks(&local).unwrap();
        let domain = alice.domain(BOOKMARKS);
        let handle = nova_sync::local_store().unwrap();
        let mut peer = nova_sync::Store::default();
        let other =
            serde_json::to_string(&Bookmarks::from([(title.clone(), vec![second.clone()])]))
                .unwrap();
        for (id, raw, _) in bookmark_records(&other).unwrap() {
            peer.set(&domain, &id, Some(raw), 0, 9001);
        }
        exchange(&mut handle.lock().unwrap(), &mut peer);
        let materialize = || {
            let records = handle.lock().unwrap().records(&domain);
            let raw = bookmark_snapshot(&records).unwrap();
            let _guard = sync::ApplyingGuard::new();
            sync::persist_sync_snapshot(bookmarks::BOOKMARKS_KEY, &raw, false).unwrap();
        };
        materialize();
        assert_eq!(bookmarks::load_bookmarks().unwrap()[&title].len(), 2);
        let stale = peer.clone();
        let mut values = bookmarks::load_bookmarks().unwrap();
        values
            .get_mut(&title)
            .unwrap()
            .iter_mut()
            .find(|scene| bookmarks::scene_id(&title, scene) == first_id)
            .unwrap()
            .note = "edited on this device".into();
        bookmarks::save_bookmarks(&values).unwrap();
        peer.set(&domain, &second_id, None, 0, 9001);
        exchange(&mut handle.lock().unwrap(), &mut peer);
        materialize();
        let merged = bookmarks::load_bookmarks().unwrap();
        assert_eq!(merged[&title].len(), 1);
        assert_eq!(merged[&title][0].note, "edited on this device");
        for item in stale.outbound(&Default::default()) {
            handle
                .lock()
                .unwrap()
                .apply(&item.domain, &item.key, item.record);
        }
        assert!(
            handle
                .lock()
                .unwrap()
                .record(&domain, &second_id)
                .unwrap()
                .is_deleted()
        );
        assert!(bob.read(bookmarks::BOOKMARKS_KEY).unwrap().is_none());

        let key_a = format!("{}local-session", viewing_journal::PREFIX);
        alice
            .write(&key_a, &session("local-session", 120.0))
            .unwrap();
        seed_sessions(&alice).unwrap();
        let stats = alice.domain(STATS);
        assert!(
            handle
                .lock()
                .unwrap()
                .record(&stats, "local-session")
                .is_some()
        );
        persist_session(&alice, &key_a, &session("local-session", 180.0)).unwrap();
        peer.set(
            &stats,
            "remote-session",
            Some(session("remote-session", 60.0)),
            0,
            9001,
        );
        exchange(&mut handle.lock().unwrap(), &mut peer);
        project_sessions(&alice).unwrap();
        let rows = alice.scan(viewing_journal::PREFIX).unwrap();
        assert_eq!(rows.len(), 2);
        let total: f64 = rows
            .iter()
            .map(|(_, raw)| {
                serde_json::from_str::<serde_json::Value>(raw).unwrap()["watched_seconds"]
                    .as_f64()
                    .unwrap()
            })
            .sum();
        assert_eq!(
            total, 240.0,
            "the session checkpoint replaces its own value rather than being counted twice"
        );
        assert!(persist_session(&alice, &key_a, &session("wrong-id", 1.0)).is_err());
        bob.write(&key_a, &session("local-session", 999.0)).unwrap();
        seed_sessions(&bob).unwrap();
        project_sessions(&bob).unwrap();
        assert_eq!(alice.scan(viewing_journal::PREFIX).unwrap().len(), 2);
        assert_eq!(bob.scan(viewing_journal::PREFIX).unwrap().len(), 1);
        assert_ne!(alice.domain(STATS), bob.domain(STATS));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&bob.read(&key_a).unwrap().unwrap()).unwrap()
                ["watched_seconds"],
            999.0
        );
        let digest = handle.lock().unwrap().digest();
        project_sessions(&alice).unwrap();
        assert_eq!(
            handle.lock().unwrap().digest(),
            digest,
            "projection never echoes new session revisions"
        );
        // This is the pending mutation state left by a failed save: durable
        // storage still has 180 s while its authoritative owner retains 300 s.
        {
            let mut store = handle.lock().unwrap();
            let raw = session("local-session", 300.0);
            store.set(
                &stats,
                "local-session",
                Some(raw.clone()),
                0,
                nova_sync::local_device().unwrap(),
            );
            store.queue_extra(&alice.key(&key_a), Some(raw));
        }
        persist_session(&alice, &key_a, &session("local-session", 240.0)).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&alice.read(&key_a).unwrap().unwrap())
                .unwrap()["watched_seconds"],
            300.0,
            "a delayed older worker must flush rather than replace a newer pending checkpoint"
        );
        peer.set(&stats, "remote-session", None, 0, 9001);
        exchange(&mut handle.lock().unwrap(), &mut peer);
        let deleted = project_sessions(&alice).unwrap();
        assert!(deleted.contains("remote-session"));
        assert!(
            alice
                .read(&format!("{}remote-session", viewing_journal::PREFIX))
                .unwrap()
                .is_none()
        );
        seed_sessions(&alice).unwrap();
        assert!(
            handle
                .lock()
                .unwrap()
                .record(&stats, "remote-session")
                .unwrap()
                .is_deleted(),
            "migration never resurrects a removed session"
        );
    }
}
