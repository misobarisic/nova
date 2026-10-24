//! Virtual catalog seasons are an opening-owned view over complete native metadata.
use super::*;
use nova_providers::catalog_grouping::{GroupingEvidence, SeasonSelection, SourceIdentity};
use std::borrow::Cow;

#[derive(Clone)]
pub(super) struct Opening {
    source: SourceIdentity,
    selection: Option<SeasonSelection>,
}
struct Bound {
    opening: Opening,
    profile: profiles::Context,
    token: Arc<()>,
    owner_generation: Option<u64>,
    revision: u64,
    allowed: HashSet<String>,
    verified: bool,
    invalid: bool,
}
#[derive(Default)]
pub(super) struct State {
    pending: Option<Opening>,
    bound: Option<Bound>,
    generation: u64,
}
#[derive(Clone)]
pub(super) struct Receipt {
    pub source: SourceIdentity,
    pub owner_generation: u64,
    pub revision: u64,
    pub evidence: Option<GroupingEvidence>,
}
impl State {
    pub(super) fn clear(&mut self) {
        self.pending = None;
        self.bound = None;
        self.generation = self.generation.wrapping_add(1);
    }
    pub(super) fn generation(&self) -> u64 {
        self.generation
    }
    pub(super) fn active(&self) -> bool {
        self.bound
            .as_ref()
            .is_some_and(|b| b.opening.selection.is_some())
    }
    pub(super) fn owner(&self, modal: &ModalItem) -> Option<(&SourceIdentity, Option<u64>)> {
        self.bound
            .as_ref()
            .filter(|b| Arc::ptr_eq(&b.token, &modal.open_token))
            .map(|b| (&b.opening.source, b.owner_generation))
    }
    fn invalidate(&mut self) {
        if let Some(bound) = self.bound.as_mut() {
            bound.verified = false;
            bound.invalid = true;
            self.generation = self.generation.wrapping_add(1);
        }
    }
    fn receipt_current(&self, state: &Shared, receipt: Option<&Receipt>) -> bool {
        let Some(receipt) = receipt else {
            return self
                .bound
                .as_ref()
                .is_none_or(|bound| bound.opening.selection.is_none());
        };
        receipt.revision == nova_providers::metadata_revision()
            && state.installed.iter().any(|addon| {
                addon.url == receipt.source.addon_url
                    && addon.enabled
                    && addon.available
                    && addon.generation == receipt.owner_generation
            })
            && self.bound.as_ref().is_none_or(|bound| {
                profiles::is_current(&bound.profile)
                    && bound.opening.source == receipt.source
                    && bound.owner_generation == Some(receipt.owner_generation)
                    && bound.revision == receipt.revision
            })
    }
    fn current(&self, state: &Shared, modal: &ModalItem) -> bool {
        self.bound.as_ref().is_some_and(|b| {
            !b.invalid
                && profiles::is_current(&b.profile)
                && Arc::ptr_eq(&b.token, &modal.open_token)
                && b.revision == nova_providers::metadata_revision()
                && b.opening.source.id == modal.id
                && b.opening.source.media_type == modal.type_
                && state.installed.iter().any(|a| {
                    a.url == b.opening.source.addon_url
                        && a.enabled
                        && a.available
                        && Some(a.generation) == b.owner_generation
                })
        })
    }
    fn valid(&self, state: &Shared, modal: &ModalItem) -> bool {
        self.current(state, modal) && self.bound.as_ref().is_some_and(|b| b.verified)
    }
    fn accept(&mut self, receipt: Option<&Receipt>, item: Option<&MetaItem>) -> bool {
        let Some(bound) = self.bound.as_mut() else {
            return true;
        };
        if bound.opening.selection.is_none() && receipt.is_none() {
            return true;
        }
        let Some(receipt) = receipt.filter(|r| {
            r.source == bound.opening.source
                && Some(r.owner_generation) == bound.owner_generation
                && r.revision == bound.revision
                && r.revision == nova_providers::metadata_revision()
                && profiles::is_current(&bound.profile)
        }) else {
            bound.invalid = true;
            return false;
        };
        let Some(selection) = &bound.opening.selection else {
            return true;
        };
        if bound.invalid {
            return false;
        }
        let valid = item.is_some_and(|item| {
            let ids = item
                .videos
                .iter()
                .map(|v| v.id.as_str())
                .collect::<Vec<_>>();
            let unique = ids.iter().copied().collect::<HashSet<_>>();
            receipt.evidence.as_ref().is_some_and(|proof| {
                proof.source == bound.opening.source
                    && proof.verified()
                    && proof.mapping_fingerprint() == selection.mapping_fingerprint
                    && proof.source_inventory_fingerprint.as_deref()
                        == Some(selection.source_inventory_fingerprint.as_str())
                    && proof
                        .season_partitions
                        .iter()
                        .find(|part| part.canonical_season == selection.canonical_season)
                        .is_some_and(|part| {
                            part.episodes.len() == selection.source_episode_ids.len()
                                && part.episodes.iter().all(|episode| {
                                    bound.allowed.contains(&episode.source_episode_id)
                                })
                        })
            }) && ids.len() == unique.len()
                && nova_providers::catalog_grouping::episode_inventory_fingerprint(ids)
                    == selection.source_inventory_fingerprint
                && !selection.source_episode_ids.is_empty()
                && selection
                    .source_episode_ids
                    .iter()
                    .all(|id| unique.contains(id.as_str()))
        });
        bound.verified = valid;
        bound.invalid = !valid;
        if !valid {
            self.generation = self.generation.wrapping_add(1);
        }
        valid
    }
}

