use super::*;

fn connection(account: &str, epoch: &str, mode: SyncMode) -> Connection {
    Connection {
        version: 1,
        account_id: account.into(),
        email: "viewer@example.invalid".into(),
        auth_key: Secret::new("token"),
        epoch: epoch.into(),
        automatic: true,
        started: true,
        bootstrap_done: true,
        mode,
    }
}

fn selected(native: &str) -> Selection {
    Selection {
        version: 1,
        enabled: true,
        native_type: "series".into(),
        native_id: native.into(),
        remote_type: "series".into(),
        remote_id: "tt0903747".into(),
        name: "Breaking Bad".into(),
        episode_ids: [("tt0903747:1:1".into(), format!("{native}:episode-1"))].into(),
        native_revision: 10,
        remote_revision: 20,
        history_since_ms: 1000,
        revision: "revision-1".into(),
    }
}

fn title(selection: &Selection) -> Title {
    let mut title = Title::new(LibraryItem::new(
        &selection.remote_id,
        &selection.name,
        &selection.remote_type,
        1,
    ));
    title.mapping.media_type = selection.native_type.clone();
    title.mapping.id = selection.native_id.clone();
    title.mapping.episode_ids = selection.episode_ids.clone();
    title.binding = Some(NativeBinding {
        media_type: selection.native_type.clone(),
        id: selection.native_id.clone(),
    });
    title.selection = Some(selection.clone());
    // Previously queued work is never evidence of current consent.
    title.pending = true;
    title.desired.saved = Stamped {
        value: true,
        changed_at: 2000,
    };
    title
}

fn install(store: &mut nova_sync::Store, connection: &Connection, selection: &Selection) {
    let domain = crate::app::sync::profile_domain(DOMAIN);
    store.set(
        &domain,
        CONNECTION,
        Some(serde_json::to_string(connection).unwrap()),
        0,
        1,
    );
    store.set(
        &domain,
        &selection.key(&connection.account_id),
        Some(serde_json::to_string(selection).unwrap()),
        0,
        1,
    );
}

#[test]
fn old_connections_keep_full_library_mode_until_an_explicit_session_preference() {
    let connection = connection("account", "epoch", SyncMode::FullLibrary);
    let mut raw = serde_json::to_value(&connection).unwrap();
    raw.as_object_mut().unwrap().remove("mode");
    let legacy: Connection = serde_json::from_value(raw).unwrap();
    assert!(legacy.valid());
    assert_eq!(legacy.mode, SyncMode::FullLibrary);

    let selection = selected("native-a");
    let mut store = nova_sync::Store::default();
    install(&mut store, &legacy, &selection);
    let domain = crate::app::sync::profile_domain(DOMAIN);
    let credentials_before = store.record(&domain, CONNECTION).unwrap().clone();
    let effective = write_session_flag(
        &mut store,
        &legacy,
        SessionFlag::Mode(SyncMode::SelectedTitles),
        2,
    )
    .unwrap();
    assert_eq!(effective.mode, SyncMode::SelectedTitles);
    assert_eq!(store.record(&domain, CONNECTION), Some(&credentials_before));

    // A stale preference writer must observe the separately merged mode.
    let effective =
        write_session_flag(&mut store, &legacy, SessionFlag::Automatic(false), 1).unwrap();
    assert_eq!(effective.mode, SyncMode::SelectedTitles);
    assert!(!effective.automatic);
    let mut other_epoch = connection.clone();
    other_epoch.epoch = "new-sign-in".into();
    session_flags(&store, &mut other_epoch).unwrap();
    assert_eq!(other_epoch.mode, SyncMode::FullLibrary);
}

#[test]
fn selected_mode_rejects_cached_canonical_and_another_native_under_the_same_parent() {
    let selection = selected("native-a");
    let selected_title = title(&selection);
    let selections = vec![selection.clone()];
    assert!(allowed(
        SyncMode::SelectedTitles,
        &selections,
        &selected_title
    ));

    let mut canonical = Title::new(selected_title.item.clone());
    canonical.pending = true;
    canonical.desired = selected_title.desired.clone();
    assert!(!allowed(SyncMode::SelectedTitles, &selections, &canonical));

    let mut other_native = title(&selected("native-b"));
    // Even an accidentally retained consent snapshot cannot authorize a
    // different source that happens to resolve to the same Stremio parent.
    other_native.selection = Some(selection);
    assert!(!allowed(
        SyncMode::SelectedTitles,
        &selections,
        &other_native
    ));
    assert!(allowed(SyncMode::FullLibrary, &[], &canonical));
}

