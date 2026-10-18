use super::*;

fn connection(epoch: &str) -> Connection {
    Connection {
        version: 1,
        account_id: "account".into(),
        email: "account@example.invalid".into(),
        auth_key: Secret::new("token"),
        epoch: epoch.into(),
        automatic: true,
        started: false,
        bootstrap_done: false,
        mode: SyncMode::default(),
    }
}
fn install(store: &mut nova_sync::Store, connection: &Connection) {
    store.set(
        DOMAIN,
        CONNECTION,
        Some(serde_json::to_string(connection).unwrap()),
        0,
        1,
    );
}

#[test]
fn stale_automatic_snapshot_cannot_reset_merge_markers_or_rewrite_credentials() {
    let mut store = nova_sync::Store::default();
    let stale = connection("epoch");
    install(&mut store, &stale);
    let before = store.record(DOMAIN, CONNECTION).unwrap().clone();
    write_session_flag(&mut store, &stale, SessionFlag::Started, 1).unwrap();
    write_session_flag(&mut store, &stale, SessionFlag::Bootstrap, 1).unwrap();
    let effective =
        write_session_flag(&mut store, &stale, SessionFlag::Automatic(false), 2).unwrap();
    assert!(effective.started && effective.bootstrap_done);
    assert!(!effective.automatic);
    assert_eq!(store.record(DOMAIN, CONNECTION), Some(&before));
    assert_eq!(
        store
            .record(DOMAIN, &session_key(&stale, "started"))
            .unwrap()
            .value
            .as_deref(),
        Some("true")
    );
    assert_eq!(
        store
            .record(DOMAIN, &session_key(&stale, "bootstrap"))
            .unwrap()
            .value
            .as_deref(),
        Some("true")
    );
}

#[test]
fn old_epoch_cannot_write_preferences_after_reauthentication_with_same_token() {
    let mut store = nova_sync::Store::default();
    let old = connection("old");
    install(&mut store, &old);
    write_session_flag(&mut store, &old, SessionFlag::Bootstrap, 1).unwrap();
    let new = connection("new");
    install(&mut store, &new);
    for flag in [
        SessionFlag::Automatic(false),
        SessionFlag::Started,
        SessionFlag::Bootstrap,
    ] {
        assert!(matches!(
            write_session_flag(&mut store, &old, flag, 2),
            Err(ApiError::Authentication)
        ));
    }
    let mut effective = new.clone();
    session_flags(&store, &mut effective).unwrap();
    assert!(effective.automatic);
    assert!(!effective.started && !effective.bootstrap_done);
}

#[test]
fn tombstone_cannot_be_restored_by_any_session_marker_write() {
    let mut store = nova_sync::Store::default();
    let connection = connection("epoch");
    install(&mut store, &connection);
    store.set(DOMAIN, CONNECTION, None, 0, 2);
    let before = store.record(DOMAIN, CONNECTION).unwrap().clone();
    assert!(matches!(
        write_session_flag(&mut store, &connection, SessionFlag::Automatic(true), 1),
        Err(ApiError::Authentication)
    ));
    assert_eq!(store.record(DOMAIN, CONNECTION), Some(&before));
}

#[test]
fn peer_markers_are_read_with_unchanged_connection_row_and_do_not_mix_epochs() {
    let mut store = nova_sync::Store::default();
    let original = connection("a:b");
    install(&mut store, &original);
    let before = store.record(DOMAIN, CONNECTION).unwrap().clone();
    let raw = "true".to_owned();
    store.set(DOMAIN, &session_key(&original, "started"), Some(raw), 0, 2);
    let mut effective = original.clone();
    session_flags(&store, &mut effective).unwrap();
    assert!(effective.started);
    assert_eq!(store.record(DOMAIN, CONNECTION), Some(&before));
    let mut other = connection("a");
    session_flags(&store, &mut other).unwrap();
    assert!(!other.started);
}

#[test]
fn legacy_connection_seeds_flags_until_session_records_exist() {
    let mut store = nova_sync::Store::default();
    let mut seed = connection("epoch");
    seed.started = true;
    seed.bootstrap_done = true;
    install(&mut store, &seed);
    let mut effective = seed.clone();
    session_flags(&store, &mut effective).unwrap();
    assert!(effective.started && effective.bootstrap_done && effective.automatic);
    write_session_flag(&mut store, &seed, SessionFlag::Automatic(false), 2).unwrap();
    session_flags(&store, &mut effective).unwrap();
    assert!(!effective.automatic);
    let mut raw = serde_json::to_value(&seed).unwrap();
    raw.as_object_mut().unwrap().remove("epoch");
    let legacy: Connection = serde_json::from_value(raw).unwrap();
    assert!(legacy.epoch.is_empty() && legacy.valid());
}