pub(super) fn ready(state: &Shared, modal: &ModalItem) -> bool {
    !state.catalog_detail.active() || state.catalog_detail.valid(state, modal)
}
pub(super) fn seasons(state: &Shared, modal: &ModalItem) -> Vec<u32> {
    if state.catalog_detail.active() {
        state
            .catalog_detail
            .bound
            .as_ref()
            .and_then(|b| b.opening.selection.as_ref())
            .map(|s| vec![s.canonical_season])
            .unwrap_or_default()
    } else {
        modal.seasons.clone()
    }
}
pub(super) fn allows(state: &Shared, modal: &ModalItem, id: &str) -> bool {
    !state.catalog_detail.active()
        || (state.catalog_detail.valid(state, modal)
            && state
                .catalog_detail
                .bound
                .as_ref()
                .is_some_and(|b| b.allowed.contains(id)))
}
pub(super) fn episodes<'a>(state: &Shared, modal: &'a ModalItem, season: u32) -> Vec<&'a Video> {
    if state.catalog_detail.active() {
        if !state.catalog_detail.valid(state, modal) {
            return Vec::new();
        }
        let mut visible: Vec<_> = modal
            .videos
            .iter()
            .filter(|v| {
                state
                    .catalog_detail
                    .bound
                    .as_ref()
                    .is_some_and(|b| b.allowed.contains(&v.id))
            })
            .collect();
        visible.sort_by_key(|video| video.episode_number().unwrap_or(u32::MAX));
        visible
    } else {
        season_episodes(&modal.videos, season)
    }
}
/// References only: progress repaint and thumbnail publication must not
/// clone a complete season's descriptions/artwork to apply a view predicate.
pub(super) fn visible_native_videos<'a>(state: &Shared, modal: &'a ModalItem) -> Vec<&'a Video> {
    if state.catalog_detail.active() {
        episodes(state, modal, 0)
    } else {
        modal.videos.iter().collect()
    }
}
pub(super) fn season_artwork(state: &Shared, modal: &ModalItem, season: u32) -> Option<String> {
    if state.cache_settings.spoiler_protection {
        return None;
    }
    if state.catalog_detail.active() {
        episodes(state, modal, season)
            .iter()
            .find_map(|video| video.thumbnail.clone())
    } else {
        season_thumb_url(&modal.videos, &modal.season_backdrops, season)
    }
}
pub(super) fn view_videos<'a>(state: &Shared, modal: &'a ModalItem) -> Cow<'a, [Video]> {
    if state.catalog_detail.active() {
        if !state.catalog_detail.valid(state, modal) {
            return Cow::Owned(Vec::new());
        }
        // This temporary navigation copy lets generic Watch Now ignore native
        // season numbering (which can be relative or absent). Persisted/source
        // Video values remain untouched, and picks still resolve by native ID.
        let season = state
            .catalog_detail
            .bound
            .as_ref()
            .and_then(|b| b.opening.selection.as_ref())
            .map(|s| s.canonical_season);
        Cow::Owned(
            modal
                .videos
                .iter()
                .filter(|v| {
                    state
                        .catalog_detail
                        .bound
                        .as_ref()
                        .is_some_and(|b| b.allowed.contains(&v.id))
                })
                .cloned()
                .map(|mut v| {
                    v.season = season;
                    v
                })
                .collect(),
        )
    } else {
        Cow::Borrowed(&modal.videos)
    }
}

/// Confirmed chooser members with a global native ID retain the ordinary
/// metadata fallback. An opaque ID is meaningful only at its chosen owner.
/// Virtual seasons always require that owner, regardless of ID namespace.
fn opening_for_source(
    source: SourceIdentity,
    selection: Option<SeasonSelection>,
) -> Option<Opening> {
    let kitsu = source.id.strip_prefix("kitsu:").is_some_and(|id| {
        !id.is_empty()
            && id.bytes().all(|byte| byte.is_ascii_digit())
            && id.parse::<u64>().is_ok_and(|id| id > 0)
    });
    let global = kitsu
        || nova_providers::ExternalId::parse(&source.id).is_some()
        || nova_providers::catalog_grouping::ParentIdentity::parse(&source.id).is_some();
    (selection.is_some() || !global).then_some(Opening { source, selection })
}

