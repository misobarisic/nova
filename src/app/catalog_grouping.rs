//! Catalog display projections. Native media identities never leave their source.
use super::*;
use nova_config::{CatalogGroupingKey, CatalogGroupingMode, CatalogSurface};
use nova_providers::catalog_grouping::{
    self as grouping, DisplayGroup, GroupingLookupStatus, OwnedCatalogEntry, SeasonMembership,
    SourceIdentity, projection_fingerprint,
};
use std::collections::BTreeMap;

const DISPLAY_KEY: &str = "novaCatalogDisplayKey";
const DISPLAY_SEASON: &str = "novaCatalogDisplaySeason";
const VIRTUAL_SEASON: &str = "novaCatalogVirtualSeason";
const LOOKUPS_PER_VIEW: usize = 12;
const LOOKUPS_IN_FLIGHT: usize = 2;
const MAX_SCOPES: usize = 96;

#[derive(Clone)]
struct Scope {
    key: CatalogGroupingKey,
    mode: CatalogGroupingMode,
    infer_series: bool,
    fingerprint: String,
    revision: u64,
    profile: profiles::Context,
    generation: u64,
    order: u64,
    sources: HashSet<SourceIdentity>,
    home_sources: Vec<HomeCatalogSource>,
}

#[derive(Clone)]
struct Lookup {
    scope: String,
    source: SourceIdentity,
    stamp: Scope,
}

#[derive(Clone)]
struct RegisteredGroup {
    scope: String,
    group: DisplayGroup,
}

#[derive(Clone)]
struct OpenContext {
    group_key: String,
    generation: u64,
    discover_index: Option<usize>,
    watch_now: bool,
    profile: profiles::Context,
}

#[derive(Default)]
pub(super) struct State {
    pub(super) home_featured_raw: Option<home::HomeShowcaseCache>,
    pub(super) home_rows_raw: Option<home::HomeShowcaseCache>,
    scopes: BTreeMap<String, Scope>,
    groups: HashMap<String, RegisteredGroup>,
    queue: VecDeque<Lookup>,
    active: usize,
    attempted: HashSet<(String, SourceIdentity)>,
    budgets: HashMap<String, usize>,
    sequence: u64,
    policy: String,
    settings_keys: HashMap<String, CatalogGroupingKey>,
    query: String,
    chooser: Option<OpenContext>,
    chooser_members: HashMap<String, OwnedCatalogEntry>,
}

#[derive(Clone)]
pub(super) struct DisplayContext {
    pub members: Vec<SourceIdentity>,
    pub canonical_season: Option<u32>,
}

pub(super) fn display_key(preview: &MetaPreview) -> String {
    preview
        .extra
        .get(DISPLAY_KEY)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| {
            projection_fingerprint(&(
                preview.extra.get("novaSourceUrl"),
                &preview.type_,
                &preview.id,
            ))
        })
}

pub(super) fn canonical_season(preview: &MetaPreview) -> Option<u32> {
    preview
        .extra
        .get(DISPLAY_SEASON)
        .and_then(serde_json::Value::as_u64)
        .and_then(|season| u32::try_from(season).ok())
}

pub(super) fn virtual_season(preview: &MetaPreview) -> Option<u32> {
    preview
        .extra
        .get(VIRTUAL_SEASON)
        .and_then(serde_json::Value::as_bool)
        .filter(|value| *value)
        .and_then(|_| canonical_season(preview))
}

fn native_preview(preview: &MetaPreview, owner: &str) -> MetaPreview {
    let mut preview = preview.clone();
    // Catalog JSON cannot grant itself a registered display action.
    preview.extra.remove(DISPLAY_KEY);
    preview.extra.remove(DISPLAY_SEASON);
    preview.extra.remove(VIRTUAL_SEASON);
    preview.extra.insert("novaSourceUrl".into(), owner.into());
    preview
}

pub(super) fn raw_fingerprint(raw: &[MetaPreview]) -> String {
    projection_fingerprint(
        &raw.iter()
            .map(|preview| {
                (
                    &preview.type_,
                    &preview.id,
                    &preview.poster,
                    preview.extra.get("novaCatalogGrouping"),
                )
            })
            .collect::<Vec<_>>(),
    )
}

fn scope_key(key: &CatalogGroupingKey, fingerprint: &str) -> String {
    projection_fingerprint(&(key, fingerprint))
}

fn profile_stamp() -> profiles::Context {
    let context = profiles::capture();
    // Shared belongs to ProfileSession; storing that Arc in its own state
    // would retain the entire outgoing profile after a switch.
    profiles::Context {
        id: context.id,
        epoch: context.epoch,
        session: None,
    }
}

fn unavailable_catalog_label(id: &str) -> String {
    if id.trim().is_empty()
        || id.len() > 96
        || id
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '?' | '#' | '@'))
    {
        text::tr("Unavailable catalog").into()
    } else {
        id.to_owned()
    }
}

fn safe_addon_labels(installed: &[Installed]) -> HashMap<String, String> {
    let mut names = BTreeMap::<String, Vec<&Installed>>::new();
    for addon in installed {
        let name = addon.manifest.name.trim();
        let name = if name.is_empty()
            || name.len() > 160
            || name.contains("://")
            || name.chars().any(char::is_control)
        {
            text::tr("Addons")
        } else {
            name
        };
        names.entry(name.into()).or_default().push(addon);
    }
    let mut labels = HashMap::new();
    for (name, mut addons) in names {
        addons.sort_by(|left, right| left.url.cmp(&right.url));
        for (index, addon) in addons.iter().enumerate() {
            // Addon picker labels can contain the full configured URL to
            // disambiguate installations. These surfaces use a safe ordinal.
            let label = if addons.len() == 1 {
                name.clone()
            } else {
                format!("{name} ({})", index + 1)
            };
            labels.insert(addon.url.clone(), label);
        }
    }
    labels
}

fn release_lookup_slot(state: &mut State, profile: &profiles::Context) -> bool {
    state.active = state.active.saturating_sub(1);
    // Obsolete catalog jobs still release capacity for this profile's new
    // queue. Only their result publication is conditional on the old scope.
    profiles::is_current(profile)
}

fn view_budget(scope: &Scope) -> String {
    projection_fingerprint(&(
        &scope.profile.id,
        scope.profile.epoch,
        &scope.key,
        scope.mode,
        scope.infer_series,
        scope.revision,
        scope.generation,
    ))
}

