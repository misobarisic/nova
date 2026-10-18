//! Persistent discovery decisions for opt-in whole-library anime tracking.
//! Fingerprints include typed identity, metadata and complete episode inventory;
//! unchanged positive and negative decisions do not trigger repeated searches.
use crate::{AccountKey, Assignment, Binding, MappingProposal, SourceRef};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum LibraryEligibility {
    Anime,
    Unknown,
    NonAnime,
}
/// Explicit anime IDs outrank sparse catalog genres. Animation alone is not
/// proof of anime, and never authorizes an automatic tracker mapping.
pub fn library_eligibility(
    media_type: &str,
    genres: &[String],
    has_anime_id: bool,
) -> LibraryEligibility {
    if !matches!(media_type, "movie" | "series" | "anime") {
        return LibraryEligibility::NonAnime;
    }
    if has_anime_id
        || media_type == "anime"
        || genres.iter().any(|g| g.eq_ignore_ascii_case("anime"))
    {
        return LibraryEligibility::Anime;
    }
    if !genres.is_empty() && !genres.iter().any(|g| g.eq_ignore_ascii_case("animation")) {
        return LibraryEligibility::NonAnime;
    }
    LibraryEligibility::Unknown
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LibraryDecision {
    pub source: SourceRef,
    pub fingerprint: String,
    pub title: String,
    pub eligibility: LibraryEligibility,
    pub proposal: MappingProposal,
    pub proven_identity: bool,
    pub dismissed: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LibraryTracking {
    pub account: AccountKey,
    pub enabled: bool,
    pub include_history: bool,
    #[serde(default)]
    pub event_boundary: u64,
    #[serde(default)]
    pub excluded: Vec<SourceRef>,
    pub decisions: Vec<LibraryDecision>,
}
impl LibraryTracking {
    pub fn allows(&self, source: &SourceRef) -> bool {
        self.enabled && !self.excluded.contains(source)
    }
}
/// The sequence is a durable local watched transition, never a metadata time
/// or a paired-device observation. Same-second watches remain distinguishable.
pub fn library_watch_authorized(include_history: bool, boundary: u64, local_sequence: u64) -> bool {
    include_history || local_sequence > boundary
}
pub fn library_scope_allows(full: bool, excluded: bool, bulk: bool) -> bool {
    !excluded && (full || !bulk)
}
/// A unique exact title is only a review seed. Conflicting years/formats and
/// ambiguous remakes stay pending instead of choosing the first search result.
pub fn library_title_candidate(
    candidates: &[crate::Media],
    titles: &[String],
    year: Option<u16>,
    movie: bool,
) -> Option<std::num::NonZeroU32> {
    let mut ids = candidates
        .iter()
        .filter(|m| {
            movie == m.format.eq_ignore_ascii_case("movie")
                && !matches!(
                    crate::title_match(m, titles, year, movie),
                    crate::TitleMatch::SearchResult
                )
                && !matches!((year,m.year), (Some(a),Some(b)) if a != b)
        })
        .map(|m| m.id)
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    (ids.len() == 1).then(|| ids[0])
}
/// A new release must never take over an already confirmed episode. Same-target
/// ongoing extensions preserve every old assignment, including target numbers.
pub fn library_preserves_bindings(
    source: &SourceRef,
    account: &AccountKey,
    proposal: &MappingProposal,
    bindings: &[Binding],
) -> bool {
    proposal.releases.iter().all(|r| {
        r.assignments.iter().all(|new| {
            bindings
                .iter()
                .filter(|b| b.enabled && b.source == *source && b.target.account == *account)
                .all(|b| {
                    b.assignments
                        .iter()
                        .filter(|old| old.episode_id == new.episode_id)
                        .all(|old| b.target.remote_media_id == r.details.media.id && old == new)
                })
        })
    })
}
pub fn library_extension_preserves(old: &[Assignment], new: &[Assignment]) -> bool {
    old.iter().all(|a| new.contains(a))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_search_is_review_only_and_ambiguous_remakes_stay_pending() {
        let media = |id, title: &str, year| crate::Media {
            id: std::num::NonZeroU32::new(id).unwrap(),
            mal_id: None,
            title: title.into(),
            year,
            format: "TV".into(),
            episodes: None,
            finished: false,
        };
        let titles = vec!["Anime".into(), "Alias".into()];
        let exact = media(1, "Anime", Some(2026));
        assert_eq!(
            library_title_candidate(std::slice::from_ref(&exact), &titles, Some(2026), false),
            Some(exact.id)
        );
        assert_eq!(
            library_title_candidate(
                &[exact.clone(), media(2, "Alias", Some(2026))],
                &titles,
                Some(2026),
                false
            ),
            None
        );
        assert_eq!(
            library_title_candidate(
                &[media(3, "Anime Part 2", Some(2026))],
                &titles,
                Some(2026),
                false
            ),
            None
        );
        assert_eq!(
            library_title_candidate(&[media(4, "Anime", Some(2020))], &titles, Some(2026), false),
            None
        );
        assert_eq!(
            library_title_candidate(&[exact], &titles, Some(2026), true),
            None
        );
        assert_eq!(
            library_title_candidate(&[], &titles, Some(2026), false),
            None
        );
    }
    #[test]
    fn live_action_is_skipped_and_animation_is_not_identity_proof() {
        assert_eq!(
            library_eligibility("series", &["Drama".into()], false),
            LibraryEligibility::NonAnime
        );
        assert_eq!(
            library_eligibility("series", &["Animation".into()], false),
            LibraryEligibility::Unknown
        );
        assert_eq!(
            library_eligibility("series", &["Drama".into()], true),
            LibraryEligibility::Anime
        );
        assert_eq!(
            library_eligibility("anime", &[], false),
            LibraryEligibility::Anime
        );
        assert_eq!(
            library_eligibility("channel", &[], true),
            LibraryEligibility::NonAnime
        );
    }
    #[test]
    fn ongoing_extension_must_keep_old_episode_numbers() {
        let a = Assignment {
            episode_id: "native".into(),
            target_episode: 1.try_into().unwrap(),
        };
        let b = Assignment {
            episode_id: "native".into(),
            target_episode: 2.try_into().unwrap(),
        };
        assert!(library_extension_preserves(
            std::slice::from_ref(&a),
            &[
                a.clone(),
                Assignment {
                    episode_id: "new".into(),
                    target_episode: 2.try_into().unwrap()
                }
            ]
        ));
        assert!(!library_extension_preserves(&[a], &[b]));
    }
    #[test]
    fn local_action_sequence_excludes_old_and_paired_history_but_allows_rewatch() {
        assert!(!library_watch_authorized(false, 42, 0));
        assert!(!library_watch_authorized(false, 42, 41));
        assert!(!library_watch_authorized(false, 42, 42));
        assert!(library_watch_authorized(false, 42, 43));
        assert!(library_watch_authorized(true, 42, 0));
    }
    #[test]
    fn selected_full_selected_keeps_explicit_titles_and_exclusions() {
        for full in [false, true, false] {
            assert!(library_scope_allows(full, false, false));
            assert_eq!(library_scope_allows(full, false, true), full);
            assert!(!library_scope_allows(full, true, false));
            assert!(!library_scope_allows(full, true, true));
        }
    }
    #[test]
    fn source_exclusion_survives_negative_cache_and_metadata_replacement() {
        let source = SourceRef {
            provider_id: "nova".into(),
            source_id: "native".into(),
            media_type: "series".into(),
        };
        let mut mode = LibraryTracking {
            account: AccountKey {
                service: crate::Service::AniList,
                remote_user_id: 1.try_into().unwrap(),
            },
            enabled: true,
            include_history: false,
            event_boundary: 42,
            excluded: vec![source.clone()],
            decisions: vec![],
        };
        assert!(!mode.allows(&source));
        mode.decisions.push(LibraryDecision {
            source: source.clone(),
            fingerprint: "metadata-new".into(),
            title: "Anime".into(),
            eligibility: LibraryEligibility::Anime,
            proposal: MappingProposal::default(),
            proven_identity: false,
            dismissed: false,
        });
        let decoded: LibraryTracking =
            serde_json::from_str(&serde_json::to_string(&mode).unwrap()).unwrap();
        assert!(!decoded.allows(&source));
        assert_eq!(decoded.event_boundary, 42);
        assert!(!decoded.include_history);
        assert!(decoded.decisions[0].proposal.releases.is_empty());
    }
    #[test]
    fn a_new_release_can_add_a_season_but_never_relink_old_episodes() {
        let source = SourceRef {
            provider_id: "nova".into(),
            source_id: "show".into(),
            media_type: "series".into(),
        };
        let account = AccountKey {
            service: crate::Service::MyAnimeList,
            remote_user_id: 1.try_into().unwrap(),
        };
        let old = Binding {
            id: "old".into(),
            source: source.clone(),
            target: crate::TargetKey {
                account: account.clone(),
                media_kind: crate::MediaKind::Anime,
                remote_media_id: 10.try_into().unwrap(),
            },
            account_generation: 1.try_into().unwrap(),
            mapping_revision: 1.try_into().unwrap(),
            enabled: true,
            bulk_origin: false,
            assignments: vec![Assignment {
                episode_id: "season1episode1".into(),
                target_episode: 1.try_into().unwrap(),
            }],
        };
        let mut proposal = MappingProposal {
            releases: vec![crate::ProposedRelease {
                details: crate::ReleaseDetails {
                    media: crate::Media {
                        id: 20.try_into().unwrap(),
                        mal_id: Some(20.try_into().unwrap()),
                        title: "Season 2".into(),
                        format: "tv".into(),
                        episodes: Some(12.try_into().unwrap()),
                        finished: true,
                        year: Some(2026),
                    },
                    aliases: vec![],
                    start: crate::ListDate::default(),
                    end: crate::ListDate::default(),
                    relations: vec![],
                },
                season: 2,
                first: 1,
                last: 12,
                assignments: vec![Assignment {
                    episode_id: "season2episode1".into(),
                    target_episode: 1.try_into().unwrap(),
                }],
                check_split: false,
            }],
            unresolved: vec![],
        };
        assert!(library_preserves_bindings(
            &source,
            &account,
            &proposal,
            std::slice::from_ref(&old)
        ));
        proposal.releases[0].assignments[0].episode_id = "season1episode1".into();
        assert!(!library_preserves_bindings(
            &source,
            &account,
            &proposal,
            &[old]
        ));
    }
}
