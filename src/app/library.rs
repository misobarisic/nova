//! My Library: entries, categories and persistence.
use super::media_identity::{IdentityIndex, TitleInventory, TitleKey};
use super::*;

/// Presentation-only groups. The earliest saved physical source remains the
/// representative, independently of filters, sort order and sync arrival order.
/// Unknown legacy dates sort first; typed native IDs break every date tie.
#[derive(Clone, Default)]
pub(super) struct LibraryDisplayGroups {
    representatives: HashMap<TitleKey, TitleKey>,
    members: HashMap<TitleKey, Vec<TitleKey>>,
}

impl LibraryDisplayGroups {
    fn from_index(mut identities: Vec<(TitleKey, u64)>, index: &IdentityIndex) -> Self {
        identities.sort_by(|left, right| (left.1, &left.0).cmp(&(right.1, &right.0)));
        let mut result = Self::default();
        let mut representatives = Vec::<TitleKey>::new();
        for (key, _) in identities {
            let representative = representatives
                .iter()
                .find(|candidate| index.equivalent_titles(candidate, &key))
                .cloned()
                .unwrap_or_else(|| {
                    representatives.push(key.clone());
                    key.clone()
                });
            result
                .members
                .entry(representative.clone())
                .or_default()
                .push(key.clone());
            result.representatives.insert(key, representative);
        }
        result
    }

    pub(super) fn is_representative(&self, type_: &str, id: &str) -> bool {
        let key = (type_.to_owned(), id.to_owned());
        self.representatives
            .get(&key)
            .is_none_or(|representative| *representative == key)
    }

    fn representative(&self, type_: &str, id: &str) -> TitleKey {
        let key = (type_.to_owned(), id.to_owned());
        self.representatives.get(&key).cloned().unwrap_or(key)
    }

    fn members(&self, type_: &str, id: &str) -> Vec<TitleKey> {
        let representative = self.representative(type_, id);
        self.members
            .get(&representative)
            .cloned()
            .unwrap_or_else(|| vec![representative])
    }

    fn matching_query(&self, all: &[LibraryCandidate], needle: &str) -> HashSet<TitleKey> {
        all.iter()
            .filter(|candidate| candidate.name.to_lowercase().contains(needle))
            .map(|candidate| self.representative(&candidate.type_, &candidate.id))
            .collect()
    }

    fn project_candidates(
        &self,
        candidates: Vec<LibraryCandidate>,
        all: &[LibraryCandidate],
    ) -> Vec<LibraryCandidate> {
        let matching: HashSet<_> = candidates
            .iter()
            .map(|candidate| self.representative(&candidate.type_, &candidate.id))
            .collect();
        all.iter()
            .filter(|candidate| {
                self.is_representative(&candidate.type_, &candidate.id)
                    && matching.contains(&(candidate.type_.clone(), candidate.id.clone()))
            })
            .cloned()
            .collect()
    }
}

pub(super) fn display_groups_from_index(
    entries: &[LibraryEntry],
    index: &IdentityIndex,
) -> LibraryDisplayGroups {
    if profile_policy::restricted() {
        return LibraryDisplayGroups::default();
    }
    LibraryDisplayGroups::from_index(
        entries
            .iter()
            .map(|entry| ((entry.type_.clone(), entry.id.clone()), entry.added_at_secs))
            .collect(),
        index,
    )
}

pub(super) fn prepare_display_groups(
    entries: &[LibraryEntry],
    mut episodes: impl FnMut(&str, &str) -> Vec<Video>,
) -> LibraryDisplayGroups {
    if profile_policy::restricted() {
        return LibraryDisplayGroups::default();
    }
    let identities = entries
        .iter()
        .map(|entry| ((entry.type_.clone(), entry.id.clone()), entry.added_at_secs))
        .collect::<Vec<_>>();
    display_groups_for_identities(identities, None, &mut episodes)
}

fn display_groups_for_identities(
    identities: Vec<(TitleKey, u64)>,
    modal: Option<&TitleKey>,
    episodes: &mut impl FnMut(&str, &str) -> Vec<Video>,
) -> LibraryDisplayGroups {
    // A currently open third source may prove the connection between two
    // saved inventories. It contributes evidence, never a saved/display member.
    let additional = modal.filter(|modal| !identities.iter().any(|(key, _)| key == *modal));
    let inventories = identities
        .iter()
        .map(|(key, _)| key)
        .chain(additional)
        .filter(|(type_, _)| matches!(type_.as_str(), "series" | "anime"))
        .map(|(media_type, series_id)| TitleInventory {
            media_type: media_type.clone(),
            series_id: series_id.clone(),
            videos: episodes(media_type, series_id),
        });
    let index = IdentityIndex::new(inventories);
    LibraryDisplayGroups::from_index(identities, &index)
}

static DISPLAY_PROOF_REVISION: AtomicU64 = AtomicU64::new(1);
pub(super) type DisplayProofStamp = (u64, u64);
pub(super) fn display_proof_stamp() -> DisplayProofStamp {
    (
        DISPLAY_PROOF_REVISION.load(Ordering::Acquire),
        nova_providers::metadata_revision(),
    )
}
pub(super) fn invalidate_display_proof(key: &str) {
    if key.starts_with("episodes:") {
        DISPLAY_PROOF_REVISION.fetch_add(1, Ordering::AcqRel);
    }
}

type DisplayGroupsCache = (
    (String, u64),
    DisplayProofStamp,
    Vec<(TitleKey, u64)>,
    Option<TitleKey>,
    Arc<LibraryDisplayGroups>,
);
thread_local! {
    // This compact index owns no metadata or physical Library records. Its
    // proof stamp changes only for episode facts/provider revision; ordinary
    // playback/settings writes cannot force metadata I/O on the next action.
    static DISPLAY_GROUPS: std::cell::RefCell<Option<DisplayGroupsCache>> = const { std::cell::RefCell::new(None) };
}

impl Bridge {
    pub(super) fn library_display_groups(&self) -> Arc<LibraryDisplayGroups> {
        // A physical alias must never replace an explicitly allowed native ID
        // with an unapproved representative in a restricted Library.
        if profile_policy::restricted() {
            return Arc::new(LibraryDisplayGroups::default());
        }
        let (identities, modal) = {
            let state = self.shared.lock().unwrap();
            (
                state
                    .entries
                    .iter()
                    .map(|entry| ((entry.type_.clone(), entry.id.clone()), entry.added_at_secs))
                    .collect::<Vec<_>>(),
                state
                    .modal_item
                    .as_ref()
                    .map(|item| (item.type_.clone(), item.id.clone())),
            )
        };
        // Saved titles already contribute their inventory. Opening one adds
        // no new proof and must not invalidate the warm Library grouping.
        let modal = modal.filter(|key| !identities.iter().any(|(saved, _)| saved == key));
        let context = profiles::capture();
        let profile = (context.id.clone(), context.epoch);
        let revision = display_proof_stamp();
        if let Some(groups) = DISPLAY_GROUPS.with(|cache| {
            cache
                .borrow()
                .as_ref()
                .filter(|(owner, version, keys, proof, _)| {
                    owner == &profile
                        && *version == revision
                        && keys == &identities
                        && proof == &modal
                })
                .map(|(_, _, _, _, groups)| groups.clone())
        }) {
            return groups;
        }
        let groups = Arc::new(display_groups_for_identities(
            identities.clone(),
            modal.as_ref(),
            &mut |type_, id| read_episodes_cache_for(type_, id).unwrap_or_default(),
        ));
        if profiles::is_current(&context) && display_proof_stamp() == revision {
            DISPLAY_GROUPS.with(|cache| {
                *cache.borrow_mut() = Some((profile, revision, identities, modal, groups.clone()))
            });
        }
        groups
    }

    pub(super) fn cache_prepared_display_groups(
        &self,
        groups: Arc<LibraryDisplayGroups>,
        stamp: DisplayProofStamp,
        identities: Vec<(TitleKey, u64)>,
        modal: Option<TitleKey>,
    ) {
        if display_proof_stamp() != stamp {
            return;
        }
        let context = profiles::capture();
        let matches = {
            let state = self.shared.lock().unwrap();
            state
                .entries
                .iter()
                .map(|entry| ((entry.type_.clone(), entry.id.clone()), entry.added_at_secs))
                .eq(identities.iter().cloned())
                && state
                    .modal_item
                    .as_ref()
                    .map(|item| (item.type_.clone(), item.id.clone()))
                    == modal
        };
        if matches && profiles::is_current(&context) && display_proof_stamp() == stamp {
            let modal = modal.filter(|key| !identities.iter().any(|(saved, _)| saved == key));
            DISPLAY_GROUPS.with(|cache| {
                *cache.borrow_mut() = Some((
                    (context.id, context.epoch),
                    stamp,
                    identities,
                    modal,
                    groups,
                ))
            });
        }
    }

    fn cached_library_display_groups(&self) -> Arc<LibraryDisplayGroups> {
        if profile_policy::restricted() {
            return Arc::new(LibraryDisplayGroups::default());
        }
        let context = profiles::capture();
        let stamp = display_proof_stamp();
        let state = self.shared.lock().unwrap();
        let modal = state.modal_item.as_ref().filter(|modal| {
            !state
                .entries
                .iter()
                .any(|entry| entry.type_ == modal.type_ && entry.id == modal.id)
        });
        DISPLAY_GROUPS.with(|cache| {
            cache
                .borrow()
                .as_ref()
                .filter(|(owner, version, keys, proof, _)| {
                    owner == &(context.id, context.epoch)
                        && *version == stamp
                        && keys.len() == state.entries.len()
                        && keys
                            .iter()
                            .zip(&state.entries)
                            .all(|((key, added), entry)| {
                                key.0 == entry.type_
                                    && key.1 == entry.id
                                    && *added == entry.added_at_secs
                            })
                        && proof
                            .as_ref()
                            .map(|(type_, id)| (type_.as_str(), id.as_str()))
                            == modal.map(|m| (m.type_.as_str(), m.id.as_str()))
                })
                .map(|(_, _, _, _, groups)| groups.clone())
                .unwrap_or_default()
        })
    }

    pub(super) fn library_contains(&self, type_: &str, id: &str) -> bool {
        self.shared
            .lock()
            .unwrap()
            .entries
            .iter()
            .any(|e| e.id == id && e.type_ == type_)
    }

    fn current_library_entry(&self, index: usize) -> Option<LibraryEntry> {
        let displayed = self.app()?.get_library().row_data(index)?;
        let entry = {
            let state = self.shared.lock().unwrap();
            let (type_, id) = state.library_displayed.get(index)?;
            if id != displayed.id.as_str() {
                return None;
            }
            state
                .entries
                .iter()
                .find(|e| e.id == *id && e.type_ == *type_)?
                .clone()
        };
        profile_policy::allows_title(&profiles::ProfileStore::capture(), &entry.type_, &entry.id)
            .then_some(entry)
    }

    /// Keep only the fields needed to filter and order outside the shared
    /// state lock. Episode-cache and activity reads must not happen under it.
    fn current_library_candidates(&self) -> Vec<LibraryCandidate> {
        let (filter, query, sort) = self
            .app()
            .map(|a| {
                (
                    a.get_library_filter_category().to_string(),
                    a.get_library_query().to_string(),
                    a.get_library_sort(),
                )
            })
            .unwrap_or_default();
        let smart_collection = self.selected_smart_collection();
        let smart_watched = smart_collection
            .as_ref()
            .is_some_and(|rule| rule.watch_state != nova_config::SmartWatchState::Any);
        let smart_metadata = smart_collection.as_ref().is_some_and(|rule| {
            !rule.genre.trim().is_empty() || rule.max_runtime_minutes.is_some()
        });
        let identities = {
            let state = self.shared.lock().unwrap();
            state
                .entries
                .iter()
                .enumerate()
                .map(|(index, entry)| LibraryIdentitySnapshot::new(index, entry))
                .collect::<Vec<_>>()
        };
        let groups = self.library_display_groups();
        let all_candidates = {
            let state = self.shared.lock().unwrap();
            state
                .entries
                .iter()
                .enumerate()
                .map(|(index, entry)| LibraryCandidate::from_entry(entry, index))
                .collect::<Vec<_>>()
        };
        let activity = read_viewing_activity();
        let dynamic_filter = !filter.is_empty()
            && BUILTIN_FILTERS.contains(&filter.as_str())
            && filter != "On Hold"
            && filter != "Dropped";
        let options = LibraryViewOptions {
            filter: &filter,
            needle: String::new(),
            sort,
        };
        let query_matches = groups.matching_query(&all_candidates, &query.trim().to_lowercase());
        let mut candidates = Vec::new();
        for identity in identities {
            if !profile_policy::allows_title(
                &profiles::ProfileStore::capture(),
                &identity.type_,
                &identity.id,
            ) {
                continue;
            }
            let episodes = ((dynamic_filter && identity.watch_status == WatchStatus::Auto)
                || (smart_watched && identity.type_ != "movie"))
                .then(|| {
                    read_episodes_cache_for(&identity.type_, &identity.id).unwrap_or_default()
                });
            // All metadata I/O precedes Shared. Rules use existing cache facts;
            // selecting a collection never launches metadata requests.
            let header = smart_metadata
                .then(|| read_meta_header_for(&identity.type_, &identity.id))
                .flatten();
            let state = self.shared.lock().unwrap();
            let Some(entry) = state.entries.get(identity.index) else {
                continue;
            };
            if !identity.matches(entry) {
                continue;
            }
            if smart_collection.as_ref().is_some_and(|rule| {
                !smart_collections::matches_library_entry(
                    rule,
                    entry,
                    header.as_ref(),
                    episodes.as_deref().unwrap_or_default(),
                    &state.progress,
                )
            }) {
                continue;
            }
            if !query_matches.contains(&groups.representative(&entry.type_, &entry.id)) {
                continue;
            }
            if let Some(candidate) = project_library_candidate(
                entry,
                identity.index,
                episodes.as_deref(),
                &state.progress,
                &activity,
                &options,
            ) {
                candidates.push(candidate);
            }
        }
        order_library_candidates(
            groups.project_candidates(candidates, &all_candidates),
            options.sort,
        )
    }

