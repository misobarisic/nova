use super::*;

#[test]
fn disconnect_remains_deliverable_when_the_normal_queue_is_full() {
    let handle = Handle::new();
    for _ in 0..32 {
        assert!(handle.tx.try_send(Command::Sync(0)).is_ok());
    }
    handle.send(Command::Disconnect(7));
    assert!(matches!(
        handle.urgent.lock().unwrap().take(),
        Some(Command::Disconnect(7))
    ));
}

#[test]
fn membership_only_preserves_remote_state() {
    let mut item: LibraryItem = serde_json::from_value(serde_json::json!({
        "_id":"tt1","type":"series","removed":false,"temp":false,
        "state":{"watched":"opaque","timeOffset":42,"video_id":"ep","future":123}
    }))
    .unwrap();
    let before = item.0["state"].clone();
    item.apply_membership(false, 200);
    assert_eq!(item.0["state"], before);
    assert!(!membership_projection(&item).saved);
}

#[test]
fn shared_field_versions_preserve_original_time_and_remote_tie_priority() {
    let mut store = nova_sync::Store::default();
    let put = |store: &mut nova_sync::Store, key: &str, value: bool, stamp: u64, remote: bool| {
        let raw = serde_json::to_string(&Stamped {
            value,
            changed_at: stamp,
        })
        .unwrap();
        store.apply(
            DOMAIN,
            key,
            nova_sync::Record {
                version: nova_sync::Version::new(
                    stamp,
                    u32::from(remote),
                    nova_config::fnv1a(raw.as_bytes()),
                    false,
                ),
                value: Some(raw),
            },
        );
    };
    put(&mut store, "saved", true, 200, false);
    put(&mut store, "saved", false, 100, true);
    let value: Stamped<bool> = serde_json::from_str(
        store
            .record(DOMAIN, "saved")
            .unwrap()
            .value
            .as_ref()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        value,
        Stamped {
            value: true,
            changed_at: 200
        }
    );
    put(&mut store, "saved", false, 200, true);
    put(&mut store, "saved", true, 200, false);
    let value: Stamped<bool> = serde_json::from_str(
        store
            .record(DOMAIN, "saved")
            .unwrap()
            .value
            .as_ref()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        value,
        Stamped {
            value: false,
            changed_at: 200
        }
    );
    assert_eq!(store.record(DOMAIN, "saved").unwrap().version.ts, 200);
}

#[test]
fn deferred_progress_merge_marker_survives_restart_and_old_rows() {
    let mut title = Title::new(LibraryItem::new("tt1", "Title", "series", 100));
    assert!(!title.progress_merged);
    let mut raw = serde_json::to_value(&title).unwrap();
    raw.as_object_mut().unwrap().remove("progress_merged");
    let old: Title = serde_json::from_value(raw).unwrap();
    assert!(!old.progress_merged);
    title.progress_merged = true;
    let loaded: Title = serde_json::from_str(&serde_json::to_string(&title).unwrap()).unwrap();
    assert!(loaded.progress_merged);
}