#[test]
fn peer_unlink_revokes_cached_pending_work_without_deleting_history() {
    let connection = connection("account", "epoch", SyncMode::SelectedTitles);
    let selection = selected("native-a");
    let title = title(&selection);
    let mut store = nova_sync::Store::default();
    install(&mut store, &connection, &selection);
    assert!(authorize(&store, &connection, &title).is_ok());

    let domain = crate::app::sync::profile_domain(DOMAIN);
    store.set(&domain, &selection.key(&connection.account_id), None, 0, 2);
    let active = load(&store, &connection).unwrap();
    assert!(active.is_empty());
    assert!(!allowed(connection.mode, &active, &title));
    assert!(!allowed(SyncMode::FullLibrary, &active, &title));
    assert!(matches!(
        authorize(&store, &connection, &title),
        Err(ApiError::Rejected)
    ));
    assert!(title.pending && title.desired.saved.value);
}

#[test]
fn relinking_the_same_identity_invalidates_an_older_consent_revision() {
    let connection = connection("account", "epoch", SyncMode::SelectedTitles);
    let old = selected("native-a");
    let cached = title(&old);
    let mut renewed = old.clone();
    renewed.revision = "revision-2".into();
    renewed.history_since_ms = 3000;
    let mut store = nova_sync::Store::default();
    install(&mut store, &connection, &renewed);
    assert!(!allowed(
        connection.mode,
        &load(&store, &connection).unwrap(),
        &cached
    ));
    assert!(matches!(
        authorize(&store, &connection, &cached),
        Err(ApiError::Rejected)
    ));
    assert!(authorize(&store, &connection, &title(&renewed)).is_ok());
}

#[test]
fn mode_change_received_from_a_peer_stops_an_inflight_full_library_pass() {
    let full = connection("account", "epoch", SyncMode::FullLibrary);
    let selection = selected("native-a");
    let mut store = nova_sync::Store::default();
    install(&mut store, &full, &selection);
    let canonical = Title::new(LibraryItem::new("tt0903747", "Breaking Bad", "series", 1));
    assert!(authorize(&store, &full, &canonical).is_ok());
    write_session_flag(
        &mut store,
        &full,
        SessionFlag::Mode(SyncMode::SelectedTitles),
        2,
    )
    .unwrap();
    assert!(matches!(
        authorize(&store, &full, &canonical),
        Err(ApiError::Rejected)
    ));
}

#[test]
fn selected_authorization_rechecks_native_and_remote_identity_at_delivery() {
    let connection = connection("account", "epoch", SyncMode::SelectedTitles);
    let selection = selected("native-a");
    let mut store = nova_sync::Store::default();
    install(&mut store, &connection, &selection);
    let mut retargeted = title(&selection);
    retargeted.mapping.id = "native-b".into();
    assert!(matches!(
        authorize(&store, &connection, &retargeted),
        Err(ApiError::Rejected)
    ));
    retargeted = title(&selection);
    retargeted.item = LibraryItem::new("tt0944947", "Game of Thrones", "series", 1);
    assert!(matches!(
        authorize(&store, &connection, &retargeted),
        Err(ApiError::Rejected)
    ));
}

#[test]
fn selections_are_account_scoped_and_malformed_storage_is_not_consent() {
    let connection = connection("account", "epoch", SyncMode::SelectedTitles);
    let selection = selected("native-a");
    let mut store = nova_sync::Store::default();
    install(&mut store, &connection, &selection);
    let mut other = connection.clone();
    other.account_id = "another-account".into();
    assert!(load(&store, &other).unwrap().is_empty());
    assert!(matches!(
        authorize(&store, &other, &title(&selection)),
        Err(ApiError::Rejected)
    ));

    let domain = crate::app::sync::profile_domain(DOMAIN);
    store.set(
        &domain,
        &selection_key(&connection.account_id, "series", "different-native"),
        Some(serde_json::to_string(&selection).unwrap()),
        0,
        2,
    );
    assert!(matches!(
        load(&store, &connection),
        Err(ApiError::InvalidResponse)
    ));
}

#[test]
fn duplicate_native_episode_targets_are_invalid_consent() {
    let mut selection = selected("native-a");
    selection
        .episode_ids
        .insert("tt0903747:1:2".into(), "native-a:episode-1".into());
    assert!(!selection.valid());
}

#[test]
fn newly_selected_history_excludes_old_pending_and_unmapped_shared_fields() {
    let selection = selected("native-a");
    let mapped = "tt0903747:1:1".to_owned();
    let unmapped = "tt0903747:2:1".to_owned();
    let mut shared = Canonical {
        saved: Stamped {
            value: true,
            changed_at: 500,
        },
        watched: [
            (
                mapped.clone(),
                Stamped {
                    value: true,
                    changed_at: 900,
                },
            ),
            (
                unmapped.clone(),
                Stamped {
                    value: true,
                    changed_at: 2000,
                },
            ),
        ]
        .into(),
        resume: Stamped {
            value: Some(Resume {
                video_id: unmapped,
                position_ms: 10000,
                duration_ms: 60000,
            }),
            changed_at: 2000,
        },
    };
    restrict_shared(Some(&selection), &mut shared);
    assert!(shared.watched.is_empty());
    assert_eq!(shared.resume, Stamped::default());
    // Library membership is independent of the opt-in for older watch history.
    assert!(shared.saved.value);

    shared.watched.insert(
        mapped.clone(),
        Stamped {
            value: false,
            changed_at: 2000,
        },
    );
    shared.resume = Stamped {
        value: Some(Resume {
            video_id: mapped.clone(),
            position_ms: 15000,
            duration_ms: 60000,
        }),
        changed_at: 2000,
    };
    restrict_shared(Some(&selection), &mut shared);
    assert_eq!(shared.watched[&mapped].changed_at, 2000);
    assert!(!shared.watched[&mapped].value);
    assert_eq!(shared.resume.value.unwrap().video_id, mapped);
}