    pub(super) fn library_view_changed(&self) {
        if let Some(app) = self.app() {
            app.set_library_scroll_y(0.0);
            app.set_library_kb_idx(0);
        }
        self.request_library_view();
    }

    /// Refresh the Slint `library` model from `Shared.entries`.
    pub(super) fn apply_library_to_ui(&self) {
        self.refresh_unified_search_library();
        self.enforce_profile_content_policy();
        self.refresh_smart_collections();
        self.migrate_viewing_activity();
        let card_snapshots = self.library_card_snapshots();
        self.remember_library_cards(&card_snapshots);
        // Search and sorting replace the model frequently. Retain decoded
        // artwork by identity so existing titles do not flash placeholders.
        let artwork: HashMap<String, MediaCard> = self
            .app()
            .map(|app| {
                app.get_library()
                    .iter()
                    .filter(|card| card.is_loaded)
                    .map(|card| (card.id.to_string(), card))
                    .collect()
            })
            .unwrap_or_default();
        let cards: Vec<MediaCard> = card_snapshots
            .into_iter()
            .map(|snapshot| {
                let previous = artwork
                    .get(&snapshot.id)
                    .filter(|card| card.poster_path.as_str() == snapshot.poster_url);
                MediaCard {
                    id: snapshot.id.into(),
                    title: snapshot.title.into(),
                    year: snapshot.year.into(),
                    poster_path: snapshot.poster_url.into(),
                    poster: previous.map(|card| card.poster.clone()).unwrap_or_default(),
                    is_loaded: previous.is_some(),
                    badge: snapshot.badge.into(),
                    status: text::tr(snapshot.status).into(),
                    status_key: snapshot.status.into(),
                    activity_action: snapshot.activity_action,
                    media_type: text::tr(if snapshot.is_movie { "Movie" } else { "TV" }).into(),
                    watched_count: snapshot.watched_count,
                    episode_count: snapshot.episode_count,
                    watched: snapshot.watched,
                }
            })
            .collect();
        if let Some(app) = self.app() {
            app.set_library(Rc::new(VecModel::from(cards)).into());
        }
        self.dispatch_library_posters();
    }

    /// Project display fields from current entries and borrowed progress.
    /// Cache reads happen before acquiring Shared, and callers decide whether
    /// to replace the model or patch its existing rows in place.
    fn library_card_snapshots(&self) -> Vec<LibraryCardSnapshot> {
        let view = self.current_library_candidates();
        let activity = read_viewing_activity();
        let mut snapshots = Vec::with_capacity(view.len());
        for candidate in view {
            let episodes =
                read_episodes_cache_for(&candidate.type_, &candidate.id).unwrap_or_default();
            let state = self.shared.lock().unwrap();
            let Some(current) = state.entries.get(candidate.entry_index) else {
                continue;
            };
            if !candidates_match_entry(&candidate, current) {
                continue;
            }
            if let Some(snapshot) =
                project_library_card(current, &candidate, &episodes, &state.progress, &activity)
            {
                snapshots.push(snapshot);
            }
        }
        snapshots
    }

    /// Recompute Library card state in place so decoded posters and pending
    /// poster loads survive playback progress updates.
    pub(super) fn update_library_badges(&self) {
        if self.shared.lock().unwrap().restoring_models {
            return;
        }
        let Some(app) = self.app() else { return };
        let cards = self.library_card_snapshots();
        let model = app.get_library();
        if model.row_count() != cards.len()
            || cards.iter().enumerate().any(|(index, snapshot)| {
                model
                    .row_data(index)
                    .is_none_or(|card| card.id.as_str() != snapshot.id)
            })
        {
            // Progress can move a title between automatic filters. Refresh
            // the model before indices can drift away from the displayed cards.
            self.apply_library_to_ui();
            return;
        }
        self.remember_library_cards(&cards);
        for (index, snapshot) in cards.into_iter().enumerate() {
            let Some(mut card) = model.row_data(index) else {
                continue;
            };
            if update_library_card_progress(&mut card, &snapshot) {
                model.set_row_data(index, card);
            }
        }
    }

    /// Main thread: library card context-menu action. `index` is into the
    /// current (filter-narrowed) library view; `action`: 1 mark the whole
    /// series watched, 2 mark it unwatched. Marking watched only touches
    /// released episodes — unaired ones stay untouched (they surface on
    /// Home → Upcoming instead).
    pub(super) fn library_watch_action(&self, index: usize, action: i32) {
        if let Some(entry) = self.current_library_entry(index) {
            self.library_entry_watch_action(&entry, action);
        }
    }

    fn library_entry_watch_action(&self, entry: &LibraryEntry, action: i32) {
        let target = match action {
            1 => true,
            2 => false,
            _ => return,
        };
        let today = today_days();
        let (series_id, episode_ids) = {
            let e = entry;
            let episodes = read_episodes_cache_for(&e.type_, &e.id).unwrap_or_default();
            let ids = if e.type_ == "movie" {
                vec![e.id.clone()]
            } else if target {
                episodes
                    .iter()
                    .filter(|v| episode_is_out(v, today))
                    .map(|v| v.id.clone())
                    .collect::<Vec<_>>()
            } else {
                episodes.iter().map(|v| v.id.clone()).collect::<Vec<_>>()
            };
            (e.id.clone(), ids)
        };
        if episode_ids.is_empty() {
            return;
        }
        let Some(_reservation) = persistence::reserve(1) else {
            return;
        };
        {
            let mut state = self.shared.lock().unwrap();
            for id in &episode_ids {
                Self::set_episode_watched_locked(&mut state.progress, &series_id, id, target);
            }
        }
        self.persist_and_refresh_progress();
    }

    /// Main thread: library card context-menu status action. `index` is
    /// into the current (filter-narrowed) library view; `status`: 0 back
    /// to automatic, 1 pin On Hold, 2 pin Dropped, 3 await return,
    /// 4 resume viewing, 5 pin Plan to Watch. Pins override the derived bucket; activity actions
    /// leave watched history unchanged.
    pub(super) fn library_status_action(&self, index: usize, status: i32) {
        if let Some(entry) = self.current_library_entry(index) {
            self.library_entry_status_action(&entry.type_, &entry.id, status);
        }
    }

    fn library_entry_status_action(&self, type_: &str, entry_id: &str, status: i32) {
        if status == 3 || status == 4 {
            let entry = {
                let state = self.shared.lock().unwrap();
                state
                    .entries
                    .iter()
                    .find(|e| e.id == entry_id && e.type_ == type_)
                    .cloned()
            };
            let Some(entry) = entry else { return };
            let episodes = read_episodes_cache_for(&entry.type_, entry_id).unwrap_or_default();
            let mut activity = read_viewing_activity();
            let action = {
                let state = self.shared.lock().unwrap();
                if !state
                    .entries
                    .iter()
                    .any(|current| current.id == entry.id && current.type_ == entry.type_)
                {
                    return;
                }
                viewing_menu_action(&entry, &episodes, &state.progress, activity.get(entry_id))
            };
            if action != status {
                return;
            }
            activity.insert(
                entry_id.into(),
                ViewingActivity {
                    phase: if status == 3 {
                        ViewingPhase::AwaitingReturn
                    } else {
                        ViewingPhase::Active
                    },
                },
            );
            write_json(VIEWING_ACTIVITY_KEY, &activity);
            self.refresh_library_progress_ui();
            return;
        }
        let watch_status = match status {
            1 => WatchStatus::OnHold,
            2 => WatchStatus::Dropped,
            5 => WatchStatus::PlanToWatch,
            0 => WatchStatus::Auto,
            _ => return,
        };
        let Some(_reservation) = persistence::reserve(1) else {
            return;
        };
        let changed = {
            let mut state = self.shared.lock().unwrap();
            match state
                .entries
                .iter_mut()
                .find(|e| e.id == entry_id && e.type_ == type_)
            {
                Some(e)
                    if e.watch_status != watch_status || e.imported_tracking_status.is_some() =>
                {
                    e.watch_status = watch_status;
                    e.imported_tracking_status = None;
                    true
                }
                _ => false,
            }
        };
        if changed {
            self.persist_library();
            self.apply_library_to_ui();
            self.refresh_library_progress_ui();
        }
    }

    /// Detail actions resolve the saved identity directly: a library filter
    /// may hide this entry or change after its status is edited.
    /// Actions: 0 toggle watched, 1 On Hold, 2 Dropped, 3 automatic, 4 remove,
    /// 5 await return, 6 resume viewing, 7 Plan to Watch.
    pub(super) fn detail_library_action(&self, action: i32) {
        let entry = {
            let state = self.shared.lock().unwrap();
            state.modal_item.as_ref().and_then(|item| {
                state
                    .entries
                    .iter()
                    .find(|e| e.id == item.id && e.type_ == item.type_)
                    .cloned()
            })
        };
        let Some(entry) = entry else { return };
        match action {
            0 => {
                let episodes = read_episodes_cache_for(&entry.type_, &entry.id).unwrap_or_default();
                let watched = library_title_completed(
                    &entry.type_,
                    &entry.id,
                    &episodes,
                    &self.shared.lock().unwrap().progress,
                    today_days(),
                );
                self.library_entry_watch_action(&entry, if watched { 2 } else { 1 });
            }
            1 => self.library_entry_status_action(&entry.type_, &entry.id, 1),
            2 => self.library_entry_status_action(&entry.type_, &entry.id, 2),
            3 => self.library_entry_status_action(&entry.type_, &entry.id, 0),
            4 => self.toggle_current_in_library(),
            5 => self.library_entry_status_action(&entry.type_, &entry.id, 3),
            6 => self.library_entry_status_action(&entry.type_, &entry.id, 4),
            7 => self.library_entry_status_action(&entry.type_, &entry.id, 5),
            _ => {}
        }
    }

    pub(super) fn refresh_detail_library_watched(&self) {
        let target = {
            let state = self.shared.lock().unwrap();
            state
                .modal_item
                .as_ref()
                .map(|item| (item.id.clone(), item.type_.clone()))
        };
        let episodes = target
            .as_ref()
            .and_then(|(id, type_)| read_episodes_cache_for(type_, id))
            .unwrap_or_default();
        let activity = read_viewing_activity();
        let (watched, action) = {
            let state = self.shared.lock().unwrap();
            let current_target = state
                .modal_item
                .as_ref()
                .map(|item| (item.id.as_str(), item.type_.as_str()));
            if current_target
                != target
                    .as_ref()
                    .map(|(id, type_)| (id.as_str(), type_.as_str()))
            {
                return;
            }
            let watched = target.as_ref().is_some_and(|(id, type_)| {
                library_title_completed(type_, id, &episodes, &state.progress, today_days())
            });
            let action = target
                .as_ref()
                .and_then(|(id, type_)| {
                    state
                        .entries
                        .iter()
                        .find(|entry| entry.id == *id && entry.type_ == *type_)
                        .map(|entry| {
                            viewing_menu_action(entry, &episodes, &state.progress, activity.get(id))
                        })
                })
                .unwrap_or(0);
            (watched, action)
        };
        if let Some(app) = self.app() {
            app.set_detail_library_activity_action(action);
            app.set_detail_library_watched(watched);
        }
    }

    /// Persist the current in-memory library list to the KV store.
    pub(super) fn persist_library(&self) {
        let entries = {
            let state = self.shared.lock().unwrap();
            state.entries.clone()
        };
        write_persisted_library(&entries);
    }

    /// Detail-page "Add to library / Remove from library" toggle.
    pub(super) fn toggle_current_in_library(&self) {
        self.toggle_current_in_library_inner(false);
    }

