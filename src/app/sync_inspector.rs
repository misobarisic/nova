//! Value-free sync inventory and explanations from the existing progress registers.
//! Reading diagnostics never writes records, advances clocks or initiates a sync.
use super::*;
use crate::{SettingsBridge, SyncInspectorRow};

const EPISODE_LIMIT: usize = 80;

fn row(title: &str, detail: String, status: &str) -> SyncInspectorRow {
    SyncInspectorRow {
        title: title.into(),
        detail: detail.into(),
        status: text::tr(status).into(),
    }
}

fn domain_title(domain: &str) -> &'static str {
    match domain {
        "library" => "Library",
        "progress" => "Watch history and progress",
        "settings" => "Settings",
        "category" => "Categories",
        "addons" | "packages" | "addon_packages" | "plugin_settings" => "Addons",
        "peers" | "presence" => "Devices",
        "profiles" => "Profiles",
        "playback_preferences" => "Playback preferences",
        "last_stream" => "Last stream choices",
        _ => "Other account data",
    }
}

fn safe_label(label: &str) -> Option<String> {
    let label = label.trim();
    // Titles are useful; transport addresses and arbitrary record keys are not.
    if label.is_empty() || label.contains("://") || label.contains('\n') {
        None
    } else {
        Some(label.chars().take(180).collect())
    }
}

fn clock(label: &str, version: nova_sync::Version) -> String {
    text::sync_inspector_clock(text::tr(label), version.ts, version.counter, version.dev)
}

fn episode_detail(inspection: &nova_sync::ProgressInspection) -> String {
    let reason = if inspection.watched {
        "The watch action is newer than the unwatch action. Playback activity alone does not clear watched."
    } else if inspection.unwatch_action.is_some() {
        "The unwatch action wins over the watch action. Equal action clocks favor unwatch."
    } else {
        "No watch action is recorded. Playback activity alone does not mark this episode watched."
    };
    let mut lines = vec![
        text::tr(reason).to_string(),
        clock("Latest activity", inspection.activity),
    ];
    if let Some(version) = inspection.watch_action {
        lines.push(clock("Watch action", version));
    }
    if let Some(version) = inspection.unwatch_action {
        lines.push(clock("Unwatch action", version));
    }
    lines.join("\n")
}

impl Bridge {
    pub(super) fn setup_sync_inspector(&self) {
        let Some(app) = self.app() else { return };
        let b = self.clone();
        app.global::<SettingsBridge>()
            .on_sync_inspector_refresh(move || b.refresh_sync_inspector());
        let b = self.clone();
        app.global::<SettingsBridge>()
            .on_sync_inspector_query_changed(move |query| {
                if let Some(app) = b.app() {
                    app.global::<SettingsBridge>()
                        .set_sync_inspector_query(query);
                }
                b.refresh_sync_inspector();
            });
        self.refresh_sync_inspector();
    }

