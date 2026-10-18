//! Tracking peer projection runs on the tracker owner, never on the UI thread.
use super::*;
use nova_tracking::peer::{
    TRACKING_DOMAIN, apply_peer_records, connection_record, peer_connections, peer_records,
};
/// Automatic refresh may replace only the exact shared credential it consumed.
/// A paired Disconnect or newer rotation can merge while HTTP is in flight.
/// Commit the rotated pair, its local copy and publication baseline together so
/// a later snapshot cannot restamp stale credentials over that newer record.
pub(super) struct RefreshStorage {
    storage: KvStorage,
    service: Service,
    domain: String,
    expected: std::sync::Mutex<Option<nova_sync::Record>>,
}
fn refresh_error() -> nova_storage::Error {
    nova_storage::Error::new(
        nova_storage::ErrorKind::Transaction,
        "tracking credential changed; retry shared state",
    )
}
fn shared_credential_key(service: Service) -> &'static str {
    match service {
        Service::MyAnimeList => "credentials:mal",
        Service::AniList => "credentials:anilist",
    }
}
fn consumes_current_pair(
    shared: &Option<nova_sync::Record>,
    local: Option<&str>,
    consumed: &str,
    previously_shared: bool,
) -> bool {
    !previously_shared
        && local == Some(consumed)
        && shared
            .as_ref()
            .is_none_or(|r| r.value.as_deref() == Some(consumed))
}
#[allow(clippy::too_many_arguments)]
fn stage_refresh(
    store: &mut nova_sync::Store,
    domain: &str,
    service: Service,
    expected: &Option<nova_sync::Record>,
    local_key: &str,
    raw: &str,
    device: u64,
) -> Result<nova_sync::Record, nova_storage::Error> {
    let key = shared_credential_key(service);
    if store.record(domain, key) != expected.as_ref()
        || expected.as_ref().is_some_and(nova_sync::Record::is_deleted)
        || nova_sync::parse_profile_domain(domain).is_some_and(|(id, _)| {
            store
                .record("profile_deletions", id)
                .is_some_and(|r| !r.is_deleted())
        })
    {
        return Err(refresh_error());
    }
    let baseline_key = format!("sync:baseline:{domain}");
    let mut baseline: std::collections::BTreeMap<String, String> = store
        .extra_value(&baseline_key)
        .map_err(|_| refresh_error())?
        .map(|s| serde_json::from_str(&s))
        .transpose()
        .map_err(|_| refresh_error())?
        .unwrap_or_default();
    if expected.is_none() && baseline.contains_key(key) {
        // A collected tombstone must not look like a first-run legacy seed.
        return Err(refresh_error());
    }
    baseline.insert(key.into(), raw.into());
    let baseline = serde_json::to_string(&baseline).map_err(|_| refresh_error())?;
    store.set(domain, key, Some(raw.into()), 0, device);
    store.queue_extra(local_key, Some(raw.into()));
    store.queue_extra(&baseline_key, Some(baseline));
    store.record(domain, key).cloned().ok_or_else(refresh_error)
}
impl RefreshStorage {
    pub(super) fn capture(
        storage: KvStorage,
        consumed: &SavedConnection,
    ) -> Result<Self, CredentialError> {
        let service = consumed.account.service;
        let (_, consumed_raw, _) = connection_record(consumed)?;
        let owner = nova_sync::local_store().map_err(|_| CredentialError::Storage)?;
        let mut store = owner.lock().unwrap();
        store.save().map_err(|_| CredentialError::Storage)?;
        let domain = storage.profile_store().domain(TRACKING_DOMAIN);
        let expected = store
            .record(&domain, shared_credential_key(service))
            .cloned();
        let local = storage
            .read(nova_tracking::credentials::credential_key(service))
            .map_err(|_| CredentialError::Storage)?;
        let previously_shared = if expected.is_none() {
            store
                .extra_value(&format!("sync:baseline:{domain}"))
                .map_err(|_| CredentialError::Storage)?
                .map(|s| serde_json::from_str::<std::collections::BTreeMap<String, String>>(&s))
                .transpose()
                .map_err(|_| CredentialError::Invalid)?
                .is_some_and(|b| b.contains_key(shared_credential_key(service)))
        } else {
            false
        };
        if !consumes_current_pair(
            &expected,
            local.as_deref(),
            &consumed_raw,
            previously_shared,
        ) {
            return Err(CredentialError::Api(ApiError::Authentication));
        }
        Ok(Self {
            storage,
            service,
            domain,
            expected: std::sync::Mutex::new(expected),
        })
    }
    pub(super) fn current(&self) -> bool {
        let expected = self.expected.lock().unwrap();
        nova_sync::local_store().ok().is_some_and(|owner| {
            owner
                .lock()
                .unwrap()
                .record(&self.domain, shared_credential_key(self.service))
                == expected.as_ref()
        })
    }
}
impl StateStorage for RefreshStorage {
    fn read(&self, key: &str) -> Result<Option<String>, nova_storage::Error> {
        self.storage.read(key)
    }
    fn write(&self, entries: &[(String, Option<String>)]) -> Result<(), nova_storage::Error> {
        let [(key, Some(raw))] = entries else {
            return Err(refresh_error());
        };
        if key != nova_tracking::credentials::credential_key(self.service) {
            return Err(refresh_error());
        }
        let owner = nova_sync::local_store().map_err(|_| refresh_error())?;
        let device = nova_sync::local_device().map_err(|_| refresh_error())?;
        let mut expected = self.expected.lock().unwrap();
        let mut store = owner.lock().unwrap();
        let next = stage_refresh(
            &mut store,
            &self.domain,
            self.service,
            &expected,
            &self.storage.profile_store().key(key),
            raw,
            device,
        )?;
        *expected = Some(next);
        store.save().map_err(|_| refresh_error())?;
        drop(store);
        nova_sync::request_local_sync();
        Ok(())
    }
}
impl Coordinator {
    /// Receipts are immutable and grow-only. Publishing a local decision must
    /// never tombstone an opaque receipt received from a newer application.
    pub(super) fn publish_completion_shared(&self) -> Result<(), String> {
        let records = completion_peer_records(self.store.state()).map_err(|_| storage_message())?;
        let owner = nova_sync::local_store().map_err(|_| storage_message())?;
        let dev = nova_sync::local_device().map_err(|_| storage_message())?;
        let mut store = owner.lock().unwrap();
        let mut changed = false;
        for (key, value, timestamp) in records {
            if store
                .record(
                    &crate::app::sync::profile_domain(TRACKING_COMPLETION_DOMAIN),
                    &key,
                )
                .and_then(|r| r.value.as_ref())
                .is_none()
            {
                changed |= store.set(
                    &crate::app::sync::profile_domain(TRACKING_COMPLETION_DOMAIN),
                    &key,
                    Some(value),
                    timestamp,
                    dev,
                );
            }
        }
        store.save().map_err(|_| storage_message())?;
        drop(store);
        if changed && !crate::app::sync::applying() {
            nova_sync::request_local_sync();
        }
        Ok(())
    }
    pub(super) fn apply_completion_shared(&mut self, force: bool) -> Result<(), String> {
        let force = force || !self.completion_restored;
        let owner = nova_sync::local_store().map_err(|_| storage_message())?;
        let (records, basis) = {
            let mut store = owner.lock().unwrap();
            store.save().map_err(|_| storage_message())?;
            if !force
                && !store
                    .pending_domains()
                    .iter()
                    .any(|d| d == &crate::app::sync::profile_domain(TRACKING_COMPLETION_DOMAIN))
            {
                return Ok(());
            }
            let Some(basis) = store
                .digest()
                .get(&crate::app::sync::profile_domain(
                    TRACKING_COMPLETION_DOMAIN,
                ))
                .cloned()
            else {
                self.completion_restored = true;
                return Ok(());
            };
            (
                store.records(&crate::app::sync::profile_domain(
                    TRACKING_COMPLETION_DOMAIN,
                )),
                basis,
            )
        };
        let _guard = crate::app::sync::ApplyingGuard::new();
        let state = apply_completion_peer_records(self.store.state(), &records)
            .map_err(|_| storage_message())?;
        if state != *self.store.state() {
            self.store.save(state).map_err(|_| storage_message())?;
        }
        let mut store = owner.lock().unwrap();
        store
            .mark_projected(
                &crate::app::sync::profile_domain(TRACKING_COMPLETION_DOMAIN),
                &basis,
            )
            .map_err(|_| storage_message())?;
        store.save().map_err(|_| storage_message())?;
        self.completion_restored = true;
        Ok(())
    }
    fn shared_records(&self) -> Result<Vec<(String, String, u64)>, String> {
        let mut records = peer_records(self.store.state()).map_err(|_| storage_message())?;
        for service in [Service::MyAnimeList, Service::AniList] {
            if let Some(connection) =
                SavedConnection::load(&self.storage, service).map_err(|_| storage_message())?
                && self
                    .store
                    .state()
                    .active_accounts
                    .contains(&connection.account)
            {
                records.push(connection_record(&connection).map_err(|_| storage_message())?);
            }
        }
        records.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(records)
    }
    pub(super) fn publish_shared(&self, seed: bool) -> Result<(), String> {
        // A failed rotated-credential transaction remains staged in the sync
        // owner. Flush it before reading the local snapshot, never publish a
        // pre-rotation disk value against its newer in-memory baseline.
        nova_sync::local_store()
            .map_err(|_| storage_message())?
            .lock()
            .unwrap()
            .save()
            .map_err(|_| storage_message())?;
        if seed
            && self
                .storage
                .read(&format!(
                    "sync:baseline:{}",
                    crate::app::sync::profile_domain(TRACKING_DOMAIN)
                ))
                .map_err(|_| storage_message())?
                .is_some()
        {
            return Ok(());
        }
        let records = self.shared_records()?;
        nova_sync::commit_snapshot(
            &crate::app::sync::profile_domain(TRACKING_DOMAIN),
            &records,
            None,
            crate::app::sync::applying(),
            seed,
        )
        .map_err(|_| storage_message())
    }
    pub(super) fn apply_shared(&mut self, force: bool) -> Result<(), String> {
        // Retry any durable local changes before overlaying the merged records.
        self.publish_shared(false)?;
        let owner = nova_sync::local_store().map_err(|_| storage_message())?;
        let (records, basis, sequence, history) = {
            let mut store = owner.lock().unwrap();
            store.save().map_err(|_| storage_message())?;
            if !force
                && !store
                    .pending_domains()
                    .iter()
                    .any(|d| d == &crate::app::sync::profile_domain(TRACKING_DOMAIN))
            {
                return Ok(());
            }
            let Some(basis) = store
                .digest()
                .get(&crate::app::sync::profile_domain(TRACKING_DOMAIN))
                .cloned()
            else {
                return Ok(());
            };
            let sequence = store
                .extra_value(&crate::app::profiles::storage_key(EVENT_COUNTER_KEY))
                .map_err(|_| storage_message())?
                .map(|raw| serde_json::from_str::<u64>(&raw))
                .transpose()
                .map_err(|_| storage_message())?
                .unwrap_or(0);
            let history = store
                .extra_value(&crate::app::profiles::storage_key(EPISODE_PROGRESS_KEY))
                .map_err(|_| storage_message())?
                .map(|raw| serde_json::from_str::<HashMap<String, EpisodeProgress>>(&raw))
                .transpose()
                .map_err(|_| storage_message())?
                .unwrap_or_default();
            (
                store.records(&crate::app::sync::profile_domain(TRACKING_DOMAIN)),
                basis,
                sequence,
                history,
            )
        };
        let _guard = crate::app::sync::ApplyingGuard::new();
        let state = apply_peer_records(self.store.state(), &records, sequence, |episode| {
            seed_watched(&history, &episode.source.source_id, &episode.episode_id)
        })
        .map_err(|_| {
            text::tr("Shared tracking data needs alignment or a newer Nova version.").to_string()
        })?;
        let connections = peer_connections(&records).map_err(|_| storage_message())?;
        let values = [Service::MyAnimeList, Service::AniList].map(|service| {
            (
                service,
                connections
                    .iter()
                    .find(|(s, _)| *s == service)
                    .map(|(_, c)| c),
            )
        });
        let mut changed = vec![];
        for (service, connection) in &values {
            let old = self
                .storage
                .read(nova_tracking::credentials::credential_key(*service))
                .map_err(|_| storage_message())?;
            let new = connection
                .map(connection_record)
                .transpose()
                .map_err(|_| storage_message())?
                .map(|(_, raw, _)| raw);
            if old != new {
                changed.push(*service);
            }
        }
        self.store
            .save_with_connections(state, &values)
            .map_err(|_| storage_message())?;
        for service in changed {
            self.cancel(service);
            self.bridge.tracking().login_generation[service_index(service)]
                .fetch_add(1, Ordering::AcqRel);
            self.sessions.retain(|s| s.service != service);
            self.pending_restore
                .retain(|c| c.connection.account.service != service);
            self.restore_after[service_index(service)] = 0;
        }
        self.choice = None;
        self.setup = None;
        self.candidates.clear();
        let pauses = nova_tracking::peer::conflict_pauses(self.store.state(), &records)
            .map_err(|_| storage_message())?;
        let records = self.shared_records()?;
        let dev = nova_sync::local_device().map_err(|_| storage_message())?;
        let mut store = owner.lock().unwrap();
        nova_sync::prepare_snapshot(
            &mut store,
            dev,
            &crate::app::sync::profile_domain(TRACKING_DOMAIN),
            &records,
            None,
            true,
            false,
        )
        .map_err(|_| storage_message())?;
        // Coverage conflicts are an intentional safety reconciliation, rather
        // than an echo of remote metadata. Publish the pause so a later repair
        // can enable one link without resurrecting the other conflicting link.
        for (key, value, _) in pauses {
            let value = nova_sync::preserve_unknown(
                store
                    .record(&crate::app::sync::profile_domain(TRACKING_DOMAIN), &key)
                    .and_then(|r| r.value.as_deref()),
                &value,
            );
            store.set(
                &crate::app::sync::profile_domain(TRACKING_DOMAIN),
                &key,
                Some(value),
                0,
                dev,
            );
        }
        store
            .mark_projected(&crate::app::sync::profile_domain(TRACKING_DOMAIN), &basis)
            .map_err(|_| storage_message())?;
        // mark_projected deliberately leaves a newer concurrent merge pending.
        store.save().map_err(|_| storage_message())?;
        self.notice = text::tr("Tracking links and sign-in received from a paired device.").into();
        Ok(())
    }
}