    fn toggle_current_in_library_inner(&self, allow_duplicate: bool) {
        if profile_policy::restricted() && !self.require_profile_management() {
            return;
        }
        let (id, type_, name, year, poster_url, background_url, genres, description, currently_in) = {
            let state = self.shared.lock().unwrap();
            let m = match state.modal_item.as_ref() {
                Some(m) => m,
                None => return,
            };
            (
                m.id.clone(),
                m.type_.clone(),
                m.name.clone(),
                m.year.clone(),
                m.poster_url.clone(),
                m.background_url.clone(),
                m.genres.clone(),
                m.description.clone(),
                state
                    .entries
                    .iter()
                    .any(|e| e.id == m.id && e.type_ == m.type_),
            )
        };

        if !currently_in && !allow_duplicate {
            let candidates = {
                let state = self.shared.lock().unwrap();
                possible_library_duplicates(&state.entries, &id, &type_, &name)
            };
            if !candidates.is_empty() {
                self.show_library_duplicates(&id, &candidates);
                return;
            }
        }
        let Some(_reservation) = persistence::reserve(1) else {
            return;
        };
        let groups = currently_in.then(|| self.library_display_groups());
        {
            let mut state = self.shared.lock().unwrap();
            if let Some(groups) = groups {
                remove_display_group(&mut state.entries, &groups, &type_, &id);
            } else {
                upsert_library(
                    &mut state.entries,
                    LibraryEntry {
                        id,
                        type_,
                        name,
                        year,
                        poster_url,
                        background_url,
                        genres,
                        description,
                        categories: Vec::new(),
                        watch_status: WatchStatus::Auto,
                        imported_tracking_status: None,
                        added_at_secs: now_secs(),
                    },
                );
            }
        }
        if let Some(app) = self.app() {
            if currently_in {
                app.set_in_library(false);
            } else {
                confirm_detail_library_add(&app);
            }
        }
        self.persist_library();
        self.apply_library_to_ui();
        self.sync_modal_category_flags();
    }

    fn show_library_duplicates(&self, target: &str, entries: &[LibraryEntry]) {
        let Some(app) = self.app() else { return };
        let cards = entries
            .iter()
            .map(|entry| MediaCard {
                id: entry.id.clone().into(),
                title: entry.name.clone().into(),
                year: entry.year.clone().into(),
                media_type: text::tr(if entry.type_ == "movie" {
                    "Movie"
                } else {
                    "TV"
                })
                .into(),
                poster_path: entry.poster_url.clone().into(),
                poster: decoded_cache_get(&sized_cache_key(
                    &entry.poster_url,
                    Some(DISPLAY_POSTER_SIDE),
                ))
                .map(Image::from_rgba8)
                .unwrap_or_default(),
                ..Default::default()
            })
            .collect::<Vec<_>>();
        app.set_library_duplicate_target(target.into());
        app.set_library_duplicates(Rc::new(VecModel::from(cards)).into());
        app.set_library_duplicate_selection(-1);
        app.set_library_duplicates_open(true);
        for entry in entries {
            let weak = self.app.clone();
            let id = entry.id.clone();
            let target = target.to_string();
            let url = entry.poster_url.clone();
            if url.is_empty() {
                continue;
            }
            net::fetch_image(url.clone(), Some(DISPLAY_POSTER_SIDE), move |pixels| {
                let Some(pixels) = pixels else { return };
                let _ = crate::app::profiles::invoke_from_event_loop(move || {
                    let Some(app) = weak.upgrade() else { return };
                    if !app.get_library_duplicates_open()
                        || app.get_library_duplicate_target().as_str() != target
                    {
                        return;
                    }
                    let model = app.get_library_duplicates();
                    let Some(rows) = model.as_any().downcast_ref::<VecModel<MediaCard>>() else {
                        return;
                    };
                    for index in 0..rows.row_count() {
                        if let Some(mut card) = rows.row_data(index)
                            && card.id.as_str() == id
                            && card.poster_path.as_str() == url
                        {
                            card.poster = Image::from_rgba8(pixels.clone());
                            card.is_loaded = true;
                            rows.set_row_data(index, card);
                        }
                    }
                });
            });
        }
    }

    pub(super) fn library_duplicate_action(&self, index: i32) {
        let Some(app) = self.app() else { return };
        if !app.get_library_duplicates_open() {
            return;
        }
        let target = app.get_library_duplicate_target();
        let mut state = self.shared.lock().unwrap();
        if state
            .modal_item
            .as_ref()
            .is_none_or(|item| item.id != target.as_str())
        {
            app.set_library_duplicates_open(false);
            return;
        }
        let target_type = state.modal_item.as_ref().unwrap().type_.clone();
        let selected = if index >= 0 {
            index
        } else {
            app.get_library_duplicate_selection()
        };
        let entry = (selected >= 0)
            .then(|| app.get_library_duplicates().row_data(selected as usize))
            .flatten()
            .and_then(|card| {
                state
                    .entries
                    .iter()
                    .find(|entry| entry.id == card.id.as_str() && entry.type_ == target_type)
                    .cloned()
            });
        if index == -1 {
            app.set_library_duplicates_open(false);
            // A paired device may have saved it while the dialog was open.
            // Add anyway must never turn into an unintended remove toggle.
            if state
                .entries
                .iter()
                .any(|entry| entry.id == target.as_str() && entry.type_ == target_type)
            {
                app.set_in_library(true);
                return;
            }
            drop(state);
            self.toggle_current_in_library_inner(true);
            return;
        }
        let Some(entry) = entry else {
            app.set_library_duplicates_open(false);
            return;
        };
        if index == -2 {
            app.set_library_duplicates_open(false);
            drop(state);
            self.open_library_entry(entry);
            return;
        }
        let item = state.modal_item.as_ref().unwrap();
        let videos = read_episodes_cache_for(&entry.type_, &entry.id).unwrap_or_default();
        let (copies, unmatched) = duplicate_progress_copies(&entry, item, &videos, &state.progress);
        // Metadata/progress may change while the warning is open. Require a
        // fresh review if the number of transferable records changes.
        if index >= 0
            || app.get_library_duplicate_matched() != copies.len() as i32
            || app.get_library_duplicate_unmatched() != unmatched as i32
        {
            app.set_library_duplicate_selection(selected);
            app.set_library_duplicate_matched(copies.len() as i32);
            app.set_library_duplicate_unmatched(unmatched as i32);
            return;
        }
        if index != -3 {
            return;
        }
        if state
            .entries
            .iter()
            .any(|entry| entry.id == target.as_str() && entry.type_ == target_type)
        {
            app.set_library_duplicates_open(false);
            app.set_in_library(true);
            return;
        }
        let Some(_reservation) = persistence::reserve(2) else {
            return;
        };
        let replacement = LibraryEntry {
            id: item.id.clone(),
            type_: item.type_.clone(),
            name: item.name.clone(),
            year: item.year.clone(),
            poster_url: item.poster_url.clone(),
            background_url: item.background_url.clone(),
            genres: item.genres.clone(),
            description: item.description.clone(),
            categories: entry.categories.clone(),
            watch_status: entry.watch_status,
            imported_tracking_status: entry.imported_tracking_status,
            added_at_secs: entry.added_at_secs,
        };
        // Keep old progress intact; unmapped history can still be recovered by
        // reopening the old source. Never overwrite progress already recorded
        // for the destination, including explicit unwatched intent.
        for copy in copies {
            state
                .progress
                .entry(progress_map_key(&copy.series_id, &copy.episode_id))
                .or_insert(copy);
        }
        remove_library_entry(&mut state.entries, &entry.type_, &entry.id);
        upsert_library(&mut state.entries, replacement);
        drop(state);
        app.set_library_duplicates_open(false);
        confirm_detail_library_add(&app);
        self.persist_library();
        self.persist_and_refresh_progress();
        self.apply_library_to_ui();
        self.sync_modal_category_flags();
    }

    /// Remove the library card at `index` (badge on the library grid). The
    /// index is into the filtered grid and resolves through the same ordered
    /// candidate projection used to render the model.
    pub(super) fn remove_library_at(&self, index: usize) {
        if profile_policy::restricted() && !self.require_profile_management() {
            return;
        }
        let Some(selected) = self.current_library_entry(index) else {
            return;
        };
        let Some(_reservation) = persistence::reserve(1) else {
            return;
        };
        let groups = self.library_display_groups();
        let removed = {
            let mut state = self.shared.lock().unwrap();
            remove_display_group(&mut state.entries, &groups, &selected.type_, &selected.id)
        };
        if removed.is_empty() {
            return;
        }
        if let Some(app) = self.app() {
            let open = self
                .shared
                .lock()
                .unwrap()
                .modal_item
                .as_ref()
                .map(|item| (item.type_.clone(), item.id.clone()));
            if open.is_some_and(|key| removed.contains(&key)) {
                app.set_in_library(false);
            }
        }
        self.persist_library();
        self.apply_library_to_ui();
    }

    /// Open a saved library item: reuse the detail page and the normal
    /// movie -> streams / series -> episodes flow. The `index` is into the
    /// filtered grid and resolves through the ordered candidate projection.
    pub(super) fn open_library_item(&self, index: usize) {
        let entry = self.current_library_entry(index);
        let Some(e) = entry else { return };
        self.open_library_entry_at(e, Some(index));
    }

    pub(super) fn open_library_entry(&self, e: LibraryEntry) {
        self.open_library_entry_at(e, None);
    }

    fn open_library_entry_at(&self, e: LibraryEntry, return_index: Option<usize>) {
        if profile_policy::entry_blocked() {
            return;
        }
        if !profile_policy::allows_title(&profiles::ProfileStore::capture(), &e.type_, &e.id) {
            return;
        }
        if let Some(app) = self.app() {
            let content = (*app).global::<nova_ui_shared::DetailContent>();
            content.set_restoring_navigation(false);
            content.set_synopsis_expanded(false);
            content.set_hero_expanded(false);
            content.set_trailers_expanded(false);
        }
        let app = match self.app() {
            Some(a) => a,
            None => return,
        };

        {
            let mut state = self.shared.lock().unwrap();
            state.related_detail_history.clear();
            state.modal_item = Some(ModalItem {
                details: Default::default(),
                open_token: Arc::new(()),
                pending_watch_now: None,
                episodes_loading: false,
                id: e.id.clone(),
                type_: e.type_.clone(),
                request_id: e.id.clone(),
                videos: Vec::new(),
                season_backdrops: HashMap::new(),
                seasons: Vec::new(),
                season_index: 0,
                episode_page: 0,
                name: e.name.clone(),
                year: e.year.clone(),
                poster_url: e.poster_url.clone(),
                // Persisted since the backdrop-URL fix; older entries carry
                // "" and are upgraded by the meta fetch below. Same for
                // genres/description (persisted since the header-text fix).
                background_url: e.background_url.clone(),
                logo_url: String::new(),
                description: e.description.clone(),
                genres: e.genres.clone(),
            });
            state.streams.clear();
        }

        // Prefer resolved detail text to an older saved preview, with the
        // entry supplying fields the header cache has not resolved yet.
        self.restore_modal_header_from_cache(&e.id);
        self.publish_detail_extras();
        let (paint_background, paint_description, paint_genres, paint_year) = {
            let state = self.shared.lock().unwrap();
            match state.modal_item.as_ref() {
                Some(m) => (
                    m.background_url.clone(),
                    m.description.clone(),
                    m.genres.clone(),
                    m.year.clone(),
                ),
                None => return,
            }
        };
        // Write back anything the header cache filled, so the next open
        // (and restart) reads it straight from the entry.
        if !paint_background.is_empty() && e.background_url.is_empty() {
            self.persist_backdrop_for(&e.id, &paint_background);
        }
        self.persist_header_for(&e.id, &paint_genres, &paint_description, &paint_year, false);

        app.set_selected_title(SharedString::from(&e.name));
        app.set_detail_is_movie(e.type_ == "movie");
        app.set_stream_selector_open(false);
        app.set_detail_episode_reveal_request(0);
        app.set_selected_year(SharedString::from(&paint_year));
        app.set_selected_index(-1);
        self.clear_streams();
        app.set_modal_episodes(false);
        app.set_detail_deep_stream(false);
        apply_stream_episode_context(&app, None);
        app.set_season_names(Rc::new(VecModel::<SharedString>::from(vec![])).into());
        app.set_season_combo_idx(-1);
        app.set_season_cards(Rc::new(VecModel::<SeasonCard>::from(vec![])).into());
        app.set_episode_rows(Rc::new(VecModel::<EpisodeRow>::from(vec![])).into());
        app.set_selected_poster(Image::default());
        // Sync fast path (mirrors episode thumbnails): the persisted URL +
        // decoded LRU paint instantly on reentry, including after restart
        // (pixels stay file-cached). Misses keep the placeholder and fall
        // back to the async load below.
        if let Some(pixels) = backdrop_pixels_cached(&paint_background) {
            app.set_selected_backdrop(Image::from_rgba8(pixels));
        } else {
            app.set_selected_backdrop(Image::default());
            if !paint_background.is_empty() {
                self.load_detail_backdrop(paint_background.clone(), e.id.clone());
            }
        }
        self.load_current_detail_logo();
        // Header text paints synchronously from the persisted snapshot (or
        // prefetched header cache) so pills + synopsis don't wait for (or
        // flash in after) the meta fetch. Older entries with empty snapshots
        // keep the previous behaviour: blank now, upgraded when meta answers.
        if paint_description.is_empty() {
            app.set_selected_description(SharedString::default());
        } else {
            app.set_selected_description(SharedString::from(&paint_description));
        }
        app.set_selected_genre_list(
            Rc::new(VecModel::from(
                paint_genres
                    .iter()
                    .map(SharedString::from)
                    .collect::<Vec<_>>(),
            ))
            .into(),
        );
        app.set_detail_tab(0);
        app.set_episode_filter(SharedString::default());
        app.set_in_library(true);
        self.sync_modal_category_flags();

        // Fast path: the library grid may already hold this item's decoded
        // poster (the user is looking at it right now). Reuse it so the
        // detail page shows the image instantly instead of a blank flash
        // while the background decode finishes.
        let mut poster_shown = false;
        if !e.poster_url.is_empty()
            && let Some(card) = return_index.and_then(|index| app.get_library().row_data(index))
            && card.id == e.id.as_str()
            && card.is_loaded
        {
            app.set_selected_poster(card.poster);
            poster_shown = true;
        }

        // Fallback: poster loads off the UI thread (cached bytes decode
        // fast, misses are downloaded); opening the page stays instant.
        if !poster_shown && !e.poster_url.is_empty() {
            self.load_detail_poster(e.poster_url.clone(), e.id.clone());
        }

        // Grid return target: backing out re-focuses the opened card (the
        // lifted kb props survive the grid page's recreation). `index` is
        // into the filtered view, which persists via the filter property.
        if let Some(index) = return_index {
            app.set_library_kb_zone(2);
            app.set_library_kb_idx(index as i32);
        }
        // Same-entry reopen restores content choices before mounting.
        self.restore_detail_snapshot(&e.id);

        if e.type_ == "movie" {
            self.start_stream_search(e.id.clone());
            self.refresh_movie_meta(e.id);
            self.refresh_watch_action();
        } else {
            self.prepare_episodes(e.id, e.type_);
        }
        self.load_detail_similar();
        app.set_modal_visible(true);
    }