fn scope_is_current(state: &Shared, id: &str, stamp: &Scope, catalog_generation: u64) -> bool {
    profiles::is_current(&stamp.profile)
        && stamp.revision == nova_providers::metadata_revision()
        && state.cache_settings.catalog_grouping_mode(&stamp.key) == stamp.mode
        && (state.cache_settings.catalog_grouping_infer_series
            && stamp.mode == CatalogGroupingMode::Series)
            == stamp.infer_series
        && (stamp.key.surface != CatalogSurface::Discover || stamp.generation == catalog_generation)
        && (stamp.key.surface != CatalogSurface::Home || home_scope_is_live(state, id, stamp))
        && state.catalog_grouping.scopes.get(id).is_some_and(|scope| {
            scope.fingerprint == stamp.fingerprint
                && scope.mode == stamp.mode
                && scope.infer_series == stamp.infer_series
                && scope.revision == stamp.revision
                && scope.generation == stamp.generation
                && scope.profile == stamp.profile
        })
}

fn home_scope_is_live(state: &Shared, id: &str, scope: &Scope) -> bool {
    if !home::catalog_grouping_sources_selected(state, &scope.key, &scope.home_sources) {
        return false;
    }
    if !home::catalog_grouping_context_sources(state, &scope.key, &scope.fingerprint).is_empty() {
        return true;
    }
    // Featured refreshes deliberately retain the outgoing slide until its
    // replacement is ready. Its current or staged projection stays usable.
    state
        .home_showcase
        .iter()
        .chain(state.home_showcase_refresh.iter().flatten())
        .chain(state.home_showcase_displayed.iter())
        .chain(state.home_catalog_row_items.iter())
        .filter_map(|preview| {
            preview
                .extra
                .get(DISPLAY_KEY)
                .and_then(serde_json::Value::as_str)
        })
        .any(|marker| {
            state
                .catalog_grouping
                .groups
                .get(marker)
                .is_some_and(|group| group.scope == id)
        })
}

impl State {
    fn remove_scope(&mut self, id: &str) {
        self.scopes.remove(id);
        self.groups.retain(|_, registered| registered.scope != id);
        self.queue.retain(|lookup| lookup.scope != id);
        self.attempted.retain(|(scope, _)| scope != id);
    }

    fn trim(&mut self) {
        while self.scopes.len() > MAX_SCOPES {
            let Some(oldest) = self
                .scopes
                .iter()
                .min_by_key(|(_, scope)| scope.order)
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            self.remove_scope(&oldest);
        }
        // Budgets contain only opaque identifiers, but should not grow during
        // an endless browsing session either.
        let live = self
            .scopes
            .values()
            .map(view_budget)
            .collect::<HashSet<_>>();
        self.budgets.retain(|key, _| live.contains(key));
    }
}

impl Bridge {
    /// Pure provider projection plus a small action registry. This method is
    /// called outside Shared locks and never performs metadata/network I/O.
    pub(super) fn project_catalog_grouping(
        &self,
        key: &CatalogGroupingKey,
        raw: &[MetaPreview],
    ) -> Vec<MetaPreview> {
        let (mode, infer_series) = {
            let state = self.shared.lock().unwrap();
            let mode = state.cache_settings.catalog_grouping_mode(key);
            (
                mode,
                state.cache_settings.catalog_grouping_infer_series
                    && mode == CatalogGroupingMode::Series,
            )
        };
        let entries = raw
            .iter()
            .map(|preview| native_preview(preview, &key.addon_url))
            .collect::<Vec<_>>();
        if mode == CatalogGroupingMode::Original {
            return entries;
        }
        let revision = nova_providers::metadata_revision();
        let profile = profile_stamp();
        let fingerprint = raw_fingerprint(raw);
        let scope_id = scope_key(key, &fingerprint);
        let owned = entries
            .into_iter()
            .map(|preview| {
                let mut entry = OwnedCatalogEntry::new(&key.addon_url, preview);
                let invalid_explicit = entry.preview.extra.contains_key("novaCatalogGrouping")
                    && entry.evidence.is_none();
                let cached = (!invalid_explicit)
                    .then(|| {
                        grouping::cached_evidence_with_inference(
                            &entry.source,
                            revision,
                            infer_series,
                        )
                        .flatten()
                    })
                    .flatten();
                entry.evidence = grouping::merge_grouping_evidence(entry.evidence, cached);
                entry
            })
            .collect::<Vec<_>>();
        let groups = grouping::project_catalog(&owned, mode);
        let evidence_stamps = owned
            .iter()
            .filter_map(|entry| {
                entry
                    .evidence
                    .as_ref()
                    .map(|evidence| (entry.source.clone(), evidence.fingerprint()))
            })
            .collect::<HashMap<_, _>>();
        let mut visible = Vec::with_capacity(groups.len());
        let mut registered = Vec::with_capacity(groups.len());
        for group in groups {
            let marker = projection_fingerprint(&(
                key,
                &group.key,
                group
                    .members
                    .iter()
                    .map(|member| (&member.source, evidence_stamps.get(&member.source)))
                    .collect::<Vec<_>>(),
            ));
            let mut preview = group.representative.preview.clone();
            preview
                .extra
                .insert(DISPLAY_KEY.into(), marker.clone().into());
            if let Some(season) = group.canonical_season {
                preview.extra.insert(DISPLAY_SEASON.into(), season.into());
                if group
                    .representative
                    .evidence
                    .as_ref()
                    .is_some_and(|evidence| evidence.season == SeasonMembership::Multiple)
                {
                    preview.extra.insert(VIRTUAL_SEASON.into(), true.into());
                }
                // Display text is a projection, never the stored/native title.
                preview.name = format!("{} · {} {season}", preview.title(), text::tr("Season"));
                preview.title_legacy = None;
            }
            visible.push(preview);
            registered.push((
                marker,
                RegisteredGroup {
                    scope: scope_id.clone(),
                    group,
                },
            ));
        }
        let mut state = self.shared.lock().unwrap();
        if !profiles::is_current(&profile)
            || revision != nova_providers::metadata_revision()
            || state.cache_settings.catalog_grouping_mode(key) != mode
            || (state.cache_settings.catalog_grouping_infer_series
                && mode == CatalogGroupingMode::Series)
                != infer_series
        {
            return raw
                .iter()
                .map(|preview| native_preview(preview, &key.addon_url))
                .collect();
        }
        let generation = match key.surface {
            CatalogSurface::Discover => self.catalog_gen.load(Ordering::Relaxed),
            CatalogSurface::Home => state.catalog_grouping.sequence,
        };
        if key.surface == CatalogSurface::Discover {
            let obsolete = state
                .catalog_grouping
                .scopes
                .iter()
                .filter(|(id, scope)| {
                    scope.key.surface == CatalogSurface::Discover && *id != &scope_id
                })
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();
            for id in obsolete {
                state.catalog_grouping.remove_scope(&id);
            }
        }
        state.catalog_grouping.sequence = state.catalog_grouping.sequence.wrapping_add(1);
        let order = state.catalog_grouping.sequence;
        // Keep the generation of the same Home raw batch stable while cached
        // evidence arrives. This also preserves its bounded lookup budget.
        let generation = state
            .catalog_grouping
            .scopes
            .get(&scope_id)
            .filter(|scope| {
                key.surface == CatalogSurface::Home
                    && scope.mode == mode
                    && scope.infer_series == infer_series
                    && scope.revision == revision
                    && scope.profile == profile
            })
            .map_or(generation, |scope| scope.generation);
        state
            .catalog_grouping
            .groups
            .retain(|_, group| group.scope != scope_id);
        let home_sources = if key.surface == CatalogSurface::Home {
            home::catalog_grouping_context_sources(&state, key, &fingerprint)
        } else {
            Vec::new()
        };
        state.catalog_grouping.scopes.insert(
            scope_id.clone(),
            Scope {
                key: key.clone(),
                mode,
                infer_series,
                fingerprint,
                revision,
                profile,
                generation,
                order,
                sources: owned.iter().map(|entry| entry.source.clone()).collect(),
                home_sources,
            },
        );
        for (marker, group) in registered {
            state.catalog_grouping.groups.insert(marker, group);
        }
        let obsolete_home = state
            .catalog_grouping
            .scopes
            .iter()
            .filter(|(id, scope)| {
                *id != &scope_id
                    && scope.key.surface == CatalogSurface::Home
                    && !home_scope_is_live(&state, id, scope)
            })
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in obsolete_home {
            state.catalog_grouping.remove_scope(&id);
        }
        state.catalog_grouping.trim();
        let close_chooser = state
            .catalog_grouping
            .chooser
            .as_ref()
            .is_some_and(|chooser| {
                !state
                    .catalog_grouping
                    .groups
                    .contains_key(&chooser.group_key)
            });
        if close_chooser {
            state.catalog_grouping.chooser = None;
            state.catalog_grouping.chooser_members.clear();
        }
        drop(state);
        if close_chooser && let Some(app) = self.app() {
            app.set_catalog_grouping_chooser_open(false);
        }
        visible
    }