fn seed_watched(
    history: &HashMap<String, EpisodeProgress>,
    series_id: &str,
    episode_id: &str,
) -> bool {
    history
        .get(&progress_map_key(series_id, episode_id))
        .is_some_and(|p| p.watched)
}

#[cfg(test)]
mod tests {
    use super::*;
    const DOMAIN: &str = "profile:alice:tracking";
    const LOCAL: &str = "profile:alice:tracking:credentials:mal:v1";
    #[test]
    fn refresh_capture_rejects_stale_consumed_pair_even_when_disk_and_shared_agree() {
        let mut store = refresh_store();
        let consumed = "old-pair";
        // A remote apply can update disk and shared state while the live
        // session (or a just-loaded connection) still holds the old pair.
        store.set(DOMAIN, "credentials:mal", Some("newer-pair".into()), 0, 2);
        store.queue_extra(LOCAL, Some("newer-pair".into()));
        let shared = store.record(DOMAIN, "credentials:mal").cloned();
        let local = store.extra_value(LOCAL).unwrap();
        assert!(!consumes_current_pair(
            &shared,
            local.as_deref(),
            consumed,
            false
        ));
        assert!(consumes_current_pair(
            &shared,
            local.as_deref(),
            "newer-pair",
            false
        ));
        assert!(!consumes_current_pair(
            &shared,
            Some(consumed),
            consumed,
            false
        ));
    }
    fn refresh_store() -> nova_sync::Store {
        let mut store = nova_sync::Store::default();
        store.set(DOMAIN, "credentials:mal", Some("old-pair".into()), 0, 1);
        store.queue_extra(LOCAL, Some("old-pair".into()));
        store.queue_extra(
            &format!("sync:baseline:{DOMAIN}"),
            Some(
                serde_json::json!({
                    "credentials:mal":"old-pair", "link:opaque":"untouched"
                })
                .to_string(),
            ),
        );
        store
    }
    #[test]
    fn refresh_stages_shared_pair_local_copy_and_only_its_baseline_together() {
        let mut store = refresh_store();
        let expected = store.record(DOMAIN, "credentials:mal").cloned();
        let next = stage_refresh(
            &mut store,
            DOMAIN,
            Service::MyAnimeList,
            &expected,
            LOCAL,
            "rotated-pair",
            1,
        )
        .unwrap();
        assert_eq!(next.value.as_deref(), Some("rotated-pair"));
        assert_eq!(
            store.extra_value(LOCAL).unwrap().as_deref(),
            Some("rotated-pair")
        );
        let baseline: serde_json::Value = serde_json::from_str(
            &store
                .extra_value(&format!("sync:baseline:{DOMAIN}"))
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(baseline["credentials:mal"], "rotated-pair");
        assert_eq!(baseline["link:opaque"], "untouched");
        // A later remote Disconnect must remain deleted even when the actor
        // publishes its unchanged rotated local snapshot.
        store.set(DOMAIN, "credentials:mal", None, 0, 2);
        nova_sync::prepare_snapshot(
            &mut store,
            1,
            DOMAIN,
            &[
                ("credentials:mal".into(), "rotated-pair".into(), 0),
                ("link:opaque".into(), "untouched".into(), 0),
            ],
            None,
            false,
            false,
        )
        .unwrap();
        assert!(
            store
                .record(DOMAIN, "credentials:mal")
                .unwrap()
                .is_deleted()
        );
    }
    #[test]
    fn refresh_and_failed_save_retry_cannot_replace_disconnect_or_newer_peer_pair() {
        for remote in [None, Some("newer-peer-pair")] {
            for failed_save_retry in [false, true] {
                let mut store = refresh_store();
                let mut expected = store.record(DOMAIN, "credentials:mal").cloned();
                if failed_save_retry {
                    // The caller retains this exact new basis even if saving
                    // its staged transaction failed. It must never recapture
                    // a later peer row as permission for the stale token.
                    expected = Some(
                        stage_refresh(
                            &mut store,
                            DOMAIN,
                            Service::MyAnimeList,
                            &expected,
                            LOCAL,
                            "pending-rotated-pair",
                            1,
                        )
                        .unwrap(),
                    );
                }
                store.set(DOMAIN, "credentials:mal", remote.map(Into::into), 0, 2);
                let before = store.record(DOMAIN, "credentials:mal").cloned();
                assert!(
                    stage_refresh(
                        &mut store,
                        DOMAIN,
                        Service::MyAnimeList,
                        &expected,
                        LOCAL,
                        "stale-pair",
                        1
                    )
                    .is_err()
                );
                assert_eq!(store.record(DOMAIN, "credentials:mal"), before.as_ref());
                if remote.is_none() {
                    // A tombstone present before capture is not a legacy
                    // absent record and cannot authorize an automatic save.
                    assert!(
                        stage_refresh(
                            &mut store,
                            DOMAIN,
                            Service::MyAnimeList,
                            &before,
                            LOCAL,
                            "stale-pair",
                            1
                        )
                        .is_err()
                    );
                }
            }
        }
    }
    #[test]
    fn legacy_absent_refresh_record_is_seedable_but_retired_profile_is_not() {
        let mut collected = nova_sync::Store::default();
        collected.queue_extra(
            &format!("sync:baseline:{DOMAIN}"),
            Some(r#"{"credentials:mal":"old-pair"}"#.into()),
        );
        assert!(
            stage_refresh(
                &mut collected,
                DOMAIN,
                Service::MyAnimeList,
                &None,
                LOCAL,
                "stale-pair",
                1
            )
            .is_err()
        );
        let mut store = nova_sync::Store::default();
        store.queue_extra(&format!("sync:baseline:{DOMAIN}"), Some("{}".into()));
        let expected = Some(
            stage_refresh(
                &mut store,
                DOMAIN,
                Service::MyAnimeList,
                &None,
                LOCAL,
                "legacy-pair",
                1,
            )
            .unwrap(),
        );
        store.set("profile_deletions", "alice", Some("true".into()), 0, 2);
        assert!(
            stage_refresh(
                &mut store,
                DOMAIN,
                Service::MyAnimeList,
                &expected,
                LOCAL,
                "late-pair",
                1
            )
            .is_err()
        );
        assert_eq!(store.record(DOMAIN, "credentials:mal"), expected.as_ref());
    }
    #[test]
    fn watched_seed_uses_series_scoped_progress_keys() {
        let history = HashMap::from([
            (
                progress_map_key("series-a", "episode-1"),
                EpisodeProgress {
                    watched: true,
                    ..Default::default()
                },
            ),
            (
                progress_map_key("series-b", "episode-1"),
                EpisodeProgress::default(),
            ),
        ]);
        assert!(seed_watched(&history, "series-a", "episode-1"));
        assert!(!seed_watched(&history, "series-b", "episode-1"));
        assert!(!seed_watched(&history, "missing", "episode-1"));
    }
}
