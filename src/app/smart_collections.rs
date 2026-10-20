//! Editable Library rules are profile settings, so their existing field-level
//! sync and per-device override policy also owns this feature. Membership is
//! computed from current library facts, never persisted or fetched from network.
use super::*;
use nova_config::{SmartCollection, SmartMediaType, SmartWatchState};
use nova_ui_shared::{SmartCollectionRow, SmartCollectionsBridge};

const MAX_COLLECTIONS: usize = 32;

impl Bridge {
    /// Header hydration can happen on workers and can improve runtime alone.
    /// Coalesce those completions before touching any Slint/model state.
    pub(super) fn smart_collection_metadata_changed(&self) {
        static PENDING: AtomicBool = AtomicBool::new(false);
        if PENDING.swap(true, Ordering::AcqRel) {
            return;
        }
        let bridge = self.clone();
        let context = profiles::capture();
        if profiles::invoke_account(move || {
            PENDING.store(false, Ordering::Release);
            if profiles::is_current(&context) && bridge.selected_smart_collection().is_some() {
                bridge.update_library_badges();
            }
        })
        .is_err()
        {
            PENDING.store(false, Ordering::Release);
        }
    }

    pub(super) fn setup_smart_collections(&self) {
        let Some(app) = self.app() else { return };
        let collections = app.global::<SmartCollectionsBridge>();
        let bridge = self.clone();
        collections.on_selected(move |id| {
            let Some(app) = bridge.app() else { return };
            let valid = id.is_empty()
                || bridge
                    .shared
                    .lock()
                    .unwrap()
                    .cache_settings
                    .smart_collections
                    .iter()
                    .any(|rule| rule.id == id.as_str());
            if valid {
                app.global::<SmartCollectionsBridge>().set_selected_id(id);
                bridge.refresh_smart_collections();
                bridge.library_view_changed();
            }
        });
        let bridge = self.clone();
        collections.on_edit(move |id| bridge.edit_smart_collection(&id));
        let bridge = self.clone();
        collections.on_save(move || bridge.save_smart_collection());
        let bridge = self.clone();
        collections.on_remove(move |id| bridge.remove_smart_collection(&id));
        self.refresh_smart_collections();
    }

    pub(super) fn refresh_smart_collections(&self) {
        let Some(app) = self.app() else { return };
        let collections = app.global::<SmartCollectionsBridge>();
        let profile = profiles::current_id();
        if collections.get_profile_id().as_str() != profile {
            collections.set_profile_id(profile.into());
            collections.set_selected_id(SharedString::default());
            collections.set_editor_open(false);
            collections.set_editing_id(SharedString::default());
            collections.set_name(SharedString::default());
            collections.set_genre(SharedString::default());
            collections.set_max_runtime(SharedString::default());
            collections.set_media_type(0);
            collections.set_watch_state(0);
            collections.set_error(SharedString::default());
        }
        let rules = self
            .shared
            .lock()
            .unwrap()
            .cache_settings
            .smart_collections
            .clone();
        let selected = collections.get_selected_id();
        let selected_rule = rules.iter().find(|rule| rule.id == selected.as_str());
        collections.set_selected_name(
            selected_rule
                .map(|rule| rule.name.clone())
                .unwrap_or_default()
                .into(),
        );
        if selected_rule.is_none() {
            collections.set_selected_id(SharedString::default());
        }
        collections.set_rows(
            Rc::new(VecModel::from(
                rules
                    .into_iter()
                    .take(MAX_COLLECTIONS)
                    .map(|rule| SmartCollectionRow {
                        id: rule.id.into(),
                        name: rule.name.into(),
                    })
                    .collect::<Vec<_>>(),
            ))
            .into(),
        );
    }

    pub(super) fn selected_smart_collection(&self) -> Option<SmartCollection> {
        let app = self.app()?;
        let selected = app.global::<SmartCollectionsBridge>().get_selected_id();
        if selected.is_empty() {
            return None;
        }
        self.shared
            .lock()
            .unwrap()
            .cache_settings
            .smart_collections
            .iter()
            .find(|rule| rule.id == selected.as_str())
            .cloned()
    }