    pub(super) fn catalog_grouping_display_context(
        &self,
        preview: &MetaPreview,
    ) -> Option<DisplayContext> {
        let state = self.shared.lock().unwrap();
        let registered = state.catalog_grouping.groups.get(&display_key(preview))?;
        Some(DisplayContext {
            members: registered
                .group
                .members
                .iter()
                .map(|member| member.source.clone())
                .collect(),
            canonical_season: registered.group.canonical_season,
        })
    }

    /// The caller supplies a small visible candidate window, not a whole
    /// catalog. Both the queue and per-view attempts have independent bounds.
    pub(super) fn schedule_catalog_grouping(&self, key: CatalogGroupingKey, raw: &[MetaPreview]) {
        let (mode, infer_series) = {
            let state = self.shared.lock().unwrap();
            let mode = state.cache_settings.catalog_grouping_mode(&key);
            (
                mode,
                state.cache_settings.catalog_grouping_infer_series
                    && mode == CatalogGroupingMode::Series,
            )
        };
        if mode == CatalogGroupingMode::Original {
            return;
        }
        let revision = nova_providers::metadata_revision();
        let sources = raw
            .iter()
            .filter_map(|preview| {
                let source = SourceIdentity {
                    addon_url: key.addon_url.clone(),
                    media_type: preview.type_.clone(),
                    id: preview.id.clone(),
                };
                let explicit = grouping::evidence_from_preview(&source, preview, &key.addon_url);
                if preview.extra.contains_key("novaCatalogGrouping") && explicit.is_none() {
                    return None;
                }
                if grouping::cached_evidence_with_inference(&source, revision, infer_series)
                    .is_some()
                {
                    return None;
                }
                if explicit.as_ref().is_some_and(|evidence| {
                    mode == CatalogGroupingMode::Series
                        || matches!(evidence.season, SeasonMembership::Season(_))
                }) {
                    return None;
                }
                Some(source)
            })
            .take(LOOKUPS_PER_VIEW)
            .collect::<Vec<_>>();
        if sources.is_empty() {
            return;
        }
        {
            let mut state = self.shared.lock().unwrap();
            let Some((id, stamp)) = state
                .catalog_grouping
                .scopes
                .iter()
                .filter(|(_, scope)| {
                    scope.key == key
                        && scope.mode == mode
                        && scope.infer_series == infer_series
                        && scope.revision == revision
                        && sources.iter().all(|source| scope.sources.contains(source))
                })
                .max_by_key(|(_, scope)| scope.order)
                .map(|(id, scope)| (id.clone(), scope.clone()))
            else {
                return;
            };
            let budget = view_budget(&stamp);
            for source in sources {
                if state.catalog_grouping.queue.len() >= LOOKUPS_PER_VIEW * 2
                    || state
                        .catalog_grouping
                        .budgets
                        .get(&budget)
                        .copied()
                        .unwrap_or(0)
                        >= LOOKUPS_PER_VIEW
                {
                    break;
                }
                if !matches!(source.media_type.as_str(), "series" | "anime")
                    || grouping::cached_evidence_with_inference(&source, revision, infer_series)
                        .is_some()
                    || state
                        .catalog_grouping
                        .attempted
                        .contains(&(id.clone(), source.clone()))
                {
                    continue;
                }
                state
                    .catalog_grouping
                    .attempted
                    .insert((id.clone(), source.clone()));
                *state
                    .catalog_grouping
                    .budgets
                    .entry(budget.clone())
                    .or_default() += 1;
                state.catalog_grouping.queue.push_back(Lookup {
                    scope: id.clone(),
                    source,
                    stamp: stamp.clone(),
                });
            }
        }
        self.pump_catalog_grouping();
    }

