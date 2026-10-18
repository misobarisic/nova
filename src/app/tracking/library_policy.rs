//! Synced user choices are separate from device-local discovery and journal
//! checkpoints. Switching modes never rewrites individual title ownership.
use super::*;
pub(super) const DOMAIN: &str = "tracking_library";
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Policy {
    pub enabled: bool,
    pub include_history: bool,
    pub excluded: Vec<SourceRef>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Origin {
    Bulk,
    Explicit,
}
fn account_prefix(account: &nova_tracking::AccountKey) -> String {
    format!(
        "account:{}:",
        serde_json::to_string(account).expect("account identity")
    )
}
fn exclusion_prefix(account: &nova_tracking::AccountKey) -> String {
    format!(
        "excluded:{}:",
        serde_json::to_string(account).expect("account identity")
    )
}
fn exclusion_key(account: &nova_tracking::AccountKey, source: &SourceRef) -> String {
    format!(
        "{}{}",
        exclusion_prefix(account),
        serde_json::to_string(source).expect("source identity")
    )
}
fn origin_key(account: &nova_tracking::AccountKey, source: &SourceRef, target: u32) -> String {
    format!(
        "origin:{}",
        serde_json::to_string(&(account, source, target)).expect("target identity")
    )
}
fn failure() -> String {
    text::tr("Tracking event data needs recovery.").into()
}
fn domain() -> String {
    sync::profile_domain(DOMAIN)
}
fn decode<T: for<'de> Deserialize<'de>>(
    store: &nova_sync::Store,
    key: &str,
) -> Result<Option<T>, String> {
    store
        .record(&domain(), key)
        .and_then(|r| r.value.as_ref())
        .map(|raw| serde_json::from_str(raw).map_err(|_| failure()))
        .transpose()
}
fn read_from(
    store: &nova_sync::Store,
    account: &nova_tracking::AccountKey,
) -> Result<Policy, String> {
    let prefix = account_prefix(account);
    let excluded = exclusion_prefix(account);
    let rows = store.records_with_prefix(&domain(), &excluded);
    if rows.len() > 20_000 {
        return Err(failure());
    }
    let mut policy = Policy {
        enabled: decode(store, &format!("{prefix}enabled"))?.unwrap_or(false),
        include_history: decode(store, &format!("{prefix}history"))?.unwrap_or(false),
        excluded: Vec::new(),
    };
    for (key, raw) in rows {
        let value: bool = serde_json::from_str(&raw).map_err(|_| failure())?;
        let source: SourceRef =
            serde_json::from_str(key.strip_prefix(&excluded).ok_or_else(failure)?)
                .map_err(|_| failure())?;
        if source.source_id.is_empty()
            || source.source_id.len() > 4096
            || source.provider_id.is_empty()
            || source.provider_id.len() > 512
            || source.media_type.is_empty()
            || source.media_type.len() > 128
        {
            return Err(failure());
        }
        if value {
            policy.excluded.push(source);
        }
    }
    Ok(policy)
}
pub(super) fn read(account: &nova_tracking::AccountKey) -> Result<Policy, String> {
    let owner = nova_sync::local_store().map_err(|_| failure())?;
    read_from(&owner.lock().unwrap(), account)
}
fn mutate(
    action: impl FnOnce(&mut nova_sync::Store, u64) -> Result<(), String>,
) -> Result<(), String> {
    if !profiles::is_current(&profiles::capture()) {
        return Err(failure());
    }
    let owner = nova_sync::local_store().map_err(|_| failure())?;
    let mut store = owner.lock().unwrap();
    let device = nova_sync::local_device().map_err(|_| failure())?;
    action(&mut store, device)?;
    store.save().map_err(|_| failure())?;
    drop(store);
    nova_sync::request_local_sync();
    Ok(())
}
pub(super) fn write_mode(
    account: &nova_tracking::AccountKey,
    enabled: bool,
    include_history: bool,
) -> Result<(), String> {
    mutate(|store, device| {
        let p = account_prefix(account);
        store.set(
            &domain(),
            &format!("{p}enabled"),
            Some(enabled.to_string()),
            0,
            device,
        );
        store.set(
            &domain(),
            &format!("{p}history"),
            Some(include_history.to_string()),
            0,
            device,
        );
        Ok(())
    })
}
pub(super) fn write_exclusion(
    account: &nova_tracking::AccountKey,
    source: &SourceRef,
    excluded: bool,
) -> Result<(), String> {
    mutate(|store, device| {
        store.set(
            &domain(),
            &exclusion_key(account, source),
            excluded.then(|| "true".into()),
            0,
            device,
        );
        Ok(())
    })
}
pub(super) fn origin(
    account: &nova_tracking::AccountKey,
    source: &SourceRef,
    target: u32,
) -> Result<Option<Origin>, String> {
    let owner = nova_sync::local_store().map_err(|_| failure())?;
    decode(&owner.lock().unwrap(), &origin_key(account, source, target))
}
pub(super) fn write_origin(
    account: &nova_tracking::AccountKey,
    source: &SourceRef,
    target: u32,
    origin: Origin,
) -> Result<(), String> {
    mutate(|store, device| {
        let key = origin_key(account, source, target);
        // Background discovery cannot downgrade existing individual consent.
        if origin == Origin::Bulk && decode::<Origin>(store, &key)? == Some(Origin::Explicit) {
            return Ok(());
        }
        store.set(
            &domain(),
            &key,
            Some(serde_json::to_string(&origin).map_err(|_| failure())?),
            0,
            device,
        );
        Ok(())
    })
}
pub(super) fn clear_origin(
    account: &nova_tracking::AccountKey,
    source: &SourceRef,
    target: u32,
) -> Result<(), String> {
    mutate(|store, device| {
        store.set(
            &domain(),
            &origin_key(account, source, target),
            None,
            0,
            device,
        );
        Ok(())
    })
}
pub(super) fn acknowledge() -> Result<(), String> {
    let owner = nova_sync::local_store().map_err(|_| failure())?;
    let mut store = owner.lock().unwrap();
    let domain = domain();
    if let Some(basis) = store.digest().get(&domain).cloned() {
        store
            .mark_projected(&domain, &basis)
            .map_err(|_| failure())?;
        store.save().map_err(|_| failure())?;
    }
    Ok(())
}
pub(super) fn clear_all() -> Result<(), String> {
    mutate(|store, device| {
        let domain = domain();
        for (key, _) in store.records(&domain) {
            store.set(&domain, &key, None, 0, device);
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn account() -> nova_tracking::AccountKey {
        nova_tracking::AccountKey {
            service: Service::AniList,
            remote_user_id: 1.try_into().unwrap(),
        }
    }
    fn source(id: &str) -> SourceRef {
        SourceRef {
            provider_id: "nova".into(),
            source_id: id.into(),
            media_type: "series".into(),
        }
    }
    #[test]
    fn mode_switches_preserve_individual_choices_and_exclusions() {
        let a = account();
        let source = source("a");
        let mut store = nova_sync::Store::default();
        let origin = origin_key(&a, &source, 42);
        store.set(&domain(), &origin, Some("\"explicit\"".into()), 0, 1);
        store.set(
            &domain(),
            &exclusion_key(&a, &self::source("b")),
            Some("true".into()),
            0,
            1,
        );
        for enabled in [true, false] {
            store.set(
                &domain(),
                &format!("{}enabled", account_prefix(&a)),
                Some(enabled.to_string()),
                0,
                1,
            );
            let p = read_from(&store, &a).unwrap();
            assert_eq!(p.enabled, enabled);
            assert_eq!(p.excluded, vec![self::source("b")]);
            assert_eq!(
                decode::<Origin>(&store, &origin).unwrap(),
                Some(Origin::Explicit)
            );
        }
    }
    #[test]
    fn account_policies_do_not_bleed_and_exclusion_tombstones_stay_removed() {
        let a = account();
        let mut other = a.clone();
        other.remote_user_id = 2.try_into().unwrap();
        let mut store = nova_sync::Store::default();
        let key = exclusion_key(&a, &source("a"));
        store.set(&domain(), &key, Some("true".into()), 0, 1);
        assert!(read_from(&store, &other).unwrap().excluded.is_empty());
        store.set(&domain(), &key, None, 0, 1);
        assert!(read_from(&store, &a).unwrap().excluded.is_empty());
    }
    #[test]
    fn malformed_remote_policy_is_an_error_instead_of_broad_consent() {
        let a = account();
        let mut store = nova_sync::Store::default();
        store.set(
            &domain(),
            &format!("{}enabled", account_prefix(&a)),
            Some("\"true\"".into()),
            0,
            1,
        );
        assert!(read_from(&store, &a).is_err());
    }
}
