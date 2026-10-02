//! Tracking peer projection runs on the tracker owner, never on the UI thread.
use super::*;
use nova_tracking::peer::{
    TRACKING_DOMAIN, apply_peer_records, connection_record, peer_connections, peer_records,
};
impl Coordinator {
    fn shared_records(&self) -> Result<Vec<(String, String, u64)>, String> {
        let mut records = peer_records(self.store.state()).map_err(|_| storage_message())?;
        for service in [Service::MyAnimeList, Service::AniList] {
            if let Some(connection) =
                SavedConnection::load(&KvStorage, service).map_err(|_| storage_message())?
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
        if seed
            && KvStorage
                .read("sync:baseline:tracking")
                .map_err(|_| storage_message())?
                .is_some()
        {
            return Ok(());
        }
        let records = self.shared_records()?;
        nova_sync::commit_snapshot(
            TRACKING_DOMAIN,
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
            if !force && !store.pending_domains().iter().any(|d| d == TRACKING_DOMAIN) {
                return Ok(());
            }
            let Some(basis) = store.digest().get(TRACKING_DOMAIN).cloned() else {
                return Ok(());
            };
            let sequence = store
                .extra_value(EVENT_COUNTER_KEY)
                .map_err(|_| storage_message())?
                .map(|raw| serde_json::from_str::<u64>(&raw))
                .transpose()
                .map_err(|_| storage_message())?
                .unwrap_or(0);
            let history = store
                .extra_value(EPISODE_PROGRESS_KEY)
                .map_err(|_| storage_message())?
                .map(|raw| serde_json::from_str::<HashMap<String, EpisodeProgress>>(&raw))
                .transpose()
                .map_err(|_| storage_message())?
                .unwrap_or_default();
            (store.records(TRACKING_DOMAIN), basis, sequence, history)
        };
        let _guard = crate::app::sync::ApplyingGuard::new();
        let state = apply_peer_records(self.store.state(), &records, sequence, |episode| {
            history.get(&episode.episode_id).is_some_and(|p| p.watched)
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
            let old = KvStorage
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
            self.bridge.tracking.login_generation[service_index(service)]
                .fetch_add(1, Ordering::AcqRel);
            self.sessions.retain(|s| s.service != service);
            self.pending_restore
                .retain(|c| c.account.service != service);
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
            TRACKING_DOMAIN,
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
                    .record(TRACKING_DOMAIN, &key)
                    .and_then(|r| r.value.as_deref()),
                &value,
            );
            store.set(TRACKING_DOMAIN, &key, Some(value), 0, dev);
        }
        store
            .mark_projected(TRACKING_DOMAIN, &basis)
            .map_err(|_| storage_message())?;
        // mark_projected deliberately leaves a newer concurrent merge pending.
        store.save().map_err(|_| storage_message())?;
        self.notice = text::tr("Tracking links and sign-in received from a paired device.").into();
        Ok(())
    }
}