    fn pump_catalog_grouping(&self) {
        loop {
            let job = {
                let mut state = self.shared.lock().unwrap();
                if state.catalog_grouping.active >= LOOKUPS_IN_FLIGHT {
                    return;
                }
                let Some(job) = state.catalog_grouping.queue.pop_front() else {
                    return;
                };
                if !scope_is_current(
                    &state,
                    &job.scope,
                    &job.stamp,
                    self.catalog_gen.load(Ordering::Relaxed),
                ) {
                    continue;
                }
                state.catalog_grouping.active += 1;
                job
            };
            let bridge = self.clone();
            thread::spawn(move || {
                let result = grouping::resolve_catalog_grouping_evidence_with_inference(
                    &job.source,
                    job.stamp.mode,
                    job.stamp.revision,
                    job.stamp.infer_series,
                );
                // Release the slot in the captured profile's session, before
                // entering the UI. An account-scoped callback here could
                // otherwise decrement a replacement profile's active count.
                let profile_current = {
                    let mut state = bridge.shared.lock().unwrap();
                    release_lookup_slot(&mut state.catalog_grouping, &job.stamp.profile)
                };
                if !profile_current {
                    return;
                }
                let _ = profiles::invoke_from_event_loop(move || {
                    let current = scope_is_current(
                        &bridge.shared.lock().unwrap(),
                        &job.scope,
                        &job.stamp,
                        bridge.catalog_gen.load(Ordering::Relaxed),
                    );
                    if current
                        && matches!(
                            result.status,
                            GroupingLookupStatus::Confirmed | GroupingLookupStatus::Inferred
                        )
                    {
                        match job.stamp.key.surface {
                            CatalogSurface::Discover => {
                                bridge.reproject_catalog_grouping_discover()
                            }
                            CatalogSurface::Home => bridge.reproject_home_catalog_grouping(),
                        }
                    }
                    bridge.pump_catalog_grouping();
                });
            });
        }
    }

    /// A marker is valid only for the original native representative and its
    /// still-current projection. Ordinary previews take the normal Detail path.
    pub(super) fn open_catalog_group(
        &self,
        preview: &MetaPreview,
        generation: u64,
        discover_index: Option<usize>,
        watch_now: bool,
    ) -> bool {
        let marker = display_key(preview);
        let (group, infer_series) = {
            let state = self.shared.lock().unwrap();
            let Some(registered) = state.catalog_grouping.groups.get(&marker) else {
                return preview.extra.contains_key(DISPLAY_KEY);
            };
            let Some(scope) = state.catalog_grouping.scopes.get(&registered.scope) else {
                return true;
            };
            if !scope_is_current(
                &state,
                &registered.scope,
                scope,
                self.catalog_gen.load(Ordering::Relaxed),
            ) || registered.group.representative.preview.id != preview.id
                || registered.group.representative.preview.type_ != preview.type_
            {
                return true;
            }
            (registered.group.clone(), scope.infer_series)
        };
        // Prefer a parent which the catalog actually supplied and whose exact
        // owner advertises a compatible Detail endpoint. Broad ID support in
        // another addon is not proof that an invented parent preview exists.
        if group.canonical_season.is_none() && group.members.len() > 1 {
            let installed = self.shared.lock().unwrap().installed.clone();
            let parent = group.members.iter().find(|member| {
                member.source.media_type == "series"
                    && group.proven_mapping.iter().any(|proof| {
                        proof.parents.iter().any(|parent| {
                            matches!(parent,
                        grouping::ParentIdentity::ImdbSeries(id) if id == &member.source.id)
                        })
                    })
                    && installed.iter().any(|addon| {
                        addon.url == member.source.addon_url
                            && addon.enabled
                            && addon.available
                            && addon.manifest.accepts("meta", "series", &member.source.id)
                    })
            });
            if let Some(member) = parent {
                self.open_catalog_detail(
                    member.preview.clone(),
                    member.source.clone(),
                    None,
                    generation,
                    discover_index,
                    watch_now,
                );
                return true;
            }
        }
        // An inferred relation may retain a real canonical preview returned by
        // the same owner. Offer it alongside the native release rather than
        // inventing a parent or changing the release's episode identity.
        let inferred_parent = if group.canonical_season.is_none() && infer_series {
            let installed = self.shared.lock().unwrap().installed.clone();
            group
                .proven_mapping
                .iter()
                .filter_map(|proof| grouping::inferred_parent_entry(proof))
                .find(|parent| {
                    !group
                        .members
                        .iter()
                        .any(|member| member.source == parent.source)
                        && installed.iter().any(|addon| {
                            addon.url == parent.source.addon_url
                                && addon.enabled
                                && addon.available
                                && addon.manifest.accepts(
                                    "meta",
                                    &parent.source.media_type,
                                    &parent.source.id,
                                )
                        })
                })
        } else {
            None
        };
        if group.members.len() == 1 && inferred_parent.is_none() {
            let member = &group.members[0];
            let selection = group.season_selection(&member.source);
            if selection.is_none() {
                return false;
            }
            self.open_catalog_detail(
                member.preview.clone(),
                member.source.clone(),
                selection,
                generation,
                discover_index,
                watch_now,
            );
            return true;
        }
        let context = OpenContext {
            group_key: marker,
            generation,
            discover_index,
            watch_now,
            profile: profile_stamp(),
        };
        let mut members = HashMap::new();
        let installed = self.shared.lock().unwrap().installed.clone();
        let safe_labels = safe_addon_labels(&installed);
        let parent_source = inferred_parent.as_ref().map(|parent| parent.source.clone());
        let choices = inferred_parent
            .into_iter()
            .chain(group.members.iter().cloned());
        let rows = choices
            .enumerate()
            .map(|(index, member)| {
                let key = projection_fingerprint(&(&context.group_key, &member.source, index));
                members.insert(key.clone(), member.clone());
                let addon = safe_labels
                    .get(&member.source.addon_url)
                    .map(String::as_str)
                    .unwrap_or_else(|| text::tr("Unavailable"));
                let season = group.canonical_season.or_else(|| {
                    member
                        .evidence
                        .as_ref()
                        .and_then(|evidence| match evidence.season {
                            SeasonMembership::Season(season) => Some(season),
                            _ => None,
                        })
                });
                let mut subtitle = season
                    .map(|season| format!("{addon} · {} {season}", text::tr("Season")))
                    .unwrap_or_else(|| addon.to_owned());
                if parent_source.as_ref() == Some(&member.source) {
                    subtitle.push_str(&format!(" · {}", text::tr("All seasons")));
                } else if member
                    .evidence
                    .as_ref()
                    .is_some_and(grouping::GroupingEvidence::is_inferred)
                {
                    subtitle.push_str(&format!(" · {}", text::tr("Inferred grouping")));
                }
                crate::CatalogGroupMemberRow {
                    key: key.into(),
                    title: member.preview.title().into(),
                    subtitle: subtitle.into(),
                }
            })
            .collect::<Vec<_>>();
        {
            let mut state = self.shared.lock().unwrap();
            state.catalog_grouping.chooser = Some(context);
            state.catalog_grouping.chooser_members = members;
        }
        if let Some(app) = self.app() {
            app.set_catalog_grouping_chooser_title(group.representative.preview.title().into());
            app.set_catalog_grouping_chooser_members(Rc::new(VecModel::from(rows)).into());
            app.set_catalog_grouping_chooser_open(true);
        }
        true
    }

