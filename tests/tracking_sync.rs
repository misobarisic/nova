//! Exercise tracking records through the real generic sync merge, then commit
//! the received links and shared credentials into the tracker storage together.
use nova_tracking::{
    auth::{ClientRegistration, Tokens},
    credentials::{SavedConnection, credential_key},
    peer::*,
    *,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
#[derive(Clone, Default)]
struct Memory(Arc<Mutex<HashMap<String, String>>>);
impl StateStorage for Memory {
    fn read(&self, key: &str) -> Result<Option<String>, nova_storage::Error> {
        Ok(self.0.lock().unwrap().get(key).cloned())
    }
    fn write(&self, rows: &[(String, Option<String>)]) -> Result<(), nova_storage::Error> {
        let mut stored = self.0.lock().unwrap();
        for (key, value) in rows {
            if let Some(value) = value {
                stored.insert(key.clone(), value.clone());
            } else {
                stored.remove(key);
            }
        }
        Ok(())
    }
}
#[derive(Clone)]
struct Viewer;
impl api::Transport for Viewer {
    fn send(&self, _: api::Request) -> Result<api::Response, ApiError> {
        Ok(api::Response {
            status: 200,
            retry_after: None,
            reset_at: None,
            body: serde_json::json!({"id":7,"name":"fixture"}),
        })
    }
}
fn transfer(a: &nova_sync::Store, b: &mut nova_sync::Store) {
    for outbound in a.outbound(&b.digest()) {
        b.apply(&outbound.domain, &outbound.key, outbound.record);
    }
}
fn connection(token: &str) -> SavedConnection {
    SavedConnection {
        account: AccountKey {
            service: Service::MyAnimeList,
            remote_user_id: 7.try_into().unwrap(),
        },
        registration: ClientRegistration {
            client_id: "fixture".into(),
            redirect_uri: "http://127.0.0.1:53926/callback".into(),
        },
        tokens: Tokens {
            access: Secret::new(token.into()).unwrap(),
            refresh: Some(Secret::new(format!("{token}-refresh")).unwrap()),
            expires_at: Some(10000),
        },
    }
}
fn receive(mesh: &nova_sync::Store, tracker: &mut Store<Memory>, sequence: u64) {
    let records = mesh.records(TRACKING_DOMAIN);
    let state = apply_peer_records(tracker.state(), &records, sequence, |_| true).unwrap();
    let connections = peer_connections(&records).unwrap();
    let entries = [Service::MyAnimeList, Service::AniList].map(|s| {
        (
            s,
            connections
                .iter()
                .find(|(service, _)| *service == s)
                .map(|(_, c)| c),
        )
    });
    tracker.save_with_connections(state, &entries).unwrap();
}
#[test]
fn paired_tracking_shares_links_tokens_rotation_and_disconnect_without_history_replay() {
    let signed_in = connection("fixture-access");
    let key = TargetKey {
        account: signed_in.account.clone(),
        media_kind: MediaKind::Anime,
        remote_media_id: 12.try_into().unwrap(),
    };
    let source = SourceRef {
        provider_id: "nova".into(),
        source_id: "merged-title".into(),
        media_type: "series".into(),
    };
    let state = TrackingState {
        accounts: vec![Account {
            key: signed_in.account.clone(),
            generation: std::num::NonZeroU64::MIN,
            display_name: "fixture".into(),
        }],
        active_accounts: vec![signed_in.account.clone()],
        targets: vec![Target {
            key: key.clone(),
            remote_entry_id: None,
            final_episode_total: Some(12.try_into().unwrap()),
            release_finished: true,
        }],
        bindings: vec![Binding {
            id: "desktop-binding".into(),
            source: source.clone(),
            target: key,
            account_generation: std::num::NonZeroU64::MIN,
            mapping_revision: std::num::NonZeroU64::MIN,
            enabled: true,
            bulk_origin: false,
            assignments: vec![Assignment {
                episode_id: "stable-episode".into(),
                target_episode: 1.try_into().unwrap(),
            }],
        }],
        automatic_paused: vec![Service::AniList],
        ..Default::default()
    };
    let mut a = nova_sync::Store::default();
    let mut b = nova_sync::Store::default();
    for (key, value, _) in peer_records(&state)
        .unwrap()
        .into_iter()
        .chain([connection_record(&signed_in).unwrap()])
    {
        a.set(TRACKING_DOMAIN, &key, Some(value), 0, 1);
    }
    transfer(&a, &mut b);
    let memory = Memory::default();
    let mut tracker = Store::load(memory.clone()).unwrap();
    receive(&b, &mut tracker, 42);
    assert_eq!(tracker.state().active_accounts, state.active_accounts);
    assert_eq!(
        tracker.state().bindings[0].assignments,
        state.bindings[0].assignments
    );
    assert_eq!(tracker.state().link_checkpoints[0].sequence, 42);
    assert!(tracker.state().outbox.pending().is_empty());
    let mut credentials = SavedConnection::load(&memory, Service::MyAnimeList)
        .unwrap()
        .unwrap();
    assert_eq!(
        credentials
            .restore(&memory, Viewer, 100)
            .unwrap()
            .viewer()
            .unwrap()
            .account,
        signed_in.account
    );
    // A refresh on either device propagates the full rotated token pair.
    let rotated = connection("rotated-fixture");
    let (record_key, value, _) = connection_record(&rotated).unwrap();
    a.set(TRACKING_DOMAIN, &record_key, Some(value), 0, 1);
    transfer(&a, &mut b);
    receive(&b, &mut tracker, 50);
    let restored = SavedConnection::load(&memory, Service::MyAnimeList)
        .unwrap()
        .unwrap();
    assert_eq!(
        connection_record(&restored).unwrap(),
        connection_record(&rotated).unwrap()
    );
    assert_eq!(
        tracker.state().link_checkpoints[0].sequence,
        42,
        "token refresh must not rebase unchanged coverage"
    );
    // Disconnect is a credential tombstone. Links/history remain available.
    b.set(TRACKING_DOMAIN, &record_key, None, 0, 2);
    transfer(&b, &mut a);
    receive(&a, &mut tracker, 51);
    assert!(tracker.state().active_accounts.is_empty());
    assert_eq!(tracker.state().bindings.len(), 1);
    assert!(
        memory
            .read(credential_key(Service::MyAnimeList))
            .unwrap()
            .is_none()
    );
    let link_key = a
        .records(TRACKING_DOMAIN)
        .into_iter()
        .find(|(key, _)| key.starts_with("link:"))
        .unwrap()
        .0;
    a.set(TRACKING_DOMAIN, &link_key, None, 0, 1);
    transfer(&a, &mut b);
    receive(&b, &mut tracker, 52);
    assert!(tracker.state().bindings.is_empty());
    assert!(tracker.state().link_checkpoints.is_empty());
    assert!(tracker.state().outbox.pending().is_empty());
    transfer(&b, &mut a);
    assert_eq!(a.digest(), b.digest());
}

#[test]
fn every_profile_receives_both_services_client_registration_and_verified_saved_tokens() {
    #[derive(Clone)]
    struct Verify;
    impl api::Transport for Verify {
        fn send(&self, request: api::Request) -> Result<api::Response, ApiError> {
            let body = if request.url == "https://graphql.anilist.co" {
                serde_json::json!({"data":{"Viewer":{"id":7,"name":"fixture","mediaListOptions":{"scoreFormat":"POINT_10"}}}})
            } else {
                assert_eq!(request.url, "https://api.myanimelist.net/v2/users/@me");
                serde_json::json!({"id":7,"name":"fixture"})
            };
            Ok(api::Response {
                status: 200,
                retry_after: None,
                reset_at: None,
                body,
            })
        }
    }
    let mut sender = nova_sync::Store::default();
    let mut receiver = nova_sync::Store::default();
    for profile in ["alice", "bob"] {
        for service in [Service::MyAnimeList, Service::AniList] {
            let mut connection = connection(&format!("fixture-{profile}-{service:?}"));
            connection.account.service = service;
            connection.registration = ClientRegistration {
                client_id: format!("custom_{profile}"),
                redirect_uri: "http://127.0.0.1:53940/shared-callback".into(),
            };
            let (key, raw, _) = connection_record(&connection).unwrap();
            // Only completed credentials and the registration necessary to
            // use them travel; transient authorization sessions do not.
            let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
            for ephemeral in [
                "code",
                "state",
                "verifier",
                "code_verifier",
                "nonce",
                "authorization_url",
            ] {
                assert!(json.get(ephemeral).is_none());
            }
            sender.set(
                &nova_sync::profile_domain(profile, TRACKING_DOMAIN),
                &key,
                Some(raw),
                0,
                1,
            );
        }
    }
    transfer(&sender, &mut receiver);
    for profile in ["alice", "bob"] {
        let connections = peer_connections(
            &receiver.records(&nova_sync::profile_domain(profile, TRACKING_DOMAIN)),
        )
        .unwrap();
        assert_eq!(connections.len(), 2);
        for (service, mut saved) in connections {
            assert_eq!(saved.registration.client_id, format!("custom_{profile}"));
            assert_eq!(
                saved.registration.redirect_uri,
                "http://127.0.0.1:53940/shared-callback"
            );
            let (_, raw, _) = connection_record(&saved).unwrap();
            let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
            assert_eq!(json["access"], format!("fixture-{profile}-{service:?}"));
            assert_eq!(
                json["refresh"],
                format!("fixture-{profile}-{service:?}-refresh")
            );
            let storage = Memory::default();
            let client = saved.restore(&storage, Verify, 100).unwrap();
            assert_eq!(client.viewer().unwrap().account, saved.account);
        }
    }
}