    pub(super) fn show_library_page(&self) {
        if let Some(app) = self.app() {
            app.set_show_about(false);
            app.set_show_home(false);
            app.set_show_library(true);
            app.set_show_settings(false);
        }
        // Publish prepared rows without holding navigation behind a complete
        // filter/sort/episode-cache pass; retained artwork stays visible.
        self.request_progress_projection();
        // Fill missing metadata. Failed posters separately recover mappings
        // even when the entry already has cached episodes and header text.
        // Unlike Discover, this is not gated by "Prefetch episode metadata":
        // My Library is the user's own curated list and should be ready.
        self.prefetch_library_meta();
        self.refresh_saved_episode_metadata();
    }

    pub(super) fn add_category_to_ui(&self, name: &str) {
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        // Built-in automatic filters are reserved: a user category with
        // the same name would hijack the library filter.
        if BUILTIN_FILTERS.contains(&name.as_str()) {
            return;
        }
        {
            let mut state = self.shared.lock().unwrap();
            add_category(&mut state.cache_settings.categories, &name);
            write_settings(&state.cache_settings);
        }
        self.apply_category_rows();
    }

    pub(super) fn remove_category_from_ui(&self, index: usize) {
        let name = {
            let state = self.shared.lock().unwrap();
            state.cache_settings.categories.get(index).cloned()
        };
        let Some(name) = name else { return };
        let Some(_reservation) = persistence::reserve(1) else {
            return;
        };
        {
            let mut state = self.shared.lock().unwrap();
            state.cache_settings.categories.retain(|c| *c != name);
            for e in state.entries.iter_mut() {
                e.categories.retain(|c| c != &name);
            }
            write_settings(&state.cache_settings);
            write_persisted_library(&state.entries);
        }
        // If the deleted category was the active filter, reset to "All".
        if let Some(app) = self.app()
            && app.get_library_filter_category().as_str() == name
        {
            app.set_library_filter_category(SharedString::default());
        }
        self.apply_category_rows();
        self.apply_library_to_ui();
    }

    pub(super) fn toggle_entry_category(&self, cat_name: &str) {
        let key = self
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_ref()
            .map(|item| (item.type_.clone(), item.id.clone()));
        let Some((type_, id)) = key else { return };
        let Some(_reservation) = persistence::reserve(1) else {
            return;
        };
        let groups = self.library_display_groups();
        {
            let mut state = self.shared.lock().unwrap();
            toggle_display_group_category(&mut state.entries, &groups, &type_, &id, cat_name);
        }
        self.persist_library();
        self.apply_library_to_ui();
        self.sync_modal_category_flags();
    }

    pub(super) fn sync_modal_category_flags(&self) {
        let (key, all_cats) = {
            let state = self.shared.lock().unwrap();
            let Some(item) = state.modal_item.as_ref() else {
                return;
            };
            (
                (item.type_.clone(), item.id.clone()),
                state.cache_settings.categories.clone(),
            )
        };
        // Painting an opening must not rebuild proof for every saved show.
        // A metadata-change projection refreshes these flags with new proof.
        let groups = self.cached_library_display_groups();
        let members = groups
            .members(&key.0, &key.1)
            .into_iter()
            .collect::<HashSet<_>>();
        let flags = {
            let state = self.shared.lock().unwrap();
            all_cats
                .iter()
                .map(|category| {
                    state.entries.iter().any(|entry| {
                        members.contains(&(entry.type_.clone(), entry.id.clone()))
                            && entry.categories.contains(category)
                    })
                })
                .collect::<Vec<_>>()
        };
        if let Some(app) = self.app() {
            app.set_selected_category_count(flags.iter().filter(|flag| **flag).count() as i32);
            app.set_modal_category_flags(Rc::new(VecModel::from(flags)).into());
        }
    }

    pub(super) fn filter_library(&self, category: &str) {
        if let Some(app) = self.app() {
            app.set_library_filter_category(SharedString::from(category));
            app.set_library_scroll_y(0.0);
            app.set_library_kb_idx(0);
        }
        self.sync_library_category_index();
        self.request_library_view();
    }

    /// Index of the active library category filter within the category
    /// names (-1 = All). Keeps keyboard cycling in sync with pill selection
    /// and category add/remove.
    pub(super) fn sync_library_category_index(&self) {
        let Some(app) = self.app() else {
            return;
        };
        let filter = app.get_library_filter_category().to_string();
        let cats = self
            .shared
            .lock()
            .unwrap()
            .cache_settings
            .categories
            .clone();
        // Pill order: built-ins first (see apply_category_rows),
        // then user categories. -1 = All.
        let idx = if filter.is_empty() {
            -1
        } else if let Some(i) = BUILTIN_FILTERS.iter().position(|b| *b == filter) {
            i as i32
        } else {
            cats.iter()
                .position(|c| c == &filter)
                .map(|i| (i + BUILTIN_FILTERS.len()) as i32)
                .unwrap_or(-1)
        };
        app.set_library_category_index(idx);
    }

    /// Sync the category list from settings to the UI. Built-in automatic
    /// filters lead the rail, user categories follow.
    pub(super) fn apply_category_rows(&self) {
        let cats = {
            let state = self.shared.lock().unwrap();
            state.cache_settings.categories.clone()
        };
        let rows: Vec<CategoryRow> = cats
            .iter()
            .map(|c| CategoryRow {
                name: SharedString::from(c),
            })
            .collect();
        // `category_names` holds the values the filter is keyed on (the
        // stable identifiers also used by `BUILTIN_FILTERS`; the new release
        // filter is namespaced so existing category names remain selectable);
        // `category_labels` is what the pills show — the same list with
        // the automatic buckets translated. Picking a pill still sends back
        // the value, so a localized label can never leak into the data.
        let names: Vec<SharedString> = BUILTIN_FILTERS
            .iter()
            .map(|s| SharedString::from(*s))
            .chain(cats.iter().map(SharedString::from))
            .collect();
        let labels: Vec<SharedString> = BUILTIN_FILTERS
            .iter()
            .map(|s| SharedString::from(text::tr(builtin_filter_label(s))))
            .chain(cats.iter().map(SharedString::from))
            .collect();
        if let Some(app) = self.app() {
            app.set_category_rows(Rc::new(VecModel::from(rows)).into());
            app.set_library_category_names(Rc::new(VecModel::from(names)).into());
            app.set_library_category_labels(Rc::new(VecModel::from(labels)).into());
        }
        self.sync_library_category_index();
    }
}

/// Feedback follows accepted local additions, including duplicate moves.
/// Loading existing state or applying a peer's records emits no receipt.
fn confirm_detail_library_add(app: &AppWindow) {
    app.set_in_library(true);
    app.set_detail_library_save_request(app.get_detail_library_save_request().wrapping_add(1));
}

/// Collision-free native title identity shared by storage rows and sync keys.
pub(super) fn library_key(type_: &str, id: &str) -> String {
    serde_json::to_string(&(type_, id)).expect("string tuple serializes")
}

/// Add/overwrite an entry in place, keeping entries unique by `(type_, id)` and
/// preserving insertion order. Returns true when the entry was newly added.
pub(crate) fn upsert_library(entries: &mut Vec<LibraryEntry>, entry: LibraryEntry) -> bool {
    if let Some(existing) = entries
        .iter_mut()
        .find(|e| e.id == entry.id && e.type_ == entry.type_)
    {
        *existing = entry;
        false
    } else {
        entries.push(entry);
        true
    }
}
fn remove_display_group(
    entries: &mut Vec<LibraryEntry>,
    groups: &LibraryDisplayGroups,
    type_: &str,
    id: &str,
) -> Vec<TitleKey> {
    let members = groups
        .members(type_, id)
        .into_iter()
        .collect::<HashSet<_>>();
    let mut removed = Vec::new();
    entries.retain(|entry| {
        let key = (entry.type_.clone(), entry.id.clone());
        if members.contains(&key) {
            removed.push(key);
            false
        } else {
            true
        }
    });
    removed
}

fn toggle_display_group_category(
    entries: &mut [LibraryEntry],
    groups: &LibraryDisplayGroups,
    type_: &str,
    id: &str,
    category: &str,
) {
    let members = groups
        .members(type_, id)
        .into_iter()
        .collect::<HashSet<_>>();
    let selected = entries.iter().any(|entry| {
        members.contains(&(entry.type_.clone(), entry.id.clone()))
            && entry.categories.iter().any(|value| value == category)
    });
    for entry in entries
        .iter_mut()
        .filter(|entry| members.contains(&(entry.type_.clone(), entry.id.clone())))
    {
        let id = entry.id.clone();
        let mut categories = entry.categories.clone();
        if selected {
            categories.retain(|value| value != category);
        } else {
            categories.push(category.into());
        }
        set_entry_categories(std::slice::from_mut(entry), &id, categories);
    }
}

/// Remove an entry by `id`. Returns true when something was removed.
pub(crate) fn remove_library_entry(entries: &mut Vec<LibraryEntry>, type_: &str, id: &str) -> bool {
    let before = entries.len();
    entries.retain(|e| e.id != id || e.type_ != type_);
    entries.len() != before
}
/// Persisted library entries (empty when absent/unreadable).
pub(crate) fn read_persisted_library() -> Vec<LibraryEntry> {
    persistence::pending_library()
        .unwrap_or_else(|| read_json::<Vec<LibraryEntry>>("library").unwrap_or_default())
}
/// Queue an ordered Library save (synchronous during startup/projection).
pub(crate) fn write_persisted_library(entries: &[LibraryEntry]) {
    persistence::write_library(entries);
}
/// Add a new category name to settings (deduplicates, preserves order).
pub(crate) fn add_category(categories: &mut Vec<String>, name: &str) {
    if !categories.iter().any(|c| c == name) {
        categories.push(name.to_string());
    }
}
/// Remove a category name from settings and strip it from all library entries.
#[allow(dead_code)]
pub(crate) fn remove_category(
    categories: &mut Vec<String>,
    entries: &mut [LibraryEntry],
    name: &str,
) {
    categories.retain(|c| c != name);
    for e in entries.iter_mut() {
        e.categories.retain(|c| c != name);
    }
}
/// Rename a category everywhere (settings + all library entries).
#[allow(dead_code)]
pub(crate) fn rename_category(
    categories: &mut [String],
    entries: &mut [LibraryEntry],
    old: &str,
    new: &str,
) {
    if let Some(c) = categories.iter_mut().find(|c| **c == old) {
        *c = new.to_string();
    }
    for e in entries.iter_mut() {
        if let Some(c) = e.categories.iter_mut().find(|c| **c == old) {
            *c = new.to_string();
        }
    }
}
/// Set the categories for a single library entry (by id).
pub(crate) fn set_entry_categories(entries: &mut [LibraryEntry], id: &str, cats: Vec<String>) {
    if let Some(e) = entries.iter_mut().find(|e| e.id == id) {
        e.categories = cats;
    }
}
/// Fold fresh header text into a library entry. Returns true when anything
/// changed. With `overwrite`, fresh non-empty values replace stored text
/// (unfinished-show refresh saves updated data); otherwise only empty slots
/// are filled (backfill without addon churn). Fresh empty values never blank
/// stored text in either mode.
pub(crate) fn merge_library_header_text(
    entry: &mut LibraryEntry,
    genres: &[String],
    description: &str,
    year: &str,
    overwrite: bool,
) -> bool {
    let mut touched = false;
    if !genres.is_empty()
        && (overwrite || entry.genres.is_empty())
        && entry.genres.as_slice() != genres
    {
        entry.genres = genres.to_vec();
        touched = true;
    }
    if !description.is_empty()
        && (overwrite || entry.description.is_empty())
        && entry.description != description
    {
        entry.description = description.to_string();
        touched = true;
    }
    if !year.is_empty() && (overwrite || entry.year.is_empty()) && entry.year != year {
        entry.year = year.to_string();
        touched = true;
    }
    touched
}

#[derive(Clone)]
struct LibraryCandidate {
    entry_index: usize,
    id: String,
    type_: String,
    name: String,
    year: String,
    poster_url: String,
    categories: Vec<String>,
    watch_status: WatchStatus,
    imported_tracking_status: Option<nova_tracking::ListStatus>,
    added_at_secs: u64,
}