    fn edit_smart_collection(&self, id: &str) {
        let Some(app) = self.app() else { return };
        let collections = app.global::<SmartCollectionsBridge>();
        let rule = if id.is_empty() {
            SmartCollection::default()
        } else {
            let state = self.shared.lock().unwrap();
            let Some(rule) = state
                .cache_settings
                .smart_collections
                .iter()
                .find(|rule| rule.id == id)
            else {
                return;
            };
            rule.clone()
        };
        collections.set_profile_id(profiles::current_id().into());
        collections.set_editing_id(rule.id.into());
        collections.set_name(rule.name.into());
        collections.set_media_type(rule.media_type.index());
        collections.set_watch_state(rule.watch_state.index());
        collections.set_genre(rule.genre.into());
        collections.set_max_runtime(
            rule.max_runtime_minutes
                .map(|minutes| minutes.to_string())
                .unwrap_or_default()
                .into(),
        );
        collections.set_error(SharedString::default());
        collections.set_editor_open(true);
    }

    fn save_smart_collection(&self) {
        let Some(app) = self.app() else { return };
        let collections = app.global::<SmartCollectionsBridge>();
        if !collections.get_editor_open()
            || collections.get_profile_id().as_str() != profiles::current_id()
            || !writable_key("settings")
        {
            return;
        }
        let raw_minutes = collections.get_max_runtime();
        let max_runtime_minutes = if raw_minutes.trim().is_empty() {
            None
        } else {
            match raw_minutes.trim().parse::<u32>() {
                Ok(minutes) if (1..=1440).contains(&minutes) => Some(minutes),
                _ => {
                    collections.set_error(
                        text::tr("Enter a runtime from 1 to 1440 minutes, or leave it empty.")
                            .into(),
                    );
                    return;
                }
            }
        };
        let editing = collections.get_editing_id().to_string();
        let rule = SmartCollection {
            id: if editing.is_empty() {
                collection_id()
            } else {
                editing.clone()
            },
            name: collections.get_name().trim().to_owned(),
            media_type: SmartMediaType::from_index(collections.get_media_type()),
            watch_state: SmartWatchState::from_index(collections.get_watch_state()),
            genre: collections.get_genre().trim().to_owned(),
            max_runtime_minutes,
        };
        if !rule.valid() {
            collections.set_error(text::tr("Choose a collection name of up to 80 characters and a genre of up to 80 characters.").into());
            return;
        }
        let mut state = self.shared.lock().unwrap();
        let rules = &mut state.cache_settings.smart_collections;
        if !editing.is_empty() {
            let Some(existing) = rules.iter_mut().find(|saved| saved.id == editing) else {
                collections.set_error(text::tr("This collection was removed. Close the editor and create a new collection.").into());
                return;
            };
            *existing = rule.clone();
        } else if rules.len() >= MAX_COLLECTIONS {
            collections.set_error(text::tr("You can save up to 32 smart collections.").into());
            return;
        } else {
            rules.push(rule.clone());
        }
        let settings = state.cache_settings.clone();
        drop(state);
        collections.set_selected_id(rule.id.into());
        collections.set_editor_open(false);
        self.persist_smart_collections(settings);
    }

    fn remove_smart_collection(&self, id: &str) {
        let Some(app) = self.app() else { return };
        let collections = app.global::<SmartCollectionsBridge>();
        if collections.get_profile_id().as_str() != profiles::current_id()
            || !writable_key("settings")
        {
            return;
        }
        let mut state = self.shared.lock().unwrap();
        let old_len = state.cache_settings.smart_collections.len();
        state
            .cache_settings
            .smart_collections
            .retain(|rule| rule.id != id);
        if state.cache_settings.smart_collections.len() == old_len {
            return;
        }
        let settings = state.cache_settings.clone();
        drop(state);
        if collections.get_selected_id().as_str() == id {
            collections.set_selected_id(SharedString::default());
        }
        if collections.get_editing_id().as_str() == id {
            collections.set_editor_open(false);
        }
        self.persist_smart_collections(settings);
    }

    fn persist_smart_collections(&self, settings: CacheSettings) {
        super::settings::allow_setting_field("smart_collections");
        set_active_cache_settings(settings);
        self.persist_settings();
        self.refresh_setting_sync();
        self.refresh_smart_collections();
        self.library_view_changed();
    }
}

/// The caller reads caches before taking Shared. Projection borrows fresh
/// library/progress state and never mutates manual categories or stored rules.
pub(super) fn matches_library_entry(
    rule: &SmartCollection,
    entry: &LibraryEntry,
    header: Option<&MetaHeader>,
    episodes: &[Video],
    progress: &HashMap<String, EpisodeProgress>,
) -> bool {
    let genres = header
        .filter(|header| !header.genres.is_empty())
        .map(|header| header.genres.as_slice())
        .unwrap_or(&entry.genres);
    let runtime = header.and_then(|header| cached_runtime_minutes(&header.details.runtime));
    let watched =
        library_title_completed(&entry.type_, &entry.id, episodes, progress, today_days());
    rule.matches(&entry.type_, watched, genres, runtime)
}