    pub(super) fn catalog_grouping_source_preview(&self, mut preview: MetaPreview) -> MetaPreview {
        let state = self.shared.lock().unwrap();
        if let Some(registered) = state.catalog_grouping.groups.get(&display_key(&preview)) {
            let member = &registered.group.representative;
            if member.preview.id == preview.id
                && member.preview.type_ == preview.type_
                && preview
                    .extra
                    .get("novaSourceUrl")
                    .and_then(serde_json::Value::as_str)
                    == Some(member.source.addon_url.as_str())
            {
                return member.preview.clone();
            }
        }
        preview.extra.remove(DISPLAY_KEY);
        preview.extra.remove(DISPLAY_SEASON);
        preview.extra.remove(VIRTUAL_SEASON);
        preview
    }

    fn close_catalog_grouping_chooser(&self) {
        let mut state = self.shared.lock().unwrap();
        state.catalog_grouping.chooser = None;
        state.catalog_grouping.chooser_members.clear();
        drop(state);
        if let Some(app) = self.app() {
            app.set_catalog_grouping_chooser_open(false);
        }
    }

    fn pick_catalog_group_member(&self, key: &str) {
        let selected = {
            let state = self.shared.lock().unwrap();
            let Some(context) = state.catalog_grouping.chooser.clone() else {
                return;
            };
            let Some(member) = state.catalog_grouping.chooser_members.get(key).cloned() else {
                return;
            };
            let Some(registered) = state.catalog_grouping.groups.get(&context.group_key) else {
                return;
            };
            let Some(scope) = state.catalog_grouping.scopes.get(&registered.scope) else {
                return;
            };
            if !profiles::is_current(&context.profile)
                || !scope_is_current(
                    &state,
                    &registered.scope,
                    scope,
                    self.catalog_gen.load(Ordering::Relaxed),
                )
            {
                return;
            }
            // The choice was captured from this exact registered projection,
            // including any owner-supplied inferred parent navigation hint.
            (
                context,
                member.preview.clone(),
                member.source.clone(),
                registered.group.season_selection(&member.source),
            )
        };
        self.close_catalog_grouping_chooser();
        let (context, preview, source, selection) = selected;
        self.open_catalog_detail(
            preview,
            source,
            selection,
            context.generation,
            context.discover_index,
            context.watch_now,
        );
    }

    pub(super) fn refresh_catalog_grouping_settings(&self) {
        let (settings, installed, query) = {
            let state = self.shared.lock().unwrap();
            (
                state.cache_settings.clone(),
                state.installed.clone(),
                state.catalog_grouping.query.clone(),
            )
        };
        let normalized_query = query.to_lowercase();
        let mut catalogs = BTreeMap::<(String, String, String), (String, String, bool)>::new();
        let safe_labels = safe_addon_labels(&installed);
        for addon in &installed {
            for catalog in &addon.manifest.catalogs {
                catalogs.insert(
                    (addon.url.clone(), catalog.type_.clone(), catalog.id.clone()),
                    (
                        catalog.name.clone(),
                        safe_labels.get(&addon.url).cloned().unwrap_or_default(),
                        addon.available,
                    ),
                );
            }
        }
        for row in &settings.catalog_grouping_overrides {
            catalogs
                .entry((
                    row.key.addon_url.clone(),
                    row.key.type_.clone(),
                    row.key.catalog_id.clone(),
                ))
                .or_insert_with(|| {
                    (
                        unavailable_catalog_label(&row.key.catalog_id),
                        text::tr("Unavailable").into(),
                        false,
                    )
                });
        }
        let mut keys = HashMap::new();
        let mut rows = Vec::new();
        for ((addon_url, type_, catalog_id), (title, addon, available)) in catalogs {
            let search = format!("{title} {addon} {type_}").to_lowercase();
            if !normalized_query.is_empty() && !search.contains(&normalized_query) {
                continue;
            }
            let key = CatalogGroupingKey {
                addon_url,
                type_: type_.clone(),
                catalog_id,
                surface: CatalogSurface::Home,
            };
            let opaque = projection_fingerprint(&key);
            let index = |surface| {
                settings
                    .catalog_grouping_overrides
                    .iter()
                    .rev()
                    .find(|row| {
                        row.key
                            == CatalogGroupingKey {
                                surface,
                                ..key.clone()
                            }
                    })
                    .map_or(0, |row| row.mode.index() + 1)
            };
            rows.push(crate::CatalogGroupingRow {
                key: opaque.clone().into(),
                title: title.into(),
                addon: addon.into(),
                media_type: match type_.as_str() {
                    "series" => text::tr("Series"),
                    "movie" => text::tr("Movie"),
                    "anime" => text::tr("Anime"),
                    _ => &type_,
                }
                .into(),
                available,
                home_mode: index(CatalogSurface::Home),
                discover_mode: index(CatalogSurface::Discover),
            });
            keys.insert(opaque, key);
        }
        rows.sort_by_key(|row| {
            (
                row.title.to_lowercase(),
                row.addon.to_lowercase(),
                row.key.to_string(),
            )
        });
        self.shared.lock().unwrap().catalog_grouping.settings_keys = keys;
        if let Some(app) = self.app() {
            app.set_catalog_grouping_default_index(settings.catalog_grouping_default.index());
            app.set_catalog_grouping_infer_series(settings.catalog_grouping_infer_series);
            app.set_catalog_grouping_search(query.into());
            app.set_catalog_grouping_rows(Rc::new(VecModel::from(rows)).into());
        }
    }