struct LibraryIdentitySnapshot {
    index: usize,
    id: String,
    type_: String,
    watch_status: WatchStatus,
    imported_tracking_status: Option<nova_tracking::ListStatus>,
}

struct LibraryViewOptions<'a> {
    filter: &'a str,
    needle: String,
    sort: i32,
}

#[derive(Clone)]
pub(super) struct LibraryCardSnapshot {
    id: String,
    type_: String,
    title: String,
    year: String,
    poster_url: String,
    badge: String,
    status: &'static str,
    activity_action: i32,
    is_movie: bool,
    watched_count: i32,
    episode_count: i32,
    watched: bool,
}

#[derive(Clone, Default)]
pub(super) struct ViewState(Arc<Mutex<PendingView>>);

#[derive(Default)]
struct PendingView {
    generation: u64,
    view: super::progress_projection::View,
    running: bool,
    cached: Option<CachedView>,
}

#[derive(Clone, PartialEq, Eq)]
struct ViewStamp {
    revision: u64,
    proof: DisplayProofStamp,
    day: i64,
    modal: Option<TitleKey>,
    smart: Option<nova_config::SmartCollection>,
}

impl ViewStamp {
    fn new(view: &super::progress_projection::View, modal: Option<TitleKey>) -> Self {
        Self {
            revision: super::progress_projection::revision(),
            proof: display_proof_stamp(),
            day: today_days(),
            modal,
            smart: view.smart.clone(),
        }
    }
    fn current(&self) -> bool {
        self.revision == super::progress_projection::revision()
            && self.proof == display_proof_stamp()
            && self.day == today_days()
    }
}

/// Compact derived rows, not episode inventories. Changing a filter reuses
/// these facts until Library/progress/metadata/policy or the local day changes.
#[derive(Clone)]
pub(super) struct CachedView {
    stamp: ViewStamp,
    index: Arc<ViewIndex>,
}

struct ViewIndex {
    all: Vec<LibraryCandidate>,
    cards: HashMap<TitleKey, LibraryCardSnapshot>,
    eligible: HashSet<TitleKey>,
    groups: LibraryDisplayGroups,
}

impl CachedView {
    pub(super) fn select(
        &self,
        view: &super::progress_projection::View,
    ) -> Vec<LibraryCardSnapshot> {
        let index = &self.index;
        let matches = index
            .groups
            .matching_query(&index.all, &view.query.trim().to_lowercase());
        let candidates = index
            .all
            .iter()
            .filter(|entry| {
                let key = (entry.type_.clone(), entry.id.clone());
                index.eligible.contains(&key)
                    && matches.contains(&index.groups.representative(&entry.type_, &entry.id))
                    && (view.filter.is_empty()
                        || if BUILTIN_FILTERS.contains(&view.filter.as_str()) {
                            index.cards.get(&key).is_some_and(|card| {
                                card.status == builtin_filter_label(&view.filter)
                            })
                        } else {
                            entry.categories.contains(&view.filter)
                        })
            })
            .cloned()
            .collect();
        order_library_candidates(
            index.groups.project_candidates(candidates, &index.all),
            view.sort,
        )
        .into_iter()
        .filter_map(|entry| index.cards.get(&(entry.type_, entry.id)).cloned())
        .collect()
    }
}

pub(super) fn prepare_cached_view(
    state: &Shared,
    view: &super::progress_projection::View,
    metadata: &super::progress_projection::Metadata<'_>,
    activity: &HashMap<String, ViewingActivity>,
    groups: &LibraryDisplayGroups,
    modal: Option<TitleKey>,
) -> CachedView {
    let stamp = ViewStamp::new(view, modal);
    let all = state
        .entries
        .iter()
        .enumerate()
        .map(|(index, entry)| LibraryCandidate::from_entry(entry, index))
        .collect::<Vec<_>>();
    let mut cards = HashMap::new();
    let mut eligible = HashSet::new();
    let owner = profiles::ProfileStore::capture();
    for candidate in &all {
        if !profile_policy::allows_title(&owner, &candidate.type_, &candidate.id) {
            continue;
        }
        let entry = &state.entries[candidate.entry_index];
        let cached = metadata.title(&entry.type_, &entry.id);
        let episodes = cached
            .episodes
            .as_deref()
            .map(Vec::as_slice)
            .unwrap_or_default();
        let key = (entry.type_.clone(), entry.id.clone());
        if view.smart.as_ref().is_none_or(|rule| {
            smart_collections::matches_library_entry(
                rule,
                entry,
                cached.header.as_deref(),
                episodes,
                &state.progress,
            )
        }) {
            eligible.insert(key.clone());
        }
        if let Some(card) =
            project_library_card(entry, candidate, episodes, &state.progress, activity)
        {
            cards.insert(key, card);
        }
    }
    CachedView {
        stamp,
        index: Arc::new(ViewIndex {
            all,
            cards,
            eligible,
            groups: groups.clone(),
        }),
    }
}

impl Bridge {
    pub(super) fn cache_library_view(&self, cached: CachedView) {
        let Some(view) = super::progress_projection::View::capture(self) else {
            return;
        };
        let state = self.shared.lock().unwrap();
        let modal = state
            .modal_item
            .as_ref()
            .map(|item| (item.type_.clone(), item.id.clone()));
        if cached.stamp == ViewStamp::new(&view, modal) {
            state.library_view.0.lock().unwrap().cached = Some(cached);
        }
    }

    /// Filter/search/sort callbacks only capture input. A warm view needs no
    /// metadata I/O, and a cold view never blocks the event loop on decoding.
    pub(super) fn request_library_view(&self) {
        let Some(view) = super::progress_projection::View::capture(self) else {
            return;
        };
        let owner = self.shared.lock().unwrap().library_view.clone();
        {
            let mut pending = owner.0.lock().unwrap();
            pending.generation = pending.generation.wrapping_add(1);
            pending.view = view;
            if pending.running {
                return;
            }
            pending.running = true;
        }
        let bridge = self.clone();
        let context = profiles::capture();
        thread::spawn(move || {
            loop {
                if !profiles::is_current(&context) {
                    owner.0.lock().unwrap().running = false;
                    return;
                }
                let (generation, view) = {
                    let pending = owner.0.lock().unwrap();
                    (pending.generation, pending.view.clone())
                };
                let modal = bridge
                    .shared
                    .lock()
                    .unwrap()
                    .modal_item
                    .as_ref()
                    .map(|item| (item.type_.clone(), item.id.clone()));
                let stamp = ViewStamp::new(&view, modal.clone());
                let current = || profiles::is_current(&context) && stamp.current();
                let cached = owner
                    .0
                    .lock()
                    .unwrap()
                    .cached
                    .as_ref()
                    .filter(|cached| cached.stamp == stamp)
                    .cloned();
                let cached = cached.or_else(|| {
                    let data = super::progress_projection::snapshot(&bridge, current)?;
                    let metadata = super::progress_projection::Metadata::new(
                        |type_, id| {
                            (
                                read_episodes_cache_for(type_, id),
                                read_meta_header_for(type_, id),
                            )
                        },
                        current,
                    );
                    let identities = data
                        .entries
                        .iter()
                        .map(|entry| ((entry.type_.clone(), entry.id.clone()), entry.added_at_secs))
                        .collect();
                    let groups = display_groups_for_identities(
                        identities,
                        modal.as_ref(),
                        &mut |type_, id| metadata.episodes(type_, id).as_ref().clone(),
                    );
                    Some(prepare_cached_view(
                        &data,
                        &view,
                        &metadata,
                        &read_viewing_activity(),
                        &groups,
                        modal.clone(),
                    ))
                });
                let Some(cached) = cached.filter(|_| current()) else {
                    continue;
                };
                {
                    let mut pending = owner.0.lock().unwrap();
                    pending.cached = Some(cached.clone());
                    if pending.generation != generation {
                        continue;
                    }
                }
                let cards = cached.select(&view);
                let callback_owner = owner.clone();
                let queued = profiles::invoke_from_event_loop(move || {
                    let matches = {
                        let mut pending = callback_owner.0.lock().unwrap();
                        pending.running = false;
                        pending.generation == generation
                    };
                    let modal = bridge
                        .shared
                        .lock()
                        .unwrap()
                        .modal_item
                        .as_ref()
                        .map(|item| (item.type_.clone(), item.id.clone()));
                    if !matches
                        || cached.stamp != ViewStamp::new(&view, modal)
                        || super::progress_projection::View::capture(&bridge).as_ref()
                            != Some(&view)
                    {
                        bridge.request_library_view();
                        return;
                    }
                    bridge.publish_progress_cards(cards);
                });
                if queued.is_err() {
                    owner.0.lock().unwrap().running = false;
                }
                return;
            }
        });
    }
}

/// Cache-only worker projection retained as a policy oracle for regressions.
#[cfg(test)]
pub(super) fn prepare_progress_cards(
    state: &Shared,
    view: &super::progress_projection::View,
    metadata: &super::progress_projection::Metadata<'_>,
    activity: &HashMap<String, ViewingActivity>,
    groups: &LibraryDisplayGroups,
) -> Vec<LibraryCardSnapshot> {
    let options = LibraryViewOptions {
        filter: &view.filter,
        needle: String::new(),
        sort: view.sort,
    };
    let all = state
        .entries
        .iter()
        .enumerate()
        .map(|(index, entry)| LibraryCandidate::from_entry(entry, index))
        .collect::<Vec<_>>();
    let query_matches = groups.matching_query(&all, &view.query.trim().to_lowercase());
    let candidates = state
        .entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            if !profile_policy::allows_title(
                &profiles::ProfileStore::capture(),
                &entry.type_,
                &entry.id,
            ) {
                return None;
            }
            if !query_matches.contains(&groups.representative(&entry.type_, &entry.id)) {
                return None;
            }
            let cached = metadata.title(&entry.type_, &entry.id);
            let episodes = cached
                .episodes
                .as_deref()
                .map(Vec::as_slice)
                .unwrap_or_default();
            if view.smart.as_ref().is_some_and(|rule| {
                !smart_collections::matches_library_entry(
                    rule,
                    entry,
                    cached.header.as_deref(),
                    episodes,
                    &state.progress,
                )
            }) {
                return None;
            }
            project_library_candidate(
                entry,
                index,
                Some(episodes),
                &state.progress,
                activity,
                &options,
            )
        })
        .collect();
    order_library_candidates(groups.project_candidates(candidates, &all), options.sort)
        .iter()
        .filter_map(|candidate| {
            project_library_card(
                &state.entries[candidate.entry_index],
                candidate,
                metadata
                    .episodes(&candidate.type_, &candidate.id)
                    .as_slice(),
                &state.progress,
                activity,
            )
        })
        .collect()
}

impl Bridge {
    /// Keep backend action identities aligned with the model being painted.
    fn remember_library_cards(&self, cards: &[LibraryCardSnapshot]) {
        self.shared.lock().unwrap().library_displayed = cards
            .iter()
            .map(|card| (card.type_.clone(), card.id.clone()))
            .collect();
    }

    /// Publish prepared values in place whenever the visible identities remain
    /// the same. A newly matching automatic filter retains artwork by URL.
    pub(super) fn publish_progress_cards(&self, cards: Vec<LibraryCardSnapshot>) {
        self.refresh_unified_search_library();
        let Some(app) = self.app() else { return };
        self.remember_library_cards(&cards);
        let model = app.get_library();
        if model.row_count() == cards.len()
            && cards.iter().enumerate().all(|(index, snapshot)| {
                model.row_data(index).is_some_and(|card| {
                    card.id.as_str() == snapshot.id
                        && card.poster_path.as_str() == snapshot.poster_url
                        && card.title.as_str() == snapshot.title
                        && card.year.as_str() == snapshot.year
                })
            })
        {
            for (index, snapshot) in cards.into_iter().enumerate() {
                if let Some(mut card) = model.row_data(index)
                    && update_library_card_progress(&mut card, &snapshot)
                {
                    model.set_row_data(index, card);
                }
            }
            // Retained identities can still have failed artwork. Revisiting
            // Library must retry those rows even when no model rebuild is needed.
            self.dispatch_library_posters();
            return;
        }
        let artwork: HashMap<_, _> = model
            .iter()
            .filter(|card| card.is_loaded)
            .map(|card| {
                (
                    (card.id.to_string(), card.poster_path.to_string()),
                    card.poster,
                )
            })
            .collect();
        let rows = cards
            .into_iter()
            .map(|snapshot| {
                let poster = artwork
                    .get(&(snapshot.id.clone(), snapshot.poster_url.clone()))
                    .cloned();
                let is_loaded = poster.is_some();
                MediaCard {
                    id: snapshot.id.into(),
                    title: snapshot.title.into(),
                    year: snapshot.year.into(),
                    poster_path: snapshot.poster_url.into(),
                    poster: poster.unwrap_or_default(),
                    is_loaded,
                    badge: snapshot.badge.into(),
                    status: text::tr(snapshot.status).into(),
                    status_key: snapshot.status.into(),
                    activity_action: snapshot.activity_action,
                    media_type: text::tr(if snapshot.is_movie { "Movie" } else { "TV" }).into(),
                    watched_count: snapshot.watched_count,
                    episode_count: snapshot.episode_count,
                    watched: snapshot.watched,
                }
            })
            .collect::<Vec<_>>();
        app.set_library(Rc::new(VecModel::from(rows)).into());
        self.dispatch_library_posters();
    }
}

