//! Timing policy is tested with explicit monotonic instants, without sleeping
//! or depending on discovery network availability.
use super::*;

#[test]
fn controls_poll_playing_and_interacting_at_250ms_but_paused_at_one_second() {
    let now = Instant::now();
    let mut state = State {
        snapshot: Some(PlaybackSnapshot {
            paused: true,
            ..Default::default()
        }),
        control_poll: PeerPoll {
            attempted: Some(now),
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(!state.control_request_due(now + Duration::from_millis(999)));
    assert!(state.control_request_due(now + Duration::from_secs(1)));
    state.snapshot.as_mut().unwrap().paused = false;
    assert!(!state.control_request_due(now + Duration::from_millis(249)));
    assert!(state.control_request_due(now + Duration::from_millis(250)));
    state.snapshot.as_mut().unwrap().paused = true;
    state.interacting_until = Some(now + Duration::from_secs(1));
    assert!(state.control_request_due(now + Duration::from_millis(250)));
    assert!(!state.control_request_due(now + Duration::from_millis(249)));
    state.interacting_until = Some(now);
    assert!(!state.control_request_due(now + Duration::from_millis(250)));
}

#[test]
fn queued_commands_bypass_paused_cadence_and_offline_backoff() {
    let now = Instant::now();
    let mut state = State {
        snapshot: Some(PlaybackSnapshot {
            paused: true,
            ..Default::default()
        }),
        control_poll: PeerPoll {
            attempted: Some(now),
            failures: 4,
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(!state.control_request_due(now));
    state.queued.push_back(Request::Command {
        session: SessionId {
            epoch: 1,
            sequence: 1,
        },
        action: PlaybackAction::TogglePause,
    });
    assert!(state.control_request_due(now));
}

#[test]
fn home_and_picker_have_independent_visibility_and_session_cadence() {
    assert_eq!(DiscoveryView::Hidden.interval(false), None);
    assert_eq!(DiscoveryView::Hidden.interval(true), None);
    assert_eq!(
        DiscoveryView::Home.interval(false),
        Some(Duration::from_secs(30))
    );
    assert_eq!(
        DiscoveryView::Home.interval(true),
        Some(Duration::from_secs(5))
    );
    assert_eq!(
        DiscoveryView::Picker.interval(false),
        Some(Duration::from_secs(5))
    );
    let now = Instant::now();
    assert!(poll_due(None, now, Duration::from_secs(30)));
    assert!(!poll_due(
        Some(now),
        now + Duration::from_secs(29),
        Duration::from_secs(30)
    ));
    assert!(poll_due(
        Some(now),
        now + Duration::from_secs(30),
        Duration::from_secs(30)
    ));
}

#[test]
fn offline_peer_backoff_caps_at_one_minute_and_ready_success_resets_it() {
    let now = Instant::now();
    let mut poll = PeerPoll::default();
    for seconds in [5, 15, 30, 60, 60] {
        poll.attempted = Some(now);
        poll.complete(None);
        assert_eq!(poll.retry_delay(), Duration::from_secs(seconds));
        assert!(!poll.ready(now + Duration::from_secs(seconds - 1)));
        assert!(poll.ready(now + Duration::from_secs(seconds)));
    }
    poll.complete(Some(PlaybackSnapshot {
        ready: false,
        ..Default::default()
    }));
    assert_eq!(poll.retry_delay(), Duration::from_secs(60));
    poll.complete(Some(PlaybackSnapshot {
        ready: true,
        ..Default::default()
    }));
    assert_eq!(poll.failures, 0);
    assert!(poll.ready(now));
}

#[test]
fn discovery_preserves_skipped_status_bypasses_backoff_and_prunes_membership() {
    let now = Instant::now();
    let mut state = State::default();
    let peers = vec!["online".to_owned(), "offline".to_owned()];
    state.discovered.insert(
        "online".into(),
        PeerPoll {
            attempted: Some(now),
            snapshot: Some(PlaybackSnapshot {
                ready: true,
                title: "Movie".into(),
                ..Default::default()
            }),
            ..Default::default()
        },
    );
    state.discovered.insert(
        "offline".into(),
        PeerPoll {
            attempted: Some(now),
            failures: 4,
            ..Default::default()
        },
    );
    assert_eq!(
        state.discovery_peers(&peers, now + Duration::from_secs(5), false),
        vec!["online"]
    );
    assert_eq!(
        state.discovered["online"].snapshot.as_ref().unwrap().title,
        "Movie"
    );
    assert_eq!(
        state
            .discovery_peers(&peers, now + Duration::from_secs(6), true)
            .len(),
        2
    );
    state.discovery_peers(&peers[..1], now + Duration::from_secs(7), false);
    assert!(!state.discovered.contains_key("offline"));
}

#[test]
fn discovery_cap_rotates_oldest_attempts_without_forgetting_live_status() {
    let now = Instant::now();
    let peers = (0..70).map(|i| format!("peer-{i:02}")).collect::<Vec<_>>();
    let mut state = State::default();
    assert_eq!(state.discovery_peers(&peers, now, false), peers[..32]);
    let snapshot = PlaybackSnapshot {
        ready: true,
        ..Default::default()
    };
    state
        .discovered
        .get_mut(&peers[0])
        .unwrap()
        .complete(Some(snapshot.clone()));
    assert_eq!(
        state.discovery_peers(&peers, now + Duration::from_secs(5), false),
        peers[32..64]
    );
    let third = state.discovery_peers(&peers, now + Duration::from_secs(10), false);
    assert_eq!(&third[..6], &peers[64..]);
    assert_eq!(third.len(), 32);
    assert_eq!(state.discovered[&peers[0]].snapshot, Some(snapshot));
}

#[test]
fn engine_invalidation_retires_commands_and_lookup_generation_but_keeps_fanout_guard() {
    let mut state = State {
        generation: 9,
        role: Role::Remote,
        target: "receiver".into(),
        in_flight: true,
        lookup_in_flight: true,
        discovery_running: true,
        discovery_at: Some(Instant::now()),
        snapshot: Some(PlaybackSnapshot::default()),
        ..Default::default()
    };
    state.queued.push_back(Request::Snapshot);
    state
        .discovered
        .insert("receiver".into(), PeerPoll::default());
    let lookup_generation = state.generation;
    state.invalidate_polling_engine();
    assert_ne!(state.generation, lookup_generation);
    assert!(state.queued.is_empty());
    assert!(!state.in_flight);
    assert!(!state.lookup_in_flight);
    assert!(state.snapshot.is_none());
    assert!(state.discovery_at.is_none());
    assert!(state.discovered.is_empty());
    assert!(
        state.discovery_running,
        "An obsolete fan-out must finish before another starts"
    );
    assert_eq!(state.target, "receiver");
    assert_eq!(state.role, Role::Remote);
}