impl Bridge {
    pub(super) fn open_catalog_detail(
        &self,
        preview: MetaPreview,
        source: SourceIdentity,
        selection: Option<SeasonSelection>,
        generation: u64,
        discover_index: Option<usize>,
        watch_now: bool,
    ) {
        if preview.id != source.id
            || preview.type_ != source.media_type
            || selection.as_ref().is_some_and(|s| s.source != source)
        {
            return;
        }
        {
            let mut state = self.shared.lock().unwrap();
            state.related_detail_history.clear();
            state.catalog_detail.pending = opening_for_source(source, selection);
        }
        self.open_preview_inner(preview, generation, false, discover_index, watch_now, true);
    }
    /// Consume before any ordinary-open callbacks can recursively navigate.
    pub(super) fn begin_catalog_detail_open(&self) -> Option<Opening> {
        let mut state = self.shared.lock().unwrap();
        let pending = state.catalog_detail.pending.take();
        state.catalog_detail.clear();
        pending
    }
    pub(super) fn bind_catalog_detail_open(&self, opening: Option<Opening>) {
        if let Some(opening) = opening {
            let mut state = self.shared.lock().unwrap();
            let Some(modal) = state.modal_item.as_ref() else {
                return;
            };
            let mut profile = profiles::capture();
            profile.session = None;
            state.catalog_detail.bound = Some(Bound {
                token: modal.open_token.clone(),
                owner_generation: state
                    .installed
                    .iter()
                    .find(|a| a.url == opening.source.addon_url && a.enabled && a.available)
                    .map(|a| a.generation),
                allowed: opening
                    .selection
                    .as_ref()
                    .map(|s| s.source_episode_ids.iter().cloned().collect())
                    .unwrap_or_default(),
                opening,
                profile,
                revision: nova_providers::metadata_revision(),
                verified: false,
                invalid: false,
            });
        }
        self.publish_catalog_detail_scope();
    }
    pub(super) fn publish_catalog_detail_scope(&self) {
        let Some(app) = self.app() else { return };
        let (active, label, notice) = {
            let state = self.shared.lock().unwrap();
            let active = state.catalog_detail.active();
            let label = state
                .catalog_detail
                .bound
                .as_ref()
                .and_then(|b| b.opening.selection.as_ref())
                .map(|s| season_label(s.canonical_season))
                .unwrap_or_default();
            let notice = if !active {
                ""
            } else if state
                .modal_item
                .as_ref()
                .is_none_or(|m| !state.catalog_detail.current(&state, m))
            {
                "This season mapping changed. Open All seasons or choose the catalog card again."
            } else if !state
                .catalog_detail
                .bound
                .as_ref()
                .is_some_and(|b| b.verified)
            {
                "Checking this season's episode mapping…"
            } else {
                ""
            };
            (active, label, text::tr(notice))
        };
        app.set_catalog_grouping_detail_scope_active(active);
        app.set_catalog_grouping_detail_season_label(label.into());
        app.set_catalog_grouping_detail_scope_notice(notice.into());
    }
    /// Catalog seasons constrain the initial navigation, not the native
    /// episode sequence once playback has reached a verified successor.
    pub(super) fn release_catalog_detail_for_native_successor(&self) {
        let mut state = self.shared.lock().unwrap();
        if !state.catalog_detail.active() {
            return;
        }
        state.catalog_detail.clear();
        drop(state);
        self.publish_catalog_detail_scope();
    }
    pub(super) fn catalog_detail_all_seasons(&self) {
        self.cancel_pending_for_navigation();
        self.cancel_season_download();
        self.clear_streams();
        {
            let mut state = self.shared.lock().unwrap();
            state.catalog_detail.clear();
            if let Some(modal) = state.modal_item.as_mut() {
                modal.pending_watch_now = None;
                modal.season_index = 0;
                modal.episode_page = 0;
            }
        }
        if let Some(app) = self.app() {
            app.set_episode_filter(SharedString::default());
            app.set_season_combo_idx(0);
            app.set_modal_episodes(true);
            app.set_stream_selector_open(false);
            app.set_detail_deep_stream(false);
            app.set_detail_tab(3);
            app.set_detail_kb_ci(0);
            app.set_detail_kb_ep(0);
            apply_stream_episode_context(&app, None);
        }
        self.publish_catalog_detail_scope();
        self.refresh_catalog_detail_season_names();
        self.apply_season_cards();
        self.refresh_episode_rows();
    }
    pub(super) fn refresh_catalog_detail_season_names(&self) {
        let names = {
            let state = self.shared.lock().unwrap();
            state
                .modal_item
                .as_ref()
                .map(|m| seasons(&state, m))
                .unwrap_or_default()
        };
        if let Some(app) = self.app() {
            app.set_season_names(
                Rc::new(VecModel::from(
                    names
                        .into_iter()
                        .map(|s| SharedString::from(season_label(s)))
                        .collect::<Vec<_>>(),
                ))
                .into(),
            );
        }
        self.publish_catalog_detail_scope();
    }
    /// Validate the exact owner again on the UI authority thread. A valid
    /// current receipt can update complete metadata even when its old season
    /// selection has become invalid; a foreign/stale receipt cannot.
    pub(super) fn catalog_detail_receipt_current(&self, receipt: Option<&Receipt>) -> bool {
        let mut state = self.shared.lock().unwrap();
        if state.catalog_detail.receipt_current(&state, receipt) {
            return true;
        }
        state.catalog_detail.invalidate();
        false
    }
    pub(super) fn observe_catalog_detail_inventory(&self, id: &str, videos: &[Video]) {
        let mut state = self.shared.lock().unwrap();
        if state.modal_item.as_ref().is_none_or(|modal| modal.id != id) {
            return;
        }
        let changed = state.catalog_detail.bound.as_ref().is_some_and(|bound| {
            bound.verified
                && bound.opening.selection.as_ref().is_some_and(|selection| {
                    nova_providers::catalog_grouping::episode_inventory_fingerprint(
                        videos.iter().map(|v| v.id.as_str()),
                    ) != selection.source_inventory_fingerprint
                })
        });
        if changed {
            state.catalog_detail.invalidate();
        }
    }
    pub(super) fn accept_catalog_detail_meta(
        &self,
        receipt: Option<&Receipt>,
        found: Option<&MetaItem>,
    ) -> bool {
        self.shared
            .lock()
            .unwrap()
            .catalog_detail
            .accept(receipt, found)
    }
}

#[cfg(all(test, feature = "desktop"))]
mod tests {
    use super::*;
    use nova_config::CatalogGroupingMode;
    use nova_providers::catalog_grouping::{
        OwnedCatalogEntry, evidence_from_detail, project_catalog,
    };