    fn change_catalog_grouping(&self, row: Option<(&str, i32)>, mode: i32, remove: bool) {
        if !writable_key("settings") {
            return;
        }
        if row.is_some() && !remove && !(-1..=2).contains(&mode) {
            return;
        }
        let field = if row.is_some() {
            "catalog_grouping_overrides"
        } else {
            "catalog_grouping_default"
        };
        let settings = {
            let mut state = self.shared.lock().unwrap();
            stream_filters::migrate_overrides(&mut state.cache_settings);
            if let Some((opaque, surface)) = row {
                let Some(mut key) = state.catalog_grouping.settings_keys.get(opaque).cloned()
                else {
                    return;
                };
                if remove {
                    state
                        .cache_settings
                        .catalog_grouping_overrides
                        .retain(|entry| {
                            entry.key.addon_url != key.addon_url
                                || entry.key.type_ != key.type_
                                || entry.key.catalog_id != key.catalog_id
                        });
                } else {
                    key.surface = if surface == 0 {
                        CatalogSurface::Home
                    } else if surface == 1 {
                        CatalogSurface::Discover
                    } else {
                        return;
                    };
                    state.cache_settings.set_catalog_grouping_override(
                        key,
                        (0..=2)
                            .contains(&mode)
                            .then(|| CatalogGroupingMode::from_index(mode)),
                    );
                }
            } else {
                if !(0..=2).contains(&mode) {
                    return;
                }
                state.cache_settings.catalog_grouping_default =
                    CatalogGroupingMode::from_index(mode);
            }
            state.cache_settings.clone()
        };
        set_active_cache_settings(settings.clone());
        allow_setting_field(field);
        notify_setting_choice(field, &settings);
        write_settings(&settings);
        self.refresh_catalog_grouping_settings();
        self.refresh_setting_sync();
        self.refresh_catalog_grouping_policy();
    }

    fn change_catalog_grouping_inference(&self, enabled: bool) {
        if !writable_key("settings") {
            return;
        }
        let settings = {
            let mut state = self.shared.lock().unwrap();
            state.cache_settings.catalog_grouping_infer_series = enabled;
            state.cache_settings.clone()
        };
        set_active_cache_settings(settings.clone());
        allow_setting_field("catalog_grouping_infer_series");
        notify_setting_choice("catalog_grouping_infer_series", &settings);
        write_settings(&settings);
        self.refresh_catalog_grouping_settings();
        self.refresh_setting_sync();
        self.refresh_catalog_grouping_policy();
    }

    /// User preferences sync; derived groups and lookup budgets remain local.
    /// A mode change reuses raw cached pages rather than refetching catalogs.
    pub(super) fn refresh_catalog_grouping_policy(&self) {
        let profile = profiles::capture();
        let changed = {
            let mut state = self.shared.lock().unwrap();
            let policy = projection_fingerprint(&(
                &profile.id,
                profile.epoch,
                nova_providers::metadata_revision(),
                state.cache_settings.catalog_grouping_default,
                state.cache_settings.catalog_grouping_infer_series,
                &state.cache_settings.catalog_grouping_overrides,
            ));
            if state.catalog_grouping.policy == policy {
                false
            } else {
                state.catalog_grouping.policy = policy;
                state.catalog_grouping.scopes.clear();
                state.catalog_grouping.groups.clear();
                state.catalog_grouping.queue.clear();
                state.catalog_grouping.attempted.clear();
                state.catalog_grouping.budgets.clear();
                state.catalog_grouping.chooser = None;
                state.catalog_grouping.chooser_members.clear();
                true
            }
        };
        if changed {
            if let Some(app) = self.app() {
                app.set_catalog_grouping_chooser_open(false);
            }
            self.reproject_catalog_grouping_discover();
            self.reproject_home_catalog_grouping();
        }
    }

    pub(super) fn refresh_catalog_grouping_evidence(&self) {
        self.reproject_catalog_grouping_discover();
        self.reproject_home_catalog_grouping();
    }
}

pub(super) fn wire_catalog_grouping_callbacks(app: &AppWindow, bridge: &Bridge) {
    let b = bridge.clone();
    app.on_catalog_grouping_infer_series_changed(move |enabled| {
        b.change_catalog_grouping_inference(enabled)
    });
    let b = bridge.clone();
    app.on_catalog_grouping_default_changed(move |mode| {
        b.change_catalog_grouping(None, mode, false)
    });
    let b = bridge.clone();
    app.on_catalog_grouping_override_changed(move |key, surface, mode| {
        b.change_catalog_grouping(Some((&key, surface)), mode, false)
    });
    let b = bridge.clone();
    app.on_catalog_grouping_override_removed(move |key| {
        b.change_catalog_grouping(Some((&key, 0)), -1, true)
    });
    let b = bridge.clone();
    app.on_catalog_grouping_search_changed(move |query| {
        b.shared.lock().unwrap().catalog_grouping.query = query.to_string();
        b.refresh_catalog_grouping_settings();
    });
    let b = bridge.clone();
    app.on_catalog_grouping_member_picked(move |key| b.pick_catalog_group_member(&key));
    let b = bridge.clone();
    app.on_catalog_grouping_chooser_closed(move || b.close_catalog_grouping_chooser());
    let b = bridge.clone();
    app.on_discover_continue_pagination(move || b.continue_catalog_pagination());
}

#[cfg(all(test, feature = "desktop"))]
mod tests {
    use super::*;

    fn key(owner: &str, surface: CatalogSurface) -> CatalogGroupingKey {
        CatalogGroupingKey {
            addon_url: owner.into(),
            type_: "series".into(),
            catalog_id: "anime".into(),
            surface,
        }
    }

    fn preview(id: &str, season: u32) -> MetaPreview {
        serde_json::from_value(serde_json::json!({
            "id":id,"type":"series","name":format!("Release {season}"),
            "novaCatalogGrouping":{"version":1,"kind":"series_member","parentIds":["tt701"],"season":season}
        })).unwrap()
    }

    #[test]
    fn obsolete_lookup_completions_release_replacement_queue_capacity() {
        let mut state = State {
            active: 2,
            ..State::default()
        };
        let profile = profile_stamp();
        assert!(state.scopes.is_empty());
        assert!(release_lookup_slot(&mut state, &profile));
        assert_eq!(state.active, 1);
        assert!(release_lookup_slot(&mut state, &profile));
        assert_eq!(state.active, 0);
    }

