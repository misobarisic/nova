//! Search evidence ranks candidates but never activates a writable binding.
use crate::Media;
fn normalized(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
/// Hide only active links for this source and verified destination account.
/// Other accounts/services and disabled links remain eligible for selection.
pub fn filter_linked_candidates(
    state: &crate::TrackingState,
    source: &crate::SourceRef,
    account: &crate::AccountKey,
    candidates: &mut Vec<Media>,
) {
    candidates.retain(|candidate| {
        !state.bindings.iter().any(|binding| {
            binding.enabled
                && binding.source == *source
                && binding.target.account == *account
                && binding.target.remote_media_id == candidate.id
        })
    });
}
/// Evidence labels describe metadata agreement, never episode equivalence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TitleMatch {
    TitleAndYear,
    Title,
    SearchResult,
}
pub fn title_match(media: &Media, titles: &[String], year: Option<u16>, movie: bool) -> TitleMatch {
    let title = normalized(&media.title);
    let exact = !title.is_empty()
        && titles
            .iter()
            .any(|candidate| normalized(candidate) == title);
    if !exact {
        return TitleMatch::SearchResult;
    }
    let format_match = movie == media.format.eq_ignore_ascii_case("movie");
    if format_match && year.is_some() && media.year == year {
        TitleMatch::TitleAndYear
    } else {
        TitleMatch::Title
    }
}
pub fn rank_candidates(
    media: &mut [Media],
    titles: &[String],
    year: Option<u16>,
    movie: bool,
    source_count: usize,
) {
    let titles: Vec<_> = titles
        .iter()
        .map(|s| normalized(s))
        .filter(|s| !s.is_empty())
        .collect();
    media.sort_by_cached_key(|m| {
        let title = normalized(&m.title);
        let exact = titles.contains(&title);
        let similar = !title.is_empty()
            && titles
                .iter()
                .any(|t| title.contains(t) || t.contains(&title));
        let format_match = movie == (m.format.eq_ignore_ascii_case("movie"));
        let year_match = year.is_some() && m.year == year;
        let count_match = m.episodes.is_some_and(|n| n.get() as usize == source_count);
        // Counts and dates are ranking hints, never numbering proof. Unknown
        // metadata stays selectable, including a later cour of the source.
        std::cmp::Reverse((exact, similar, format_match, year_match, count_match))
    });
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU32;
    #[test]
    fn ranking_preserves_ambiguity_and_prefers_alias_and_release_context() {
        let make = |id, title: &str, format: &str, year| Media {
            id: NonZeroU32::new(id).unwrap(),
            mal_id: None,
            title: title.into(),
            format: format.into(),
            episodes: NonZeroU32::new(12),
            finished: true,
            year,
        };
        let mut candidates = vec![
            make(1, "Other", "TV", Some(2020)),
            make(2, "Alias", "MOVIE", Some(2020)),
            make(3, "Alias", "TV", Some(2020)),
            make(4, "Alias", "TV", Some(2021)),
        ];
        rank_candidates(&mut candidates, &["Alias".into()], Some(2020), false, 12);
        assert_eq!(
            candidates.iter().map(|m| m.id.get()).collect::<Vec<_>>(),
            vec![3, 4, 2, 1]
        );
        assert_eq!(
            candidates.len(),
            4,
            "ranking cannot discard an ambiguous candidate"
        );
    }
    #[test]
    fn evidence_does_not_claim_a_release_match_from_partial_titles_or_unknown_metadata() {
        let mut media = Media {
            id: NonZeroU32::new(1).unwrap(),
            mal_id: None,
            title: "Example Part 2".into(),
            format: "TV".into(),
            year: Some(2020),
            episodes: None,
            finished: false,
        };
        assert_eq!(
            title_match(&media, &["Example".into()], Some(2020), false),
            TitleMatch::SearchResult
        );
        let aliases = vec!["Example Part 2".into()];
        assert_eq!(
            title_match(&media, &aliases, None, false),
            TitleMatch::Title
        );
        assert_eq!(
            title_match(&media, &aliases, Some(2021), false),
            TitleMatch::Title
        );
        assert_eq!(
            title_match(&media, &aliases, Some(2020), true),
            TitleMatch::Title
        );
        assert_eq!(
            title_match(&media, &aliases, Some(2020), false),
            TitleMatch::TitleAndYear
        );
        media.title = "!!!".into();
        assert_eq!(
            title_match(&media, &["...".into()], Some(2020), false),
            TitleMatch::SearchResult
        );
    }
    #[test]
    fn linked_suggestions_are_filtered_only_for_the_same_source_and_account() {
        use crate::*;
        let n = |id| NonZeroU32::new(id).unwrap();
        let source = SourceRef {
            provider_id: "nova".into(),
            source_id: "source".into(),
            media_type: "series".into(),
        };
        let account = AccountKey {
            service: Service::MyAnimeList,
            remote_user_id: n(7),
        };
        let mut state = TrackingState::default();
        state.bindings.push(Binding {
            id: "link".into(),
            source: source.clone(),
            target: TargetKey {
                account: account.clone(),
                media_kind: MediaKind::Anime,
                remote_media_id: n(1),
            },
            account_generation: std::num::NonZeroU64::MIN,
            mapping_revision: std::num::NonZeroU64::MIN,
            enabled: true,
            assignments: vec![],
        });
        let candidates = vec![Media {
            id: n(1),
            mal_id: None,
            title: "Title".into(),
            format: "TV".into(),
            year: None,
            episodes: None,
            finished: false,
        }];
        let mut filtered = candidates.clone();
        filter_linked_candidates(&state, &source, &account, &mut filtered);
        assert!(filtered.is_empty());
        let other_account = AccountKey {
            service: Service::MyAnimeList,
            remote_user_id: n(8),
        };
        filtered = candidates.clone();
        filter_linked_candidates(&state, &source, &other_account, &mut filtered);
        assert_eq!(filtered, candidates);
        let other_service = AccountKey {
            service: Service::AniList,
            remote_user_id: n(7),
        };
        filtered = candidates.clone();
        filter_linked_candidates(&state, &source, &other_service, &mut filtered);
        assert_eq!(filtered, candidates);
        let other_source = SourceRef {
            source_id: "other-source".into(),
            ..source.clone()
        };
        filtered = candidates.clone();
        filter_linked_candidates(&state, &other_source, &account, &mut filtered);
        assert_eq!(filtered, candidates);
        state.bindings[0].enabled = false;
        filtered = candidates.clone();
        filter_linked_candidates(&state, &source, &account, &mut filtered);
        assert_eq!(filtered, candidates);
    }
}