    fn fixture() -> (
        MetaItem,
        SourceIdentity,
        GroupingEvidence,
        Vec<SeasonSelection>,
    ) {
        let detail = Addon::parse_meta(&serde_json::to_vec(&serde_json::json!({"meta": {
            "id":"kitsu:77", "type":"anime", "name":"Example", "videos":[
                {"id":"kitsu:77:1","season":1,"episode":1,"released":"2000-01-01","imdb_id":"tt7","imdbSeason":1,"imdbEpisode":1},
                {"id":"kitsu:77:2","season":1,"episode":2,"released":"2000-01-01","imdb_id":"tt7","imdbSeason":2,"imdbEpisode":1},
                {"id":"kitsu:77:3","season":1,"episode":3,"released":"2000-01-01","imdb_id":"tt7","imdbSeason":2,"imdbEpisode":2}
            ]
        }})).unwrap()).unwrap().unwrap();
        let mut entry = OwnedCatalogEntry::new("https://owner.example", detail.preview.clone());
        let source = entry.source.clone();
        let evidence = evidence_from_detail(&source, &detail, &source.addon_url).unwrap();
        entry.evidence = Some(evidence.clone());
        let selections = project_catalog(&[entry], CatalogGroupingMode::Season)
            .into_iter()
            .map(|group| group.season_selection(&source).unwrap())
            .collect();
        (detail, source, evidence, selections)
    }
    fn installed(source: &SourceIdentity) -> Installed {
        Installed {
            url: source.addon_url.clone(), label: "Owner".into(), enabled: true,
            configure_ok: None, available: true, generation: 7,
            manifest: serde_json::from_value(serde_json::json!({
                "id":"owner", "name":"Owner", "version":"1.0.0", "resources":["meta"], "types":["anime"], "catalogs":[]
            })).unwrap(),
        }
    }
    fn modal(detail: &MetaItem) -> ModalItem {
        ModalItem {
            details: Default::default(),
            open_token: Arc::new(()),
            pending_watch_now: None,
            episodes_loading: false,
            id: detail.preview.id.clone(),
            type_: detail.preview.type_.clone(),
            request_id: detail.preview.id.clone(),
            videos: detail.videos.clone(),
            season_backdrops: HashMap::new(),
            seasons: ordered_seasons(&detail.videos),
            season_index: 0,
            episode_page: 0,
            name: detail.preview.name.clone(),
            year: String::new(),
            poster_url: String::new(),
            background_url: String::new(),
            logo_url: String::new(),
            description: String::new(),
            genres: vec![],
        }
    }
    fn scoped(detail: &MetaItem, selection: SeasonSelection) -> Shared {
        let source = selection.source.clone();
        let modal = modal(detail);
        let mut profile = profiles::capture();
        profile.session = None;
        let scope = State {
            bound: Some(Bound {
                token: modal.open_token.clone(),
                opening: Opening {
                    source: source.clone(),
                    selection: Some(selection.clone()),
                },
                profile,
                owner_generation: Some(7),
                revision: nova_providers::metadata_revision(),
                allowed: selection.source_episode_ids.into_iter().collect(),
                verified: false,
                invalid: false,
            }),
            ..Default::default()
        };
        Shared {
            installed: vec![installed(&source)],
            modal_item: Some(modal),
            catalog_detail: scope,
            ..Default::default()
        }
    }
    fn receipt(source: &SourceIdentity, evidence: Option<GroupingEvidence>) -> Receipt {
        Receipt {
            source: source.clone(),
            evidence,
            owner_generation: 7,
            revision: nova_providers::metadata_revision(),
        }
    }

    #[test]
    fn ordinary_global_members_keep_fallback_but_virtual_and_opaque_members_bind_owner() {
        let (_, source, _, choices) = fixture();
        for id in [
            "tt7",
            "kitsu:77",
            "mal:77",
            "anilist:77",
            "tmdb:tv:77",
            "tvdb:77",
        ] {
            assert!(
                opening_for_source(
                    SourceIdentity {
                        id: id.into(),
                        ..source.clone()
                    },
                    None
                )
                .is_none(),
                "global {id} retains normal metadata fallback"
            );
        }
        for id in ["owner-specific-show", "77", "kitsu:unknown", "kitsu:0"] {
            assert!(
                opening_for_source(
                    SourceIdentity {
                        id: id.into(),
                        ..source.clone()
                    },
                    None
                )
                .is_some(),
                "opaque {id} remains owner-bound"
            );
        }
        assert!(
            opening_for_source(source, Some(choices[1].clone())).is_some(),
            "even a global ID needs exact-owner inventory for virtual seasons"
        );
    }