impl LibraryIdentitySnapshot {
    fn new(index: usize, entry: &LibraryEntry) -> Self {
        Self {
            index,
            id: entry.id.clone(),
            type_: entry.type_.clone(),
            watch_status: entry.watch_status,
            imported_tracking_status: entry.imported_tracking_status,
        }
    }

    fn matches(&self, entry: &LibraryEntry) -> bool {
        entry.id == self.id
            && entry.type_ == self.type_
            && entry.watch_status == self.watch_status
            && entry.imported_tracking_status == self.imported_tracking_status
    }
}

impl LibraryCandidate {
    fn from_entry(entry: &LibraryEntry, entry_index: usize) -> Self {
        Self {
            entry_index,
            id: entry.id.clone(),
            type_: entry.type_.clone(),
            name: entry.name.clone(),
            year: entry.year.clone(),
            poster_url: entry.poster_url.clone(),
            categories: entry.categories.clone(),
            watch_status: entry.watch_status,
            imported_tracking_status: entry.imported_tracking_status,
            added_at_secs: entry.added_at_secs,
        }
    }
}

fn candidates_match_entry(candidate: &LibraryCandidate, entry: &LibraryEntry) -> bool {
    candidate.id == entry.id
        && candidate.type_ == entry.type_
        && candidate.name == entry.name
        && candidate.year == entry.year
        && candidate.categories == entry.categories
        && candidate.watch_status == entry.watch_status
        && candidate.imported_tracking_status == entry.imported_tracking_status
        && candidate.added_at_secs == entry.added_at_secs
}

fn order_library_candidates(
    mut entries: Vec<LibraryCandidate>,
    sort: i32,
) -> Vec<LibraryCandidate> {
    // Newer insertions win ties, including older entries with no timestamp.
    entries.reverse();
    match sort {
        1 => entries.sort_by_cached_key(|e| e.name.to_lowercase()),
        2 => entries.sort_by_cached_key(|e| std::cmp::Reverse(e.year.clone())),
        _ => entries.sort_by_key(|e| std::cmp::Reverse(e.added_at_secs)),
    }
    entries
}

#[cfg(test)]
fn selected_library_entries(
    entries: &[LibraryEntry],
    candidates: &[LibraryCandidate],
) -> Vec<LibraryEntry> {
    candidates
        .iter()
        .filter_map(|candidate| selected_library_entry(entries, candidate))
        .collect()
}

#[cfg(test)]
fn selected_library_entry(
    entries: &[LibraryEntry],
    candidate: &LibraryCandidate,
) -> Option<LibraryEntry> {
    let entry = entries.get(candidate.entry_index)?;
    candidates_match_entry(candidate, entry).then(|| entry.clone())
}

fn project_library_candidate(
    entry: &LibraryEntry,
    entry_index: usize,
    episodes: Option<&[Video]>,
    progress: &HashMap<String, EpisodeProgress>,
    activity: &HashMap<String, ViewingActivity>,
    options: &LibraryViewOptions<'_>,
) -> Option<LibraryCandidate> {
    let today = today_days();
    let matches_filter = if options.filter.is_empty() {
        true
    } else if BUILTIN_FILTERS.contains(&options.filter) {
        let bucket = entry.watch_status.badge_label().unwrap_or_else(|| {
            library_activity_bucket(
                entry,
                episodes.unwrap_or_default(),
                progress,
                activity.get(&entry.id),
                today,
            )
        });
        bucket == builtin_filter_label(options.filter)
    } else {
        entry
            .categories
            .iter()
            .any(|category| category == options.filter)
    };
    (matches_filter && entry.name.to_lowercase().contains(&options.needle))
        .then(|| LibraryCandidate::from_entry(entry, entry_index))
}

fn project_library_card(
    current: &LibraryEntry,
    candidate: &LibraryCandidate,
    videos: &[Video],
    progress: &HashMap<String, EpisodeProgress>,
    activity: &HashMap<String, ViewingActivity>,
) -> Option<LibraryCardSnapshot> {
    if !candidates_match_entry(candidate, current) || current.poster_url != candidate.poster_url {
        return None;
    }
    let today = today_days();
    let status = candidate.watch_status.badge_label().unwrap_or_else(|| {
        library_activity_bucket(
            current,
            videos,
            progress,
            activity.get(&candidate.id),
            today,
        )
    });
    let (watched_count, episode_count) = if candidate.type_ == "movie" {
        (0, 0)
    } else {
        library_episode_counts(&candidate.id, videos, progress)
    };
    Some(LibraryCardSnapshot {
        id: candidate.id.clone(),
        type_: candidate.type_.clone(),
        title: candidate.name.clone(),
        year: candidate.year.clone(),
        poster_url: candidate.poster_url.clone(),
        badge: library_badge_for(&candidate.id, videos, progress),
        status,
        activity_action: viewing_menu_action(
            current,
            videos,
            progress,
            activity.get(&candidate.id),
        ),
        is_movie: candidate.type_ == "movie",
        watched_count,
        episode_count,
        watched: library_title_completed(&candidate.type_, &candidate.id, videos, progress, today),
    })
}

fn update_library_card_progress(card: &mut MediaCard, snapshot: &LibraryCardSnapshot) -> bool {
    let status = text::tr(snapshot.status);
    let media_type = text::tr(if snapshot.is_movie { "Movie" } else { "TV" });
    if card.badge.as_str() == snapshot.badge
        && card.watched == snapshot.watched
        && card.status.as_str() == status
        && card.status_key.as_str() == snapshot.status
        && card.activity_action == snapshot.activity_action
        && card.media_type.as_str() == media_type
        && card.watched_count == snapshot.watched_count
        && card.episode_count == snapshot.episode_count
    {
        return false;
    }
    card.badge = snapshot.badge.clone().into();
    card.status = status.into();
    card.status_key = snapshot.status.into();
    card.activity_action = snapshot.activity_action;
    card.media_type = media_type.into();
    card.watched = snapshot.watched;
    card.watched_count = snapshot.watched_count;
    card.episode_count = snapshot.episode_count;
    true
}