    #[test]
    fn catalog_projection_is_reversible_scoped_and_routes_native_members() {
        const CHILD: &str = "NOVA_CATALOG_PROJECTION_CHILD";
        let Some(root) = std::env::var_os(CHILD) else {
            let root = std::env::temp_dir()
                .join(format!("nova-catalog-projection-{}", std::process::id()));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "app::catalog_grouping::tests::catalog_projection_is_reversible_scoped_and_routes_native_members", "--nocapture"])
                .env(CHILD, &root).output().unwrap();
            let _ = std::fs::remove_dir_all(&root);
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        let root = PathBuf::from(root);
        i_slint_backend_testing::init_integration_test_with_mock_time();
        storage::init_at(&root);
        let app = AppWindow::new().unwrap();
        app.window().show().unwrap();
        let player = crate::player::Player::setup(&app);
        let downloads = DownloadCoordinator::new(root.join("downloads"));
        let (hi, _) = mpsc::channel();
        let (lo, _) = mpsc::channel();
        let bridge = Bridge::new(
            app.as_weak(),
            PosterTx { hi, lo },
            Arc::new(AtomicU64::new(7)),
            player,
            downloads,
        );
        wire_catalog_grouping_callbacks(&app, &bridge);
        let owner = "https://catalog.invalid/private-token/manifest.json";
        let home = key(owner, CatalogSurface::Home);
        let discover = key(owner, CatalogSurface::Discover);
        let raw = vec![preview("mal:701", 1), preview("kitsu:702", 2)];
        {
            let mut state = bridge.shared.lock().unwrap();
            state.cache_settings.home_catalog_sources = vec![HomeCatalogSource {
                addon_url: owner.into(),
                type_: "series".into(),
                catalog_id: "anime".into(),
                genre: String::new(),
            }];
            state.installed.push(Installed { url: owner.into(), label: "Fixture".into(), enabled: true, available: false, configure_ok: None, generation: 1,
                manifest: serde_json::from_value(serde_json::json!({"id":"fixture","name":"Fixture","version":"1","types":["series"],"resources":["catalog"],"catalogs":[{"id":"anime","type":"series","name":"Anime"}]})).unwrap() });
            state.installed[0].label = format!("Fixture ({owner})");
            let mut duplicate = state.installed[0].clone();
            duplicate.url = "https://catalog2.invalid/other-private-token/manifest.json".into();
            duplicate.label = format!("Fixture ({})", duplicate.url);
            state.installed.push(duplicate);
        }
        let original = bridge.project_catalog_grouping(&home, &raw);
        assert_eq!(
            original.iter().map(|p| &p.id).collect::<Vec<_>>(),
            raw.iter().map(|p| &p.id).collect::<Vec<_>>()
        );
        bridge.schedule_catalog_grouping(home.clone(), &raw);
        assert!(
            bridge
                .shared
                .lock()
                .unwrap()
                .catalog_grouping
                .attempted
                .is_empty()
        );
        {
            let mut state = bridge.shared.lock().unwrap();
            state
                .cache_settings
                .set_catalog_grouping_override(home.clone(), Some(CatalogGroupingMode::Series));
        }
        let mut full_context = profiles::capture();
        full_context.session = Some(bridge.sessions.active());
        let series = profiles::with_context(full_context, || {
            bridge.project_catalog_grouping(&home, &raw)
        });
        assert!(
            bridge
                .shared
                .lock()
                .unwrap()
                .catalog_grouping
                .scopes
                .values()
                .all(|scope| scope.profile.session.is_none()),
            "retained scopes must not own their ProfileSession"
        );
        bridge.shared.lock().unwrap().home_showcase = series.clone();
        assert_eq!(series.len(), 1);
        assert_eq!(series[0].id, raw[0].id);
        assert_eq!(
            bridge.project_catalog_grouping(&discover, &raw).len(),
            2,
            "surface overrides remain independent"
        );
        bridge.schedule_catalog_grouping(home.clone(), &raw);
        assert!(
            bridge
                .shared
                .lock()
                .unwrap()
                .catalog_grouping
                .attempted
                .is_empty(),
            "complete explicit proof requires no lookup"
        );
        let second_membership = vec![raw[0].clone(), preview("anilist:703", 3)];
        let other = bridge.project_catalog_grouping(&home, &second_membership);
        assert_ne!(display_key(&series[0]), display_key(&other[0]));
        assert_eq!(
            bridge
                .catalog_grouping_display_context(&series[0])
                .unwrap()
                .members[1]
                .id,
            raw[1].id
        );
        assert_eq!(
            bridge
                .catalog_grouping_display_context(&other[0])
                .unwrap()
                .members[1]
                .id,
            "anilist:703"
        );
        assert!(bridge.open_catalog_group(&series[0], 7, None, false));
        assert!(app.get_catalog_grouping_chooser_open());
        {
            let mut state = bridge.shared.lock().unwrap();
            let selected = std::mem::take(&mut state.cache_settings.home_catalog_sources);
            let registered = &state.catalog_grouping.groups[&display_key(&series[0])];
            let scope = &state.catalog_grouping.scopes[&registered.scope];
            assert!(
                !scope_is_current(&state, &registered.scope, scope, 7),
                "a removed Home selection invalidates its retained outgoing card"
            );
            state.cache_settings.home_catalog_sources = selected;
        }
        assert_eq!(app.get_catalog_grouping_chooser_members().row_count(), 2);
        app.invoke_catalog_grouping_member_picked("stale-key".into());
        assert!(app.get_catalog_grouping_chooser_open());
        let second = app
            .get_catalog_grouping_chooser_members()
            .row_data(1)
            .unwrap();
        assert!(!second.subtitle.contains("private-token"));
        assert!(second.subtitle.contains("Fixture (1)"));
        app.invoke_catalog_grouping_member_picked(second.key);
        assert!(!app.get_catalog_grouping_chooser_open());
        assert_eq!(
            bridge
                .shared
                .lock()
                .unwrap()
                .modal_item
                .as_ref()
                .unwrap()
                .id,
            "kitsu:702"
        );
        assert!(bridge.shared.lock().unwrap().entries.is_empty());
        assert!(bridge.shared.lock().unwrap().progress.is_empty());

        // A conflicting declaration cannot be silently replaced by older
        // valid cached evidence for the same native source.
        let mut conflict = raw[0].clone();
        let source = SourceIdentity {
            addon_url: owner.into(),
            media_type: "series".into(),
            id: conflict.id.clone(),
        };
        grouping::remember_evidence(
            &source,
            nova_providers::metadata_revision(),
            grouping::evidence_from_preview(&source, &conflict, owner),
        );
        conflict.extra.get_mut("novaCatalogGrouping").unwrap()["parentIds"] =
            serde_json::json!(["tt701", "tt999"]);
        assert_eq!(
            bridge
                .project_catalog_grouping(&home, &[conflict, raw[1].clone()])
                .len(),
            2
        );