    #[test]
    fn virtual_seasons_choose_distinct_native_ids_without_changing_inventory_or_successors() {
        let (detail, source, proof, choices) = fixture();
        let before = serde_json::to_value(&detail.videos).unwrap();
        assert_eq!(
            choices
                .iter()
                .map(|s| s.canonical_season)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(choices[0].source, choices[1].source);
        for choice in choices {
            let expected = choice.source_episode_ids.clone();
            let mut state = scoped(&detail, choice.clone());
            let receipt = receipt(&source, Some(proof.clone()));
            assert!(state.catalog_detail.receipt_current(&state, Some(&receipt)));
            assert!(state.catalog_detail.accept(Some(&receipt), Some(&detail)));
            let modal = state.modal_item.as_ref().unwrap();
            assert!(ready(&state, modal));
            assert_eq!(seasons(&state, modal), [choice.canonical_season]);
            assert_eq!(
                episodes(&state, modal, choice.canonical_season)
                    .iter()
                    .map(|v| v.id.clone())
                    .collect::<Vec<_>>(),
                expected
            );
            assert!(
                view_videos(&state, modal)
                    .iter()
                    .all(|v| v.season == Some(choice.canonical_season))
            );
            assert_eq!(serde_json::to_value(&modal.videos).unwrap(), before);
            assert_eq!(
                season_episodes(&modal.videos, 1).len(),
                3,
                "native successor inventory remains complete"
            );
            let viewed = view_videos(&state, modal);
            assert_eq!(
                watch_now_episode(&modal.id, &viewed, &state.progress)
                    .unwrap()
                    .id,
                expected[0]
            );
        }
    }

    #[test]
    fn canonical_partition_does_not_require_native_season_numbers() {
        let (mut detail, source, proof, choices) = fixture();
        for video in &mut detail.videos {
            video.season = None;
        }
        let before = serde_json::to_value(&detail.videos).unwrap();
        let mut state = scoped(&detail, choices[1].clone());
        assert!(
            state
                .catalog_detail
                .accept(Some(&receipt(&source, Some(proof))), Some(&detail))
        );
        let modal = state.modal_item.as_ref().unwrap();
        assert!(modal.seasons.is_empty());
        assert_eq!(seasons(&state, modal), [2]);
        assert_eq!(
            episodes(&state, modal, 2)
                .iter()
                .map(|video| video.id.as_str())
                .collect::<Vec<_>>(),
            ["kitsu:77:2", "kitsu:77:3"]
        );
        assert_eq!(serde_json::to_value(&modal.videos).unwrap(), before);
        let view = view_videos(&state, modal);
        assert_eq!(
            watch_now_episode(&modal.id, &view, &state.progress)
                .unwrap()
                .id,
            "kitsu:77:2"
        );
    }

    #[test]
    fn refreshed_unmapped_inventory_invalidates_stickily_and_wrong_partition_is_rejected() {
        let (mut detail, source, proof, choices) = fixture();
        let mut state = scoped(&detail, choices[1].clone());
        let old = detail.clone();
        let valid = receipt(&source, Some(proof.clone()));
        assert!(state.catalog_detail.accept(Some(&valid), Some(&detail)));
        detail.videos.push(Video {
            id: "new-unmapped-episode".into(),
            season: Some(1),
            ..Default::default()
        });
        let generation = state.catalog_detail.generation();
        let fresh = receipt(
            &source,
            evidence_from_detail(&source, &detail, &source.addon_url),
        );
        assert!(!state.catalog_detail.accept(Some(&fresh), Some(&detail)));
        assert_ne!(state.catalog_detail.generation(), generation);
        assert!(
            !state.catalog_detail.accept(Some(&valid), Some(&old)),
            "late old proof cannot repair the old opening"
        );
        let modal = state.modal_item.as_ref().unwrap();
        assert!(episodes(&state, modal, 2).is_empty());
        assert!(!allows(&state, modal, "kitsu:77:2"));
        let mut forged = choices[1].clone();
        forged.source_episode_ids = choices[0].source_episode_ids.clone();
        let mut state = scoped(&old, forged);
        assert!(
            !state.catalog_detail.accept(Some(&valid), Some(&old)),
            "membership must match the proved canonical partition"
        );
    }

    #[test]
    fn opening_owner_generation_revision_and_token_all_guard_the_view() {
        let (detail, source, proof, choices) = fixture();
        let mut state = scoped(&detail, choices[1].clone());
        let valid = receipt(&source, Some(proof));
        assert!(state.catalog_detail.accept(Some(&valid), Some(&detail)));
        let mut foreign = valid.clone();
        foreign.source.addon_url = "https://different-owner.example".into();
        assert!(!state.catalog_detail.receipt_current(&state, Some(&foreign)));
        state.installed[0].generation += 1;
        assert!(!ready(&state, state.modal_item.as_ref().unwrap()));
        assert!(!state.catalog_detail.receipt_current(&state, Some(&valid)));
        state.installed[0].generation -= 1;
        state.catalog_detail.bound.as_mut().unwrap().revision += 1;
        assert!(!ready(&state, state.modal_item.as_ref().unwrap()));
        state.catalog_detail.bound.as_mut().unwrap().revision -= 1;
        state.modal_item.as_mut().unwrap().open_token = Arc::new(());
        assert!(
            !ready(&state, state.modal_item.as_ref().unwrap()),
            "same ID reopened is a different scope"
        );
        state.catalog_detail.clear();
        assert!(!state.catalog_detail.active());
        assert!(ready(&state, state.modal_item.as_ref().unwrap()));
    }

    #[test]
    fn bridge_season_actions_use_native_members_and_all_seasons_restores_full_view() {
        const KEY: &str = "NOVA_CATALOG_DETAIL_TEST_ROOT";
        let Some(root) = std::env::var_os(KEY) else {
            let root = std::env::temp_dir().join(format!(
                "nova-catalog-detail-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(KEY, &root).env("XDG_CACHE_HOME", root.join("cache"))
                .args(["--exact", "app::catalog_detail::tests::bridge_season_actions_use_native_members_and_all_seasons_restores_full_view", "--nocapture"])
                .output().unwrap();
            let _ = fs::remove_dir_all(root);
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        let root = PathBuf::from(root);
        storage::init_at(&root);
        profiles::initialize().unwrap();
        let other = profiles::create("Other").unwrap();
        i_slint_backend_testing::init_integration_test_with_system_time();
        let app = AppWindow::new().unwrap();
        let (hi, _) = mpsc::channel();
        let (lo, _) = mpsc::channel();
        let bridge = Bridge::new(
            app.as_weak(),
            PosterTx { hi, lo },
            Arc::new(AtomicU64::new(0)),
            crate::player::Player::setup(&app),
            DownloadCoordinator::new(root.join("downloads")),
        );
        let writer = persistence::start().unwrap();
        let (detail, source, proof, choices) = fixture();
        {
            let mut state = bridge.shared.lock().unwrap();
            state.installed = vec![installed(&source)];
            state.modal_item = Some(modal(&detail));
        }
        write_episodes_cache_for(&source.media_type, &source.id, &detail.videos);
        let cached = read_episodes_cache_for(&source.media_type, &source.id).unwrap();
        bridge.bind_catalog_detail_open(Some(Opening {
            source: source.clone(),
            selection: Some(choices[1].clone()),
        }));
        assert!(app.get_catalog_grouping_detail_scope_active());
        assert!(
            bridge
                .shared
                .lock()
                .unwrap()
                .catalog_detail
                .bound
                .as_ref()
                .unwrap()
                .profile
                .session
                .is_none(),
            "retained state must not create Shared → ProfileSession → Shared cycles"
        );
        assert!(!app.get_catalog_grouping_detail_scope_notice().is_empty());
        assert_eq!(
            bridge.current_episode_page().0,
            0,
            "unverified cached rows cannot select a native season accidentally"
        );
        let receipt = receipt(&source, Some(proof));
        assert!(bridge.catalog_detail_receipt_current(Some(&receipt)));
        assert!(bridge.accept_catalog_detail_meta(Some(&receipt), Some(&detail)));
        bridge.apply_episode_rows();
        bridge.apply_season_cards();
        assert_eq!(
            app.get_episode_rows()
                .iter()
                .map(|row| row.id.to_string())
                .collect::<Vec<_>>(),
            ["kitsu:77:2", "kitsu:77:3"]
        );
        assert_eq!(app.get_season_cards().row_count(), 1);
        assert_eq!(app.get_season_cards().row_data(0).unwrap().id, "2");
        bridge.season_watch_action(0, 1);
        {
            let state = bridge.shared.lock().unwrap();
            assert!(
                !state
                    .progress
                    .contains_key(&progress_map_key(&source.id, "kitsu:77:1"))
            );
            for id in ["kitsu:77:2", "kitsu:77:3"] {
                assert!(state.progress[&progress_map_key(&source.id, id)].watched);
            }
            assert_eq!(
                serde_json::to_value(&state.modal_item.as_ref().unwrap().videos).unwrap(),
                serde_json::to_value(&detail.videos).unwrap()
            );
        }
        assert_eq!(
            serde_json::to_value(read_episodes_cache_for(&source.media_type, &source.id).unwrap())
                .unwrap(),
            serde_json::to_value(cached).unwrap()
        );
        {
            let mut state = bridge.shared.lock().unwrap();
            let selected = state
                .progress
                .get_mut(&progress_map_key(&source.id, "kitsu:77:3"))
                .unwrap();
            selected.watched = false;
            selected.position_secs = 37.0;
            selected.duration_secs = 1000.0;
            selected.updated_at_secs = now_secs();
            state.progress.insert(
                progress_map_key(&source.id, "kitsu:77:1"),
                EpisodeProgress {
                    series_id: source.id.clone(),
                    episode_id: "kitsu:77:1".into(),
                    position_secs: 89.0,
                    duration_secs: 1000.0,
                    updated_at_secs: now_secs() + 10,
                    ..Default::default()
                },
            );
        }
        bridge.watch_now();
        assert_eq!(
            bridge
                .shared
                .lock()
                .unwrap()
                .modal_item
                .as_ref()
                .unwrap()
                .request_id,
            "kitsu:77:3",
            "initial grouped resume excludes even a newer position outside the selected season"
        );
        bridge.episode_streams_by_id(&source.id, "kitsu:77:1");
        assert_eq!(
            bridge
                .shared
                .lock()
                .unwrap()
                .modal_item
                .as_ref()
                .unwrap()
                .request_id,
            "kitsu:77:3",
            "out-of-view native IDs cannot be selected through a stale callback"
        );
        bridge.release_catalog_detail_for_native_successor();
        assert!(!app.get_catalog_grouping_detail_scope_active());
        {
            let state = bridge.shared.lock().unwrap();
            assert!(
                allows(&state, state.modal_item.as_ref().unwrap(), "kitsu:77:1"),
                "verified native successor navigation releases the catalog-only restriction"
            );
        }
        bridge.bind_catalog_detail_open(Some(Opening {
            source: source.clone(),
            selection: Some(choices[1].clone()),
        }));
        assert!(bridge.accept_catalog_detail_meta(Some(&receipt), Some(&detail)));
        bridge.season_download_start(0, false);
        let plan = bridge
            .shared
            .lock()
            .unwrap()
            .season_download
            .clone()
            .unwrap();
        {
            let state = bridge.shared.lock().unwrap();
            assert_eq!(
                plan.catalog_scope_test_state(&state),
                (vec!["kitsu:77:2".into(), "kitsu:77:3".into()], true)
            );
        }
        {
            let mut state = bridge.shared.lock().unwrap();
            state.catalog_detail.bound.as_mut().unwrap().profile = profiles::context_for(&other);
            assert!(!ready(&state, state.modal_item.as_ref().unwrap()));
        }
        bridge.publish_catalog_detail_scope();
        assert!(!app.get_catalog_grouping_detail_scope_notice().is_empty());
        {
            let state = bridge.shared.lock().unwrap();
            assert!(
                !plan.catalog_scope_test_state(&state).1,
                "profile changes revoke the captured download view"
            );
        }
        bridge.bind_catalog_detail_open(Some(Opening {
            source: source.clone(),
            selection: Some(choices[1].clone()),
        }));
        assert!(bridge.accept_catalog_detail_meta(Some(&receipt), Some(&detail)));
        let token = bridge
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_ref()
            .unwrap()
            .open_token
            .clone();
        let mut foreign = receipt.clone();
        foreign.source.addon_url = "https://wrong.example".into();
        let mut wrong_item = detail.clone();
        wrong_item.videos.clear();
        bridge.apply_episode_meta_owned(
            source.id.clone(),
            Some(wrong_item),
            true,
            token.clone(),
            Some(foreign),
        );
        assert_eq!(
            bridge
                .shared
                .lock()
                .unwrap()
                .modal_item
                .as_ref()
                .unwrap()
                .videos
                .len(),
            3,
            "foreign owner cannot overwrite native metadata"
        );
        bridge.bind_catalog_detail_open(Some(Opening {
            source: source.clone(),
            selection: Some(choices[1].clone()),
        }));
        assert!(bridge.accept_catalog_detail_meta(Some(&receipt), Some(&detail)));
        let mut fresh = detail.clone();
        fresh.videos.push(Video {
            id: "new-unmapped".into(),
            season: Some(1),
            episode: Some(4),
            released: Some("2000-01-01".into()),
            ..Default::default()
        });
        write_episodes_cache_for(&source.media_type, &source.id, &fresh.videos);
        let fresh_receipt = Receipt {
            evidence: None,
            ..receipt.clone()
        };
        bridge.apply_episode_meta_owned(
            source.id.clone(),
            Some(fresh),
            true,
            token,
            Some(fresh_receipt),
        );
        assert_eq!(
            bridge
                .shared
                .lock()
                .unwrap()
                .modal_item
                .as_ref()
                .unwrap()
                .videos
                .len(),
            4,
            "current metadata stays complete when old view proof expires"
        );
        assert_eq!(app.get_episode_rows().row_count(), 0);
        assert!(!app.get_catalog_grouping_detail_scope_notice().is_empty());
        assert!(
            !bridge.accept_catalog_detail_meta(Some(&receipt), Some(&detail)),
            "old reply cannot revalidate a sticky invalid scope"
        );
        bridge.catalog_detail_all_seasons();
        assert!(!app.get_catalog_grouping_detail_scope_active());
        assert_eq!(app.get_episode_rows().row_count(), 4);
        assert_eq!(app.get_season_cards().row_data(0).unwrap().id, "1");
        {
            let state = bridge.shared.lock().unwrap();
            assert!(
                !plan.catalog_scope_test_state(&state).1,
                "All seasons invalidates already reviewed scoped plans"
            );
        }
        bridge.season_download_confirm();
        assert!(
            bridge.downloads.jobs().is_empty(),
            "a cleared scoped plan cannot enqueue stale native IDs"
        );
        assert!(
            bridge.begin_catalog_detail_open().is_none(),
            "an ordinary opening cannot inherit the previous season selection"
        );
        persistence::flush().unwrap();
        drop(writer);
    }
    #[test]
    fn fresh_owner_fetch_revalidates_mapping_instead_of_reusing_same_id_cache() {
        const KEY: &str = "NOVA_CATALOG_DETAIL_FETCH_TEST_ROOT";
        let Some(root) = std::env::var_os(KEY) else {
            let root = std::env::temp_dir().join(format!(
                "nova-catalog-detail-fetch-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(KEY, &root).env("XDG_CACHE_HOME", root.join("cache"))
                .args(["--exact", "app::catalog_detail::tests::fresh_owner_fetch_revalidates_mapping_instead_of_reusing_same_id_cache", "--nocapture"])
                .output().unwrap();
            let _ = fs::remove_dir_all(root);
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        use nova_providers::catalog_grouping::{
            GroupingProof, ParentIdentity, cached_evidence, remember_evidence,
        };
        use std::io::{Read, Write};
        let root = PathBuf::from(root);
        storage::init_at(&root);
        profiles::initialize().unwrap();
        i_slint_backend_testing::init_integration_test_with_system_time();
        let app = AppWindow::new().unwrap();
        let (hi, _) = mpsc::channel();
        let (lo, _) = mpsc::channel();
        let bridge = Bridge::new(
            app.as_weak(),
            PosterTx { hi, lo },
            Arc::new(AtomicU64::new(0)),
            crate::player::Player::setup(&app),
            DownloadCoordinator::new(root.join("downloads")),
        );
        let writer = persistence::start().unwrap();
        for case in [
            "removed",
            "partial",
            "conflicting",
            "changed-mapping",
            "other-provenance",
            "external-broker",
            "external-addon",
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let (detail, mut source, _, _) = fixture();
            source.addon_url = format!("http://{}", listener.local_addr().unwrap());
            let mut cached = evidence_from_detail(&source, &detail, &source.addon_url).unwrap();
            let mut fresh = detail.clone();
            match case {
                "removed" | "external-broker" | "external-addon" => {
                    for video in &mut fresh.videos {
                        for key in ["imdb_id", "imdbSeason", "imdbEpisode"] {
                            video.extra.remove(key);
                        }
                    }
                    if case == "external-broker" {
                        cached.proofs = vec![GroupingProof::BrokerAlignment {
                            provider_id: "external-canonical-broker".into(),
                            episode_ids: vec!["tt7:1:1".into(), "tt7:2:1".into(), "tt7:2:2".into()],
                        }];
                    } else if case == "external-addon" {
                        cached.proofs = vec![GroupingProof::CanonicalEpisodes {
                            provider_id: "external-compatible-addon".into(),
                            episode_ids: detail
                                .videos
                                .iter()
                                .map(|video| video.id.clone())
                                .collect(),
                        }];
                    }
                }
                "partial" => {
                    fresh.videos[2].extra.remove("imdbEpisode");
                }
                "conflicting" => {
                    fresh.videos[2]
                        .extra
                        .insert("imdb_id".into(), serde_json::json!("tt999"));
                }
                "changed-mapping" => {
                    fresh.videos[2]
                        .extra
                        .insert("imdbSeason".into(), serde_json::json!(3));
                }
                "other-provenance" => {
                    cached.parents.push(ParentIdentity::TvdbSeries(7));
                    cached.proofs.push(GroupingProof::ExplicitParent {
                        provider_id: "another-provider".into(),
                    });
                    let direct = evidence_from_detail(&source, &fresh, &source.addon_url).unwrap();
                    assert_ne!(direct.fingerprint(), cached.fingerprint());
                    assert_eq!(direct.mapping_fingerprint(), cached.mapping_fingerprint());
                }
                _ => unreachable!(),
            }
            assert!(cached.verified());
            let selection = SeasonSelection {
                source: source.clone(),
                canonical_season: 2,
                source_episode_ids: vec!["kitsu:77:2".into(), "kitsu:77:3".into()],
                source_inventory_fingerprint: cached.source_inventory_fingerprint.clone().unwrap(),
                mapping_fingerprint: cached.mapping_fingerprint(),
            };
            let old_mapping = selection.mapping_fingerprint.clone();
            let addon = installed(&source);
            // Mirror installation's registry publication. The real network
            // worker captures this same owner inventory; leaving the global
            // revision at zero would correctly reject the fixture before its
            // fresh proof could even be extracted or persisted.
            nova_providers::configure_metadata_addons(vec![nova_providers::MetadataAddon {
                url: addon.url.clone(),
                manifest: addon.manifest.clone(),
            }]);
            let revision = nova_providers::metadata_revision();
            remember_evidence(&source, revision, Some(cached));
            write_episodes_cache_for(&source.media_type, &source.id, &detail.videos);
            bridge.begin_catalog_detail_open();
            {
                let mut state = bridge.shared.lock().unwrap();
                state.installed = vec![addon];
                state.modal_item = Some(modal(&detail));
            }
            assert_eq!(
                profiles::capture()
                    .session
                    .as_ref()
                    .unwrap()
                    .provider_inventory()
                    .unwrap()
                    .with(nova_providers::metadata_revision),
                revision,
                "UI and real fetch worker must share metadata authority: {case}"
            );
            bridge.bind_catalog_detail_open(Some(Opening {
                source: source.clone(),
                selection: Some(selection),
            }));
            let ids = fresh
                .videos
                .iter()
                .map(|video| video.id.clone())
                .collect::<Vec<_>>();
            let body = serde_json::to_vec(&serde_json::json!({"meta": fresh})).unwrap();
            let server = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let mut request = [0_u8; 8192];
                let count = socket.read(&mut request).unwrap();
                assert!(String::from_utf8_lossy(&request[..count]).starts_with("GET "));
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                socket.write_all(&body).unwrap();
            });
            bridge.prepare_episodes(source.id.clone(), source.media_type.clone());
            let started = std::time::Instant::now();
            let timer = slint::Timer::default();
            let pending = bridge.shared.clone();
            timer.start(
                slint::TimerMode::Repeated,
                Duration::from_millis(3),
                move || {
                    assert!(
                        started.elapsed() < Duration::from_secs(15),
                        "metadata callback timed out: {case}"
                    );
                    if pending
                        .lock()
                        .unwrap()
                        .modal_item
                        .as_ref()
                        .is_some_and(|modal| !modal.episodes_loading)
                    {
                        slint::quit_event_loop().unwrap();
                    }
                },
            );
            slint::run_event_loop().unwrap();
            drop(timer);
            server.join().unwrap();
            let expected = matches!(
                case,
                "other-provenance" | "external-broker" | "external-addon"
            );
            {
                let state = bridge.shared.lock().unwrap();
                let modal = state.modal_item.as_ref().unwrap();
                assert_eq!(
                    ready(&state, modal),
                    expected,
                    "current owner response: {case}"
                );
                assert_eq!(
                    modal
                        .videos
                        .iter()
                        .map(|video| video.id.clone())
                        .collect::<Vec<_>>(),
                    ids,
                    "full native inventory remains intact: {case}"
                );
            }
            let saved =
                cached_evidence(&source, revision).expect("worker persists the extraction verdict");
            assert_eq!(
                saved.is_some(),
                expected || case == "changed-mapping",
                "worker must persist fresh proof or clear obsolete proof: {case}"
            );
            if case == "changed-mapping" {
                assert_ne!(
                    saved.as_ref().unwrap().mapping_fingerprint(),
                    old_mapping,
                    "current valid mapping replaces the old mapping instead of silently reviving it"
                );
            }
            assert_eq!(
                app.get_episode_rows().row_count(),
                if expected { 2 } else { 0 },
                "published scoped rows: {case}"
            );
            assert_eq!(
                app.get_catalog_grouping_detail_scope_notice().is_empty(),
                expected,
                "visible scope explanation: {case}"
            );
            assert_eq!(
                read_episodes_cache_for(&source.media_type, &source.id)
                    .unwrap()
                    .iter()
                    .map(|video| video.id.clone())
                    .collect::<Vec<_>>(),
                ids
            );
        }
        persistence::flush().unwrap();
        drop(writer);
    }
}