fn builtin_filter_label(value: &str) -> &str {
    if value == NEW_EPISODES_FILTER {
        "New episodes"
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warm_filter_index_reuses_metadata_and_matches_legacy_grouped_filters() {
        let (mut entries, inventories) = grouped_identity_fixture();
        entries.extend((0..400).map(|number| serde_json::from_value(serde_json::json!({
            "id": format!("large-{number}"), "type_":"series", "name": format!("Saved {number}"),
            "year": format!("{}", 2000 + number % 25), "categories":["Favorites"],
            "imported_tracking_status": match number % 3 { 0 => "planning", 1 => "watching", _ => "completed" }
        })).unwrap()));
        let state = Shared {
            entries,
            ..Default::default()
        };
        let groups = prepare_display_groups(&state.entries, |_, id| {
            inventories.get(id).cloned().unwrap_or_default()
        });
        let reads = std::cell::Cell::new(0);
        let metadata = super::super::progress_projection::Metadata::new(
            |_, id| {
                reads.set(reads.get() + 1);
                (Some(inventories.get(id).cloned().unwrap_or_default()), None)
            },
            || true,
        );
        let view = super::super::progress_projection::View::default();
        let cache = prepare_cached_view(&state, &view, &metadata, &HashMap::new(), &groups, None);
        let prepared_reads = reads.get();
        for filter in [
            "",
            "Plan to Watch",
            "Watching",
            "Completed",
            "Anime",
            "Favorites",
        ] {
            for query in ["", "native", "saved 3"] {
                for sort in 0..=2 {
                    let view = super::super::progress_projection::View {
                        filter: filter.into(),
                        query: query.into(),
                        sort,
                        ..Default::default()
                    };
                    let selected = cache.select(&view);
                    assert_eq!(
                        reads.get(),
                        prepared_reads,
                        "warm filter must not reopen metadata"
                    );
                    let oracle = super::super::progress_projection::Metadata::new(
                        |_, id| (Some(inventories.get(id).cloned().unwrap_or_default()), None),
                        || true,
                    );
                    let expected =
                        prepare_progress_cards(&state, &view, &oracle, &HashMap::new(), &groups);
                    let facts = |cards: Vec<LibraryCardSnapshot>| {
                        cards
                            .into_iter()
                            .map(|card| {
                                (
                                    card.type_,
                                    card.id,
                                    card.status,
                                    card.watched,
                                    card.watched_count,
                                    card.episode_count,
                                )
                            })
                            .collect::<Vec<_>>()
                    };
                    assert_eq!(facts(selected), facts(expected), "{filter} {query} {sort}");
                }
            }
        }
    }

    fn grouped_identity_fixture() -> (Vec<LibraryEntry>, HashMap<String, Vec<Video>>) {
        let mut entries: Vec<LibraryEntry> = serde_json::from_value(serde_json::json!([
            {"id":"tt100", "type_":"series", "name":"Canonical show", "added_at_secs":2, "categories":["Anime"], "genres":["Fantasy"]},
            {"id":"kitsu:42", "type_":"series", "name":"Native show", "added_at_secs":1, "categories":["Drama"]},
            {"id":"season-one", "type_":"series", "name":"Partial season", "added_at_secs":3},
            {"id":"unproven", "type_":"series", "name":"Native show", "added_at_secs":4}
        ])).unwrap();
        entries[0].poster_url = "canonical-poster".into();
        entries[1].poster_url = "native-poster".into();
        let videos: Vec<Video> = (1..=2)
            .map(|episode| {
                serde_json::from_value(serde_json::json!({
                    "id":format!("kitsu:42:{episode}"), "season":1, "episode":episode,
                    "released": if episode == 1 { "2020-01-01" } else { "2099-01-01" },
                    "novaStreamIds":[format!("tt100:1:{episode}")],
                    "novaMetadataRevision":nova_providers::metadata_revision()
                }))
                .unwrap()
            })
            .collect();
        let inventories = HashMap::from([
            ("tt100".into(), videos.clone()),
            ("kitsu:42".into(), videos.clone()),
            ("season-one".into(), vec![videos[0].clone()]),
            (
                "unproven".into(),
                vec![Video {
                    id: "private-episode".into(),
                    ..Default::default()
                }],
            ),
        ]);
        (entries, inventories)
    }

    #[test]
    fn modal_inventory_bridges_ui_and_worker_proof_without_becoming_a_saved_member() {
        let video = |id: &str, aliases: &[&str]| {
            serde_json::from_value::<Video>(serde_json::json!({
            "id":id,"novaStreamIds":aliases,"novaMetadataRevision":nova_providers::metadata_revision()
        })).unwrap()
        };
        let inventories = HashMap::from([
            ("native-a", vec![video("episode-a", &["kitsu:42:1"])]),
            ("native-b", vec![video("episode-b", &["tt100:1:1"])]),
            ("modal", vec![video("bridge", &["kitsu:42:1", "tt100:1:1"])]),
        ]);
        let identities = vec![
            (("series".into(), "native-a".into()), 1),
            (("series".into(), "native-b".into()), 2),
        ];
        let mut load = |_: &str, id: &str| inventories[id].clone();
        let without = display_groups_for_identities(identities.clone(), None, &mut load);
        assert!(without.is_representative("series", "native-b"));
        let modal = ("series".into(), "modal".into());
        let with = display_groups_for_identities(identities.clone(), Some(&modal), &mut load);
        assert!(!with.is_representative("series", "native-b"));
        assert_eq!(with.members("series", "native-a").len(), 2);
        assert!(
            !with.representatives.contains_key(&modal),
            "proof-only modal is never added to Library"
        );
        let index = IdentityIndex::new(
            identities
                .iter()
                .map(|(key, _)| key)
                .chain(std::iter::once(&modal))
                .map(|(media_type, id)| TitleInventory {
                    media_type: media_type.clone(),
                    series_id: id.clone(),
                    videos: load(media_type, id),
                }),
        );
        let worker = LibraryDisplayGroups::from_index(identities, &index);
        assert_eq!(with.representatives, worker.representatives);
    }

    #[test]
    fn exact_title_groups_keep_native_actions_union_filters_and_remove_all_physical_members() {
        let (entries, inventories) = grouped_identity_fixture();
        let groups = prepare_display_groups(&entries, |_, id| {
            inventories.get(id).cloned().unwrap_or_default()
        });
        let all = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| LibraryCandidate::from_entry(entry, index))
            .collect::<Vec<_>>();
        let collapsed = groups.project_candidates(all.clone(), &all);
        let ids = collapsed
            .iter()
            .map(|candidate| candidate.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["kitsu:42", "season-one", "unproven"]);
        let selected = selected_library_entry(&entries, &collapsed[0]).unwrap();
        assert_eq!(
            selected.id, "kitsu:42",
            "actions retain the original native addon request ID"
        );
        assert_eq!(selected.poster_url, "native-poster");
        let options = LibraryViewOptions {
            filter: "Anime",
            needle: String::new(),
            sort: 0,
        };
        let members = entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                project_library_candidate(
                    entry,
                    index,
                    None,
                    &HashMap::new(),
                    &HashMap::new(),
                    &options,
                )
            })
            .collect();
        let filtered = groups.project_candidates(members, &all);
        assert_eq!(filtered.len(), 1);
        assert_eq!(
            filtered[0].id, "kitsu:42",
            "category membership on an alias must not change the shown source"
        );
        assert!(
            groups
                .matching_query(&all, "native")
                .contains(&groups.representative("series", "tt100"))
        );
        let mut reversed = entries.clone();
        reversed.reverse();
        let reordered = prepare_display_groups(&reversed, |_, id| {
            inventories.get(id).cloned().unwrap_or_default()
        });
        assert_eq!(
            groups.representative("series", "tt100"),
            reordered.representative("series", "tt100")
        );
        let mut changed = entries.clone();
        toggle_display_group_category(&mut changed, &groups, "series", "kitsu:42", "Anime");
        assert!(
            changed
                .iter()
                .all(|entry| !entry.categories.iter().any(|category| category == "Anime"))
        );
        assert_eq!(changed[1].categories, ["Drama"]);
        toggle_display_group_category(&mut changed, &groups, "series", "kitsu:42", "Favorites");
        assert!(changed[..2].iter().all(|entry| {
            entry
                .categories
                .iter()
                .any(|category| category == "Favorites")
        }));
        assert!(changed[2..].iter().all(|entry| entry.categories.is_empty()));
        let removed = remove_display_group(&mut changed, &groups, "series", "kitsu:42");
        assert_eq!(removed.len(), 2);
        assert_eq!(
            changed
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            ["season-one", "unproven"]
        );
        assert_eq!(
            entries.len(),
            4,
            "presentation and category actions never rewrite source identities"
        );
    }

    #[test]
    fn worker_library_and_home_use_the_same_exact_title_representative() {
        let (entries, inventories) = grouped_identity_fixture();
        let groups = prepare_display_groups(&entries, |_, id| {
            inventories.get(id).cloned().unwrap_or_default()
        });
        let metadata = progress_projection::Metadata::new(
            |_, id| (inventories.get(id).cloned(), None),
            || true,
        );
        let mut state = Shared {
            entries,
            ..Default::default()
        };
        let view = progress_projection::View {
            filter: "Anime".into(),
            query: "Native".into(),
            smart: Some(nova_config::SmartCollection {
                genre: "Fantasy".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let cards = prepare_progress_cards(&state, &view, &metadata, &HashMap::new(), &groups);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].id, "kitsu:42");
        assert_eq!(cards[0].poster_url, "native-poster");
        for id in ["kitsu:42", "tt100"] {
            state.progress.insert(
                progress_map_key(id, "kitsu:42:1"),
                EpisodeProgress {
                    series_id: id.into(),
                    episode_id: "kitsu:42:1".into(),
                    watched: true,
                    updated_at_secs: 10,
                    ..Default::default()
                },
            );
        }
        let original_progress = state.progress.clone();
        let _home = super::super::home::prepare_progress_home(
            &mut state,
            &metadata,
            &HashMap::new(),
            &HashMap::new(),
            &groups,
        );
        assert_eq!(state.upcoming_list.len(), 1);
        assert_eq!(state.upcoming_list[0].series_id, "kitsu:42");
        assert_eq!(state.upcoming_list[0].episode_id, "kitsu:42:2");
        assert_eq!(state.entries.len(), 4);
        assert_eq!(
            serde_json::to_value(&state.progress).unwrap(),
            serde_json::to_value(&original_progress).unwrap(),
            "display grouping cannot create progress actions"
        );
    }

    #[test]
    fn movie_completion_without_episode_metadata_matches_cards_and_filters() {
        let mut movie: LibraryEntry = serde_json::from_value(serde_json::json!({
            "id": "movie", "type_": "movie", "name": "Movie"
        }))
        .unwrap();
        let activity = HashMap::new();
        // A series sharing the raw title ID cannot complete a movie.
        let mut progress = HashMap::from([(
            progress_map_key("movie", "episode"),
            EpisodeProgress {
                series_id: "movie".into(),
                episode_id: "episode".into(),
                watched: true,
                ..Default::default()
            },
        )]);
        for (record, status) in [
            (None, "Plan to Watch"),
            (Some((600.0, false)), "Watching"),
            (Some((3600.0, true)), "Completed"),
            (Some((0.0, false)), "Watching"),
        ] {
            movie.imported_tracking_status = record.map(|_| nova_tracking::ListStatus::Planning);
            if let Some((position, watched)) = record {
                progress.insert(
                    progress_map_key("movie", "movie"),
                    EpisodeProgress {
                        series_id: "movie".into(),
                        episode_id: "movie".into(),
                        position_secs: position,
                        duration_secs: 3600.0,
                        watched,
                        updated_at_secs: 1,
                        ..Default::default()
                    },
                );
            }
            let candidate = LibraryCandidate::from_entry(&movie, 0);
            let card = project_library_card(&movie, &candidate, &[], &progress, &activity).unwrap();
            assert_eq!(card.status, status);
            assert_eq!(card.watched, status == "Completed");
            if card.watched {
                assert!(card.badge.is_empty());
            }
            for filter in ["Plan to Watch", "Watching", "Completed"] {
                assert_eq!(
                    project_library_candidate(
                        &movie,
                        0,
                        None,
                        &progress,
                        &activity,
                        &LibraryViewOptions {
                            filter,
                            needle: String::new(),
                            sort: 0,
                        }
                    )
                    .is_some(),
                    filter == status
                );
            }
        }
        movie.watch_status = WatchStatus::OnHold;
        let candidate = LibraryCandidate::from_entry(&movie, 0);
        assert_eq!(
            project_library_card(&movie, &candidate, &[], &progress, &activity)
                .unwrap()
                .status,
            "On Hold"
        );
    }

    #[test]
    fn search_and_sort_keep_entries_and_action_indices_together() {
        let entry = |id: &str, type_: &str, name: &str, year: &str, added: u64, category: &str| {
            LibraryEntry {
                id: id.into(),
                type_: type_.into(),
                name: name.into(),
                year: year.into(),
                poster_url: String::new(),
                background_url: String::new(),
                genres: vec![],
                description: String::new(),
                categories: vec![category.into()],
                watch_status: WatchStatus::Auto,
                imported_tracking_status: None,
                added_at_secs: added,
            }
        };
        let entries = vec![
            entry("dup", "series", "Zebra", "2020", 10, "A"),
            entry("dup", "movie", "alpha", "2026", 20, "A"),
            entry("c", "series", "Beta", "2024", 20, "A"),
        ];
        let identities = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| LibraryIdentitySnapshot::new(index, entry))
            .collect::<Vec<_>>();
        let progress = HashMap::new();
        let activity = HashMap::new();
        let project = |filter: &str, query: &str, sort| {
            let options = LibraryViewOptions {
                filter,
                needle: query.trim().to_lowercase(),
                sort,
            };
            order_library_candidates(
                identities
                    .iter()
                    .filter_map(|identity| {
                        let entry = entries.get(identity.index)?;
                        if !identity.matches(entry) {
                            return None;
                        }
                        project_library_candidate(
                            entry,
                            identity.index,
                            None,
                            &progress,
                            &activity,
                            &options,
                        )
                    })
                    .collect(),
                sort,
            )
        };
        fn labels(rows: &[LibraryCandidate]) -> Vec<(&str, &str)> {
            rows.iter()
                .map(|entry| (entry.type_.as_str(), entry.id.as_str()))
                .collect::<Vec<_>>()
        }
        assert_eq!(
            labels(&project("", "", 0)),
            [("series", "c"), ("movie", "dup"), ("series", "dup")]
        );
        assert_eq!(
            labels(&project("", "", 1)),
            [("movie", "dup"), ("series", "c"), ("series", "dup")]
        );
        assert_eq!(
            labels(&project("", "", 2)),
            [("movie", "dup"), ("series", "c"), ("series", "dup")]
        );
        assert_eq!(labels(&project("A", "  ALP  ", 0)), [("movie", "dup")]);
        let selected = selected_library_entries(&entries, &project("A", "ZEB", 0));
        assert_eq!(selected.len(), 1);
        assert_eq!(
            (selected[0].type_.as_str(), selected[0].name.as_str()),
            ("series", "Zebra")
        );
        assert_eq!(project("missing-category", "", 0).len(), 0);
        assert!(project("", "missing", 0).is_empty());
    }

    #[test]
    fn card_projection_keeps_typed_episode_caches_and_badges_separate() {
        let entry = |id: &str, type_: &str, name: &str| LibraryEntry {
            id: id.into(),
            type_: type_.into(),
            name: name.into(),
            year: String::new(),
            poster_url: String::new(),
            background_url: String::new(),
            genres: vec![],
            description: String::new(),
            categories: vec![],
            watch_status: WatchStatus::Auto,
            imported_tracking_status: None,
            added_at_secs: 1,
        };
        let entries = [
            entry("same", "series", "Series"),
            entry("same", "movie", "Movie"),
            entry("unknown", "series", "Unknown episodes"),
        ];
        let view = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| LibraryCandidate::from_entry(entry, index))
            .collect::<Vec<_>>();
        let video = |id: &str| Video {
            id: id.into(),
            released: Some("2000-01-01".into()),
            ..Video::default()
        };
        let episodes = HashMap::from([
            (("series", "same"), vec![video("one"), video("two")]),
            (("movie", "same"), vec![]),
        ]);
        let mut progress = HashMap::new();
        progress.insert(
            progress_map_key("same", "one"),
            EpisodeProgress {
                series_id: "same".into(),
                episode_id: "one".into(),
                watched: true,
                ..EpisodeProgress::default()
            },
        );
        progress.insert(
            progress_map_key("unknown", "old"),
            EpisodeProgress {
                series_id: "unknown".into(),
                episode_id: "old".into(),
                watched: true,
                ..EpisodeProgress::default()
            },
        );
        let activity = HashMap::new();
        let cards = view
            .iter()
            .filter_map(|candidate| {
                let videos = episodes
                    .get(&(candidate.type_.as_str(), candidate.id.as_str()))
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                project_library_card(
                    &entries[candidate.entry_index],
                    candidate,
                    videos,
                    &progress,
                    &activity,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(cards.len(), 3);
        assert_eq!(
            cards[0].badge,
            library_badge_for("same", &episodes[&("series", "same")], &progress)
        );
        assert_eq!((cards[0].watched_count, cards[0].episode_count), (1, 2));
        assert_eq!(
            cards[1].badge,
            library_badge_for("same", &episodes[&("movie", "same")], &progress)
        );
        assert_eq!((cards[1].watched_count, cards[1].episode_count), (0, 0));
        assert_eq!(cards[2].badge, library_badge_for("unknown", &[], &progress));

        let mut loaded_card = MediaCard {
            id: "same".into(),
            title: "Series".into(),
            year: String::new().into(),
            poster_path: "poster-url".into(),
            poster: Image::default(),
            is_loaded: true,
            badge: String::new().into(),
            watched: false,
            status: String::new().into(),
            status_key: String::new().into(),
            activity_action: 0,
            media_type: "TV".into(),
            watched_count: 0,
            episode_count: 0,
        };
        assert!(update_library_card_progress(&mut loaded_card, &cards[0]));
        assert_eq!(loaded_card.poster_path.as_str(), "poster-url");
        assert!(loaded_card.is_loaded);
    }

    #[test]
    fn built_in_bucket_uses_the_matching_media_types_episode_cache() {
        let entry = |type_: &str| LibraryEntry {
            id: "shared-id".into(),
            type_: type_.into(),
            name: type_.into(),
            year: String::new(),
            poster_url: String::new(),
            background_url: String::new(),
            genres: vec![],
            description: String::new(),
            categories: vec![],
            watch_status: WatchStatus::Auto,
            imported_tracking_status: None,
            added_at_secs: 1,
        };
        let entries = [entry("series"), entry("movie")];
        let identities = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| LibraryIdentitySnapshot::new(index, entry))
            .collect::<Vec<_>>();
        let video = Video {
            id: "episode".into(),
            released: Some("2000-01-01".into()),
            ..Video::default()
        };
        let episodes = HashMap::from([
            (("series", "shared-id"), vec![video]),
            (("movie", "shared-id"), vec![]),
        ]);
        let mut progress = HashMap::new();
        progress.insert(
            progress_map_key("shared-id", "episode"),
            EpisodeProgress {
                series_id: "shared-id".into(),
                episode_id: "episode".into(),
                watched: true,
                ..EpisodeProgress::default()
            },
        );
        let activity = HashMap::new();
        let options = LibraryViewOptions {
            filter: "Completed",
            needle: String::new(),
            sort: 0,
        };
        let candidates = order_library_candidates(
            identities
                .iter()
                .filter_map(|identity| {
                    let entry = entries.get(identity.index)?;
                    if !identity.matches(entry) {
                        return None;
                    }
                    let videos = episodes
                        .get(&(identity.type_.as_str(), identity.id.as_str()))
                        .map(Vec::as_slice);
                    project_library_candidate(
                        entry,
                        identity.index,
                        videos,
                        &progress,
                        &activity,
                        &options,
                    )
                })
                .collect(),
            options.sort,
        );
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].type_, "series");
    }
}