#[test]
fn include_history_keeps_only_mapped_history_and_full_mode_keeps_its_original_view() {
    let mut selection = selected("native-a");
    selection.history_since_ms = 0;
    let mapped = "tt0903747:1:1".to_owned();
    let unmapped = "tt0903747:2:1".to_owned();
    let full = Canonical {
        watched: [
            (
                mapped.clone(),
                Stamped {
                    value: true,
                    changed_at: 500,
                },
            ),
            (
                unmapped,
                Stamped {
                    value: true,
                    changed_at: 2000,
                },
            ),
        ]
        .into(),
        ..Default::default()
    };
    let mut selected_view = full.clone();
    restrict_shared(Some(&selection), &mut selected_view);
    assert_eq!(selected_view.watched.len(), 1);
    assert!(selected_view.watched[&mapped].value);
    let mut full_view = full.clone();
    restrict_shared(None, &mut full_view);
    assert_eq!(full_view, full);
}

#[test]
fn explicit_exclusion_overrides_full_library_including_its_canonical_import() {
    let mut excluded = selected("native-a");
    excluded.enabled = false;
    let connection = connection("account", "epoch", SyncMode::FullLibrary);
    let bound = title(&excluded);
    let canonical = Title::new(bound.item.clone());
    assert!(!allowed(connection.mode, &[excluded.clone()], &bound));
    assert!(!allowed(connection.mode, &[excluded.clone()], &canonical));
    let mut store = nova_sync::Store::default();
    install(&mut store, &connection, &excluded);
    store.set(
        &crate::app::sync::profile_domain(DOMAIN),
        &remote_exclusion_key(&connection.account_id, &excluded),
        Some("true".into()),
        0,
        1,
    );
    assert_eq!(
        authorize(&store, &connection, &canonical),
        Err(ApiError::Rejected)
    );
    let unrelated = Title::new(LibraryItem::new("tt-unrelated", "Other show", "series", 1));
    assert!(allowed(connection.mode, &[excluded], &unrelated));
    assert!(authorize(&store, &connection, &unrelated).is_ok());
}

#[test]
fn consent_controls_remain_deliverable_when_normal_requests_fill_the_queue() {
    let handle = Handle::new();
    for _ in 0..32 {
        assert!(handle.tx.try_send(Command::Sync(0)).is_ok());
    }
    handle.send(Command::Mode(SyncMode::SelectedTitles, 1));
    handle.send(Command::Review(Request::Confirm(false, 1), 1));
    assert!(matches!(
        handle.mode_change.lock().unwrap().take(),
        Some(Command::Mode(SyncMode::SelectedTitles, 1))
    ));
    assert!(matches!(
        handle.selection_change.lock().unwrap().take(),
        Some(Command::Review(Request::Confirm(false, 1), 1))
    ));
}

#[test]
fn appended_new_seasons_do_not_revoke_or_extend_approved_episode_coverage() {
    let selection = selected("native-a");
    let mut local = stremio_metadata::LocalTitle {
        media_type: selection.native_type.clone(),
        id: selection.native_id.clone(),
        videos: vec![
            Video {
                id: "native-a:episode-1".into(),
                ..Default::default()
            },
            Video {
                id: "native-a:new-season".into(),
                ..Default::default()
            },
        ],
    };
    let remote = vec![
        nova_stremio::Video {
            id: "tt0903747:2:1".into(),
            ..Default::default()
        },
        nova_stremio::Video {
            id: "tt0903747:1:1".into(),
            ..Default::default()
        },
    ];
    assert!(approved_pairs_present(&selection, &local, &remote));
    assert_eq!(selection.episode_ids.len(), 1);
    local.videos.remove(0);
    assert!(!approved_pairs_present(&selection, &local, &remote));
}

#[test]
fn delivery_cannot_replace_an_approved_episode_pair_with_another_episode() {
    let selected = selected("native-a");
    let connection = connection("account", "epoch", SyncMode::SelectedTitles);
    let mut store = nova_sync::Store::default();
    install(&mut store, &connection, &selected);
    let mut incoming = title(&selected);
    incoming
        .mapping
        .episode_ids
        .insert("tt0903747:1:1".into(), "native-a:other-episode".into());
    assert!(!allowed(connection.mode, &[selected], &incoming));
    assert_eq!(
        authorize(&store, &connection, &incoming),
        Err(ApiError::Rejected)
    );
}