    pub(super) fn refresh_sync_inspector(&self) {
        let Some(app) = self.app() else { return };
        let global = app.global::<SettingsBridge>();
        if !global.get_sync_inspector_active() || global.get_sel() != 7 {
            return;
        }
        let query = global.get_sync_inspector_query().to_lowercase();
        let Ok(owner) = nova_sync::local_store() else {
            global.set_sync_inspector_rows(
                Rc::new(VecModel::from(vec![row(
                    text::tr("Local sync records unavailable"),
                    text::tr("Refresh after local storage becomes available.").into(),
                    "Unavailable",
                )]))
                .into(),
            );
            global.set_sync_inspector_episodes(
                Rc::new(VecModel::from(Vec::<SyncInspectorRow>::new())).into(),
            );
            return;
        };
        let profile = profiles::current_id();
        let progress_domain = nova_sync::profile_domain(&profile, "progress");
        let (local, mut identities) = {
            let state = self.shared.lock().unwrap();
            let identities = state
                .entries
                .iter()
                .filter(|entry| entry.type_ != "movie")
                .map(|entry| (entry.type_.clone(), entry.id.clone()))
                .collect::<HashSet<_>>();
            (state.progress.clone(), identities)
        };
        if let Some(item) = self.shared.lock().unwrap().modal_item.as_ref()
            && item.type_ != "movie"
        {
            identities.insert((item.type_.clone(), item.id.clone()));
        }
        let alias_groups = identities
            .into_iter()
            .flat_map(|(kind, series)| {
                let videos = read_episodes_cache_for(&kind, &series).unwrap_or_default();
                sync::episode_progress_alias_groups(&videos)
                    .into_iter()
                    .map(move |(native, aliases)| {
                        (progress_map_key(&series, &native), series.clone(), aliases)
                    })
            })
            .collect::<Vec<_>>();
        let store = owner.lock().unwrap_or_else(|e| e.into_inner());
        let digest = store.digest();
        let pending = store.pending_domains();
        let mut rows = Vec::new();
        let mut inventory = std::collections::BTreeMap::<&str, (usize, usize, bool)>::new();
        for (domain, versions) in &digest {
            let base = match nova_sync::parse_profile_domain(domain) {
                Some((owner, base)) if owner == profile => base,
                Some(_) => continue,
                None => domain,
            };
            let deleted = versions.values().filter(|version| version.deleted).count();
            let entry = inventory.entry(domain_title(base)).or_default();
            entry.0 += versions.len() - deleted;
            entry.1 += deleted;
            entry.2 |= pending.contains(domain);
        }
        for (title, (live, deleted, pending)) in inventory {
            rows.push(row(
                text::tr(title),
                text::sync_inspector_inventory(live, deleted, pending),
                "Stored on this device",
            ));
        }
        let mut episodes = Vec::new();
        for (key, _) in store.records(&progress_domain) {
            let Some(record) = store.record(&progress_domain, &key) else {
                continue;
            };
            let mut resolved = record.clone();
            let mut includes_aliases = false;
            if let Some((_, series, aliases)) =
                alias_groups.iter().find(|(native, _, _)| native == &key)
            {
                let keys = aliases
                    .iter()
                    .filter_map(|alias| {
                        let key = progress_map_key(series, alias);
                        let candidate = store.record(&progress_domain, &key)?;
                        let progress: EpisodeProgress =
                            serde_json::from_str(candidate.value.as_deref()?).ok()?;
                        (progress.series_id == *series && progress.episode_id == *alias)
                            .then_some(key)
                    })
                    .collect::<Vec<_>>();
                if keys.len() > 1
                    && let Some(value) = store
                        .resolved_progress_in(&progress_domain, keys.iter().map(String::as_str))
                {
                    for alias in &keys {
                        if let Some(candidate) = store.record(&progress_domain, alias)
                            && candidate.version.newer_than(&resolved.version)
                        {
                            resolved.version = candidate.version;
                        }
                    }
                    resolved.value = Some(value);
                    includes_aliases = true;
                }
            }
            let Some(inspection) = nova_sync::inspect_progress(&resolved) else {
                continue;
            };
            let progress = record
                .value
                .as_ref()
                .and_then(|value| serde_json::from_str::<EpisodeProgress>(value).ok());
            let title = progress
                .as_ref()
                .and_then(|progress| progress.history.as_ref())
                .and_then(|history| {
                    safe_label(&history.title).map(|title| {
                        safe_label(&history.episode_label)
                            .map(|episode| format!("{title} · {episode}"))
                            .unwrap_or(title)
                    })
                })
                .unwrap_or_else(|| text::tr("Episode with no saved title").into());
            if !query.is_empty() && !title.to_lowercase().contains(&query) {
                continue;
            }
            let mut detail = episode_detail(&inspection);
            if includes_aliases {
                detail.push('\n');
                detail.push_str(text::tr(
                    "Includes action registers from confirmed episode aliases.",
                ));
            }
            if local
                .get(&key)
                .is_none_or(|progress| progress.watched != inspection.watched)
            {
                detail.push('\n');
                detail.push_str(text::tr("The local view differs from this stored sync state; application may still be pending."));
            }
            episodes.push((
                (
                    resolved.version.ts,
                    resolved.version.counter,
                    resolved.version.dev,
                ),
                key,
                row(
                    &title,
                    detail,
                    if inspection.watched {
                        "Watched"
                    } else {
                        "Unwatched"
                    },
                ),
            ));
        }
        episodes.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        episodes.truncate(EPISODE_LIMIT);
        drop(store);
        if let Some(engine) = nova_sync::engine() {
            let names = engine.peer_names();
            for (index, peer) in engine.peers().iter().enumerate() {
                let title = names
                    .get(peer)
                    .and_then(|name| safe_label(name))
                    .unwrap_or_else(|| text::sync_inspector_count(text::tr("Device"), index + 1));
                let (detail, status) = match engine.peer_exchange_clock(peer) {
                    Some(at) => {
                        let newer = digest.values().flat_map(|records| records.values())
                            .filter(|version| version.hlc().newer_than(at)).count();
                        (text::sync_inspector_exchange(&format!("{}:{}", at.physical_ms, at.counter), newer), "Exchange recorded")
                    }
                    None => (text::tr("No completed exchange snapshot is recorded for this device. Delivery is unknown.").into(), "Delivery unknown"),
                };
                rows.push(row(&title, detail, status));
            }
        }
        global.set_sync_inspector_rows(Rc::new(VecModel::from(rows)).into());
        global.set_sync_inspector_episodes(
            Rc::new(VecModel::from(
                episodes
                    .into_iter()
                    .map(|(_, _, row)| row)
                    .collect::<Vec<_>>(),
            ))
            .into(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspector_never_uses_transport_addresses_as_display_labels() {
        assert!(safe_label("https://host/path?token=secret").is_none());
        assert!(safe_label("line\nsecret").is_none());
        assert_eq!(
            safe_label("  Living room  ").as_deref(),
            Some("Living room")
        );
        assert_eq!(domain_title("https://host/secret"), "Other account data");
    }
}