/// Similar-name suggestions are deliberately independent of library filters.
/// Preserve season numbers and suffixes: a shared franchise is not a duplicate.
fn possible_library_duplicates(
    entries: &[LibraryEntry],
    id: &str,
    type_: &str,
    name: &str,
) -> Vec<LibraryEntry> {
    let normalized = |value: &str| {
        value
            .chars()
            .flat_map(char::to_lowercase)
            .filter(|c| c.is_alphanumeric())
            .collect::<String>()
    };
    let title = normalized(name);
    if title.is_empty() {
        return Vec::new();
    }
    entries
        .iter()
        .filter(|entry| entry.id != id && entry.type_ == type_ && normalized(&entry.name) == title)
        .cloned()
        .collect()
}

/// Reuse confirmed episode aliases before comparing unique real titles.
/// Episode numbers alone are unsafe across split seasons/cours.
fn duplicate_progress_copies(
    old: &LibraryEntry,
    new: &ModalItem,
    old_videos: &[Video],
    progress: &HashMap<String, EpisodeProgress>,
) -> (Vec<EpisodeProgress>, usize) {
    let title = |value: &str| {
        value
            .chars()
            .flat_map(char::to_lowercase)
            .filter(|c| c.is_alphanumeric())
            .collect::<String>()
    };
    let meaningful = |video: &Video| {
        let name = title(&video.label());
        let mut remainder = name.as_str();
        while let Some(rest) = ["episode", "ep", "stage", "turn"]
            .iter()
            .find_map(|prefix| remainder.strip_prefix(prefix))
        {
            let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
            if digits == 0 {
                break;
            }
            remainder = &rest[digits..];
        }
        let generic = remainder.is_empty() || name.chars().all(|c| c.is_ascii_digit());
        (!remainder.is_empty()
            && !generic
            && !["unknown", "untitled", "tba", "tbd"].contains(&remainder))
        .then(|| remainder.to_string())
    };
    let start_year = |year: &str| {
        year.get(..4)
            .filter(|value| value.bytes().all(|c| c.is_ascii_digit()))
            .and_then(|value| value.parse::<u32>().ok())
    };
    let compatible_years = old.year.is_empty()
        || new.year.is_empty()
        || start_year(&old.year).is_none()
        || start_year(&new.year).is_none()
        || start_year(&old.year) == start_year(&new.year);
    let identities = |video: &Video| {
        std::iter::once(video.id.clone())
            .chain(
                confirmed_episode_stream_ids(video)
                    .into_iter()
                    .filter(|id| public_stream_id(id)),
            )
            .collect::<HashSet<_>>()
    };
    let new_identities = new.videos.iter().map(identities).collect::<Vec<_>>();
    let mut copies = Vec::new();
    let mut unmatched = 0;
    let mut used = HashSet::new();
    for record in progress.values().filter(|p| p.series_id == old.id) {
        let old_video = old_videos
            .iter()
            .find(|video| video.id == record.episode_id);
        let old_ids = old_video
            .map(identities)
            .unwrap_or_else(|| HashSet::from([record.episode_id.clone()]));
        let confirmed = new
            .videos
            .iter()
            .zip(&new_identities)
            .filter(|(_, ids)| !old_ids.is_disjoint(ids))
            .map(|(video, _)| video)
            .collect::<Vec<_>>();
        let matches = if !confirmed.is_empty() {
            confirmed
        } else {
            old_video
                .and_then(meaningful)
                .filter(|key| {
                    old_videos
                        .iter()
                        .filter(|v| meaningful(v).as_ref() == Some(key))
                        .count()
                        == 1
                })
                .filter(|_| compatible_years)
                .map(|key| {
                    new.videos
                        .iter()
                        .zip(&new_identities)
                        .filter(|(video, ids)| {
                            // A matching caption must not overrule a confirmed
                            // different canonical episode (e.g. another season).
                            meaningful(video).as_ref() == Some(&key)
                                && (!old_ids.iter().any(|id| public_stream_id(id))
                                    || !ids.iter().any(|id| public_stream_id(id)))
                        })
                        .map(|(video, _)| video)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        if matches.len() != 1
            || new.videos.iter().filter(|v| v.id == matches[0].id).count() != 1
            || !used.insert(matches[0].id.clone())
        {
            unmatched += 1;
            continue;
        }
        let mut copy = record.clone();
        copy.series_id = new.id.clone();
        copy.episode_id = matches[0].id.clone();
        copies.push(copy);
    }
    (copies, unmatched)
}

#[cfg(test)]
mod duplicate_tests {
    use super::*;
    fn entry(id: &str, name: &str) -> LibraryEntry {
        serde_json::from_value(serde_json::json!({"id":id,"type_":"series","name":name,"year":"2026","categories":["Favorites"],"added_at_secs":123})).unwrap()
    }
    fn video(id: &str, name: &str) -> Video {
        Video {
            id: id.into(),
            name: name.into(),
            season: Some(1),
            episode: Some(1),
            ..Default::default()
        }
    }
    fn modal(videos: Vec<Video>) -> ModalItem {
        ModalItem {
            details: Default::default(),
            open_token: Arc::new(()),
            pending_watch_now: None,
            episodes_loading: false,
            id: "new".into(),
            type_: "series".into(),
            request_id: "new".into(),
            videos,
            season_backdrops: HashMap::new(),
            seasons: vec![1],
            season_index: 0,
            episode_page: 0,
            name: "Example".into(),
            year: "2026".into(),
            poster_url: String::new(),
            background_url: String::new(),
            logo_url: String::new(),
            description: String::new(),
            genres: vec![],
        }
    }
    #[test]
    fn duplicate_suggestions_preserve_seasons_media_types_and_same_id_behavior() {
        let mut movie = entry("movie", "EXAMPLE!");
        movie.type_ = "movie".into();
        let entries = vec![
            entry("saved", "EXAMPLE!"),
            entry("new", "Example"),
            entry("later", "Example Season 2"),
            movie,
        ];
        let found = possible_library_duplicates(&entries, "new", "series", "Example");
        assert_eq!(
            found.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["saved"]
        );
        assert!(possible_library_duplicates(&entries, "x", "series", "!!!").is_empty());
    }
    #[test]
    fn confirmed_episode_aliases_move_only_the_destination_season_without_old_cache() {
        let mut old = entry("tt9999999", "The Apothecary Diaries");
        old.year = "2023–2025".into();
        let videos = (1..=24)
            .map(|n| {
                let mut v = video(&format!("anikoto:episode:{n}"), &format!("Episode {n}"));
                v.extra.insert(
                    "novaStreamIds".into(),
                    serde_json::json!([format!("tt9999999:1:{n}")]),
                );
                v
            })
            .collect::<Vec<_>>();
        let mut new = modal(videos);
        new.year = "2023".into();
        let progress = (1..=2)
            .flat_map(|season| {
                (1..=24).map(move |n| {
                    let id = format!("tt9999999:{season}:{n}");
                    (
                        progress_map_key("tt9999999", &id),
                        EpisodeProgress {
                            series_id: "tt9999999".into(),
                            episode_id: id,
                            watched: true,
                            position_secs: 1400.0,
                            duration_secs: 1440.0,
                            ..Default::default()
                        },
                    )
                })
            })
            .collect::<HashMap<_, _>>();
        let (copies, unmatched) = duplicate_progress_copies(&old, &new, &[], &progress);
        assert_eq!((copies.len(), unmatched), (24, 24));
        assert!(
            copies
                .iter()
                .all(|p| p.watched && p.series_id == "new" && p.episode_id.starts_with("anikoto:"))
        );
        // Duplicate claims and obsolete inventory aliases are not evidence.
        new.videos.push(new.videos[0].clone());
        assert_eq!(
            duplicate_progress_copies(&old, &new, &[], &progress)
                .0
                .len(),
            23
        );
        for video in &mut new.videos {
            video.extra.insert(
                "novaMetadataRevision".into(),
                serde_json::json!(nova_providers::metadata_revision().wrapping_add(1)),
            );
        }
        assert_eq!(duplicate_progress_copies(&old, &new, &[], &progress).1, 48);
    }

    #[test]
    fn year_ranges_allow_real_title_matches_but_canonical_conflicts_do_not() {
        let mut old = entry("old", "Example");
        old.year = "2023–2025".into();
        let old_video = video("tt9999999:2:1", "A New Beginning");
        let mut new = modal(vec![video("private-new-episode", "A New Beginning")]);
        new.year = "2023".into();
        let progress = HashMap::from([(
            progress_map_key("old", &old_video.id),
            EpisodeProgress {
                series_id: "old".into(),
                episode_id: old_video.id.clone(),
                watched: true,
                ..Default::default()
            },
        )]);
        assert_eq!(
            duplicate_progress_copies(&old, &new, std::slice::from_ref(&old_video), &progress)
                .0
                .len(),
            1
        );
        new.videos[0]
            .extra
            .insert("novaStreamIds".into(), serde_json::json!(["tt9999999:1:1"]));
        assert_eq!(
            duplicate_progress_copies(&old, &new, std::slice::from_ref(&old_video), &progress).1,
            1
        );
        // Confirmed mappings work in the reverse direction too.
        let mut old_native = video("private-old-episode", "Episode 1");
        old_native
            .extra
            .insert("novaStreamIds".into(), serde_json::json!(["tt9999999:1:1"]));
        new.videos = vec![video("tt9999999:1:1", "Episode 1")];
        let progress = HashMap::from([(
            progress_map_key("old", &old_native.id),
            EpisodeProgress {
                series_id: "old".into(),
                episode_id: old_native.id.clone(),
                watched: true,
                ..Default::default()
            },
        )]);
        assert_eq!(
            duplicate_progress_copies(&old, &new, &[old_native], &progress)
                .0
                .len(),
            1
        );
    }

    #[test]
    fn migration_only_copies_unique_real_titles_or_shared_ids_and_keeps_original_history() {
        let old = entry("old", "Example");
        let videos = vec![
            video("old-1", "Episode 1: A New Beginning"),
            video("old-2", "Episode 2: Episode 2"),
            video("shared", "Episode 3"),
            video("old-4", "Repeated Title"),
            video("old-5", "Missing"),
        ];
        let new = modal(vec![
            video("new-1", "A New Beginning"),
            video("new-2", "Episode 2"),
            video("shared", "Episode 3"),
            video("repeat-a", "Repeated Title"),
            video("repeat-b", "Repeated Title"),
        ]);
        let progress = videos
            .iter()
            .map(|v| {
                (
                    progress_map_key("old", &v.id),
                    EpisodeProgress {
                        series_id: "old".into(),
                        episode_id: v.id.clone(),
                        watched: true,
                        position_secs: 900.0,
                        duration_secs: 1200.0,
                        play_count: 3,
                        updated_at_secs: 123,
                        ..Default::default()
                    },
                )
            })
            .collect::<HashMap<_, _>>();
        let (copies, unmatched) = duplicate_progress_copies(&old, &new, &videos, &progress);
        assert_eq!(unmatched, 3);
        assert_eq!(copies.len(), 2);
        assert!(copies.iter().any(|p| p.episode_id == "new-1"));
        assert!(copies.iter().any(|p| p.episode_id == "shared"));
        assert!(copies.iter().all(|p| p.series_id == "new"
            && p.watched
            && p.position_secs == 900.0
            && p.play_count == 3));
        assert_eq!(progress.len(), 5);
        let mut remake = new;
        remake.year = "2030".into();
        assert_eq!(
            duplicate_progress_copies(&old, &remake, &videos, &progress)
                .0
                .len(),
            1
        );
        let invalid_ids = modal(vec![
            video("new-1", "A New Beginning"),
            video("new-1", "Different title"),
        ]);
        assert!(
            duplicate_progress_copies(&old, &invalid_ids, &videos, &progress)
                .0
                .is_empty()
        );
        let incomplete = modal(vec![]);
        assert_eq!(
            duplicate_progress_copies(&old, &incomplete, &videos, &progress).1,
            5
        );
    }
}

#[cfg(test)]
mod imported_status_projection_tests {
    use super::*;
    #[test]
    fn cold_tracker_status_matches_the_library_filter_and_visible_card() {
        let mut entry: LibraryEntry = serde_json::from_value(serde_json::json!({"id":"mal:34","type_":"series","name":"Imported","imported_tracking_status":"completed"})).unwrap();
        let progress = HashMap::new();
        let activity = HashMap::new();
        for (remote, label) in [
            (nova_tracking::ListStatus::Planning, "Plan to Watch"),
            (nova_tracking::ListStatus::Watching, "Watching"),
            (nova_tracking::ListStatus::Completed, "Completed"),
            (nova_tracking::ListStatus::OnHold, "On Hold"),
            (nova_tracking::ListStatus::Dropped, "Dropped"),
        ] {
            entry.imported_tracking_status = Some(remote);
            let options = LibraryViewOptions {
                filter: label,
                needle: String::new(),
                sort: 0,
            };
            let candidate =
                project_library_candidate(&entry, 0, None, &progress, &activity, &options).unwrap();
            let card = project_library_card(&entry, &candidate, &[], &progress, &activity).unwrap();
            assert_eq!(card.status, label);
            assert!(
                project_library_candidate(
                    &entry,
                    0,
                    None,
                    &progress,
                    &activity,
                    &LibraryViewOptions {
                        filter: "Watching",
                        needle: String::new(),
                        sort: 0
                    }
                )
                .is_some()
                    == (label == "Watching")
            );
        }
    }
}