fn collection_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "{nanos:x}-{:x}-{:x}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// Accept only explicit bounded runtime values from the title metadata cache.
/// A missing/ambiguous value never satisfies an under-two-hours collection.
pub(super) fn cached_runtime_minutes(raw: &str) -> Option<u32> {
    let raw = raw.trim().to_ascii_lowercase();
    if raw.is_empty() || raw.len() > 64 {
        return None;
    }
    if raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return raw
            .parse::<u32>()
            .ok()
            .filter(|minutes| (1..=1440).contains(minutes));
    }
    let raw = raw.strip_prefix("pt").unwrap_or(&raw);
    let chars = raw.as_bytes();
    let mut index = 0;
    let mut total = 0u32;
    let mut units = HashSet::new();
    while index < chars.len() {
        while index < chars.len() && chars[index].is_ascii_whitespace() {
            index += 1;
        }
        if index == chars.len() {
            break;
        }
        let number_start = index;
        while index < chars.len() && chars[index].is_ascii_digit() {
            index += 1;
        }
        if number_start == index {
            return None;
        }
        let number = raw[number_start..index].parse::<u32>().ok()?;
        while index < chars.len() && chars[index].is_ascii_whitespace() {
            index += 1;
        }
        let unit_start = index;
        while index < chars.len() && chars[index].is_ascii_alphabetic() {
            index += 1;
        }
        let unit = match &raw[unit_start..index] {
            "h" | "hr" | "hrs" | "hour" | "hours" => 60,
            "m" | "min" | "mins" | "minute" | "minutes" => 1,
            _ => return None,
        };
        if !units.insert(unit) {
            return None;
        }
        total = total.checked_add(number.checked_mul(unit)?)?;
    }
    (1..=1440).contains(&total).then_some(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fresh_cached_metadata_and_progress_recompute_membership_without_editing_categories() {
        let entry: LibraryEntry = serde_json::from_value(serde_json::json!({"id":"film", "type_":"movie", "name":"Film", "categories":["Favorites"]})).unwrap();
        let rule = SmartCollection {
            id: "scifi".into(),
            name: "Short sci-fi".into(),
            media_type: SmartMediaType::Movies,
            watch_state: SmartWatchState::Unwatched,
            genre: "Sci-Fi".into(),
            max_runtime_minutes: Some(120),
        };
        let mut header = MetaHeader {
            genres: vec!["Science Fiction".into()],
            details: detail_extras::DetailExtras {
                runtime: "118 min".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut progress = HashMap::new();
        assert!(!matches_library_entry(&rule, &entry, None, &[], &progress));
        assert!(matches_library_entry(
            &rule,
            &entry,
            Some(&header),
            &[],
            &progress
        ));
        progress.insert(
            progress_map_key("film", "film"),
            EpisodeProgress {
                series_id: "film".into(),
                episode_id: "film".into(),
                watched: true,
                ..Default::default()
            },
        );
        assert!(!matches_library_entry(
            &rule,
            &entry,
            Some(&header),
            &[],
            &progress
        ));
        header.details.runtime.clear();
        progress.clear();
        assert!(!matches_library_entry(
            &rule,
            &entry,
            Some(&header),
            &[],
            &progress
        ));
        assert_eq!(entry.categories, vec!["Favorites"]);
    }

    #[test]
    fn runtime_parser_accepts_explicit_minutes_and_hours_without_guessing_unknown_values() {
        for (raw, expected) in [
            ("119 min", 119),
            ("90", 90),
            ("1h 45m", 105),
            ("1 hr 45 minutes", 105),
            ("PT2H", 120),
            ("PT1H30M", 90),
        ] {
            assert_eq!(cached_runtime_minutes(raw), Some(expected), "{raw}");
        }
        for raw in [
            "",
            "unknown",
            "about 90 min",
            "1:30",
            "90-120 min",
            "0 min",
            "2000 min",
            "1 min 2 min",
            "999999999999999 hours",
        ] {
            assert_eq!(cached_runtime_minutes(raw), None, "{raw}");
        }
    }
}