        let combined: MetaItem = serde_json::from_value(serde_json::json!({
            "id":"mal:704","type":"series","name":"Combined",
            "videos":[{"id":"tt701:1:1","season":1,"episode":1},{"id":"tt701:2:1","season":1,"episode":2}]
        })).unwrap();
        let source = SourceIdentity {
            addon_url: owner.into(),
            media_type: "series".into(),
            id: combined.preview.id.clone(),
        };
        grouping::remember_evidence(
            &source,
            nova_providers::metadata_revision(),
            grouping::evidence_from_detail(&source, &combined, owner),
        );
        bridge
            .shared
            .lock()
            .unwrap()
            .cache_settings
            .set_catalog_grouping_override(home.clone(), Some(CatalogGroupingMode::Season));
        let seasons =
            bridge.project_catalog_grouping(&home, std::slice::from_ref(&combined.preview));
        assert_eq!(seasons.len(), 2);
        assert_eq!(seasons[0].id, seasons[1].id);
        assert_ne!(display_key(&seasons[0]), display_key(&seasons[1]));
        assert_eq!(virtual_season(&seasons[0]), Some(1));
        assert_eq!(virtual_season(&seasons[1]), Some(2));
        bridge
            .shared
            .lock()
            .unwrap()
            .cache_settings
            .set_catalog_grouping_override(home.clone(), None);
        let restored =
            bridge.project_catalog_grouping(&home, std::slice::from_ref(&combined.preview));
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].title(), "Combined");
        assert!(!restored[0].extra.contains_key(DISPLAY_KEY));

        // Removed installations retain editable overrides without displaying
        // configured installation URLs (which may contain credentials).
        bridge
            .shared
            .lock()
            .unwrap()
            .cache_settings
            .set_catalog_grouping_override(home.clone(), Some(CatalogGroupingMode::Series));
        bridge
            .shared
            .lock()
            .unwrap()
            .cache_settings
            .set_catalog_grouping_override(discover, Some(CatalogGroupingMode::Season));
        bridge.refresh_catalog_grouping_settings();
        assert!(
            app.get_catalog_grouping_rows()
                .iter()
                .all(|row| !row.addon.contains("://") && !row.addon.contains("private-token"))
        );
        assert_eq!(app.get_catalog_grouping_rows().row_count(), 2);
        let row = app.get_catalog_grouping_rows().row_data(0).unwrap();
        assert!(!row.available);
        assert!(
            !format!("{} {} {}", row.title, row.addon, row.media_type).contains("private-token")
        );
        assert_eq!((row.home_mode, row.discover_mode), (3, 2));
        app.invoke_catalog_grouping_override_removed(row.key);
        assert!(
            bridge
                .shared
                .lock()
                .unwrap()
                .cache_settings
                .catalog_grouping_overrides
                .is_empty()
        );

        // A real parent preview retained by optional inference offers a
        // canonical page and the original release, even for a singleton card.
        {
            let mut state = bridge.shared.lock().unwrap();
            state.cache_settings.catalog_grouping_default = CatalogGroupingMode::Series;
            state.cache_settings.catalog_grouping_infer_series = true;
            state.installed[0].available = true;
            state.installed[0].manifest = serde_json::from_value(serde_json::json!({"id":"fixture","name":"Fixture","version":"1","types":["series"],"resources":["catalog","meta"],"catalogs":[{"id":"anime","type":"series","name":"Anime"}]})).unwrap();
        }
        let future = MetaPreview {
            id: "mal:705".into(),
            type_: "series".into(),
            name: "Fixture Season 3".into(),
            ..Default::default()
        };
        let visible = bridge.project_catalog_grouping(&home, std::slice::from_ref(&future));
        let source = SourceIdentity {
            addon_url: owner.into(),
            media_type: "series".into(),
            id: future.id.clone(),
        };
        let evidence = grouping::GroupingEvidence {
            source: source.clone(),
            parents: vec![grouping::ParentIdentity::ImdbSeries("tt705".into())],
            season: SeasonMembership::Unknown,
            season_partitions: vec![],
            source_inventory_fingerprint: Some("a".repeat(64)),
            proofs: vec![grouping::GroupingProof::InferredSeasonChain {
                provider_id: nova_providers::addon_metadata_id(owner),
                related_release_id: "kitsu:704".into(),
                source_season: 3,
                related_season: 2,
                parent_preview: grouping::InferredParentPreview {
                    source: SourceIdentity {
                        addon_url: owner.into(),
                        media_type: "series".into(),
                        id: "tt705".into(),
                    },
                    name: "Fixture".into(),
                    poster: None,
                    background: None,
                    logo: None,
                    release_info: None,
                },
            }],
        };
        assert!(evidence.verified());
        {
            let mut state = bridge.shared.lock().unwrap();
            let registered = state
                .catalog_grouping
                .groups
                .get_mut(&display_key(&visible[0]))
                .unwrap();
            registered.group.members[0].evidence = Some(evidence.clone());
            registered.group.proven_mapping = vec![Arc::new(evidence)];
        }
        assert!(bridge.open_catalog_group(&visible[0], 7, None, false));
        assert_eq!(app.get_catalog_grouping_chooser_members().row_count(), 2);
        let parent = app
            .get_catalog_grouping_chooser_members()
            .row_data(0)
            .unwrap();
        let native = app
            .get_catalog_grouping_chooser_members()
            .row_data(1)
            .unwrap();
        assert_eq!(parent.title, "Fixture");
        assert!(parent.subtitle.contains("All seasons"));
        assert!(native.subtitle.contains("Inferred grouping"));
        assert_eq!(native.title, "Fixture Season 3");
        let captured_parent_key = parent.key;
        app.invoke_catalog_grouping_member_picked(native.key);
        assert_eq!(
            bridge
                .shared
                .lock()
                .unwrap()
                .modal_item
                .as_ref()
                .unwrap()
                .id,
            "mal:705"
        );
        app.invoke_catalog_grouping_infer_series_changed(false);
        assert!(!app.get_catalog_grouping_infer_series());
        app.invoke_catalog_grouping_member_picked(captured_parent_key);
        assert_eq!(
            bridge
                .shared
                .lock()
                .unwrap()
                .modal_item
                .as_ref()
                .unwrap()
                .id,
            "mal:705",
            "turning inference off revokes old parent actions"
        );
        let restored = bridge.project_catalog_grouping(&home, std::slice::from_ref(&future));
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].id, future.id);
        assert!(bridge.shared.lock().unwrap().entries.is_empty());
        assert!(bridge.shared.lock().unwrap().progress.is_empty());
    }
}
