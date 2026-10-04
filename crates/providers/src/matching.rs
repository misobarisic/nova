use crate::{IdNamespace, IdResolution, MediaItem};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MetadataMatch {
    Exact(usize),
    Ambiguous(Vec<usize>),
    Missing,
}

/// Resolve external metadata without changing a source's identity. Only a
/// unique external-ID match or exact normalized title/alias match qualifies;
/// conflicting years and IDs are never accepted through fuzzy similarity.
pub fn match_metadata(source: &MediaItem, candidates: &[MediaItem]) -> MetadataMatch {
    let titles = normalized_titles(source);
    let matches = candidates.iter().enumerate().filter_map(|(index, candidate)| {
        if source.media_type != candidate.media_type { return None; }
        let mut typed_match = false;
        for namespace in [IdNamespace::Imdb, IdNamespace::MalAnime, IdNamespace::MalManga,
            IdNamespace::AnilistAnime, IdNamespace::AnilistManga, IdNamespace::TmdbTv,
            IdNamespace::TmdbMovie] {
            match (source.external_ids.resolve_id(namespace), candidate.external_ids.resolve_id(namespace)) {
                (IdResolution::Conflict(_), _) | (_, IdResolution::Conflict(_)) => return None,
                (IdResolution::Unique(a), IdResolution::Unique(b)) => {
                    if a != b { return None; }
                    typed_match = true;
                }
                _ => {}
            }
        }
        let ids = [
            (&source.external_ids.imdb, &candidate.external_ids.imdb),
            (&source.external_ids.tmdb, &candidate.external_ids.tmdb),
            (&source.external_ids.mal, &candidate.external_ids.mal),
            (&source.external_ids.anilist, &candidate.external_ids.anilist),
        ];
        if ids.iter().any(|(a, b)| matches!((a.as_deref(), b.as_deref()), (Some(a), Some(b)) if a != b)) {
            return None;
        }
        let id_match = ids.iter().any(|(a, b)| matches!((a.as_deref(), b.as_deref()), (Some(a), Some(b)) if !a.is_empty() && a == b));
        if typed_match || id_match { return Some(index); }
        if let Some(year) = source.year.as_deref().and_then(start_year)
            && candidate.year.as_deref().and_then(start_year) != Some(year) {
            return None;
        }
        normalized_titles(candidate).iter().any(|title| titles.contains(title)).then_some(index)
    }).collect::<Vec<_>>();
    match matches.as_slice() {
        [] => MetadataMatch::Missing,
        [index] => MetadataMatch::Exact(*index),
        _ => MetadataMatch::Ambiguous(matches),
    }
}

pub(crate) fn ids_conflict(a: &MediaItem, b: &MediaItem) -> bool {
    [
        IdNamespace::Imdb,
        IdNamespace::MalAnime,
        IdNamespace::MalManga,
        IdNamespace::AnilistAnime,
        IdNamespace::AnilistManga,
        IdNamespace::TmdbTv,
        IdNamespace::TmdbMovie,
    ]
    .into_iter()
    .any(
        |ns| match (a.external_ids.resolve_id(ns), b.external_ids.resolve_id(ns)) {
            (IdResolution::Conflict(_), _) | (_, IdResolution::Conflict(_)) => true,
            (IdResolution::Unique(a), IdResolution::Unique(b)) => a != b,
            _ => false,
        },
    )
}

fn normalized_titles(item: &MediaItem) -> Vec<String> {
    std::iter::once(&item.title)
        .chain(&item.aliases)
        .map(|title| {
            title
                .chars()
                .flat_map(char::to_lowercase)
                .filter(|character| character.is_alphanumeric())
                .collect::<String>()
        })
        .filter(|title| !title.is_empty())
        .collect()
}

fn start_year(value: &str) -> Option<&str> {
    let year = value.get(..4)?;
    year.bytes()
        .all(|byte| byte.is_ascii_digit())
        .then_some(year)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(title: &str, year: Option<&str>) -> MediaItem {
        MediaItem {
            title: title.into(),
            media_type: "series".into(),
            year: year.map(str::to_owned),
            ..Default::default()
        }
    }

    #[test]
    fn exact_alias_match_rejects_conflicting_years_and_types() {
        let mut source = item("Shingeki no Kyojin", Some("2013"));
        source.aliases.push("Attack on Titan".into());
        let candidates = [
            item("Attack on Titan", Some("2013–2023")),
            item("Attack on Titan", Some("2020")),
        ];
        assert_eq!(
            match_metadata(&source, &candidates),
            MetadataMatch::Exact(0)
        );
        let mut wrong_type = candidates[0].clone();
        wrong_type.media_type = "movie".into();
        assert_eq!(
            match_metadata(&source, &[wrong_type]),
            MetadataMatch::Missing
        );
        assert_eq!(
            match_metadata(&source, &[item("Attack on Titans", Some("2013"))]),
            MetadataMatch::Missing
        );
    }

    #[test]
    fn ambiguity_never_selects_a_candidate() {
        let source = item("Monster", None);
        assert_eq!(
            match_metadata(
                &source,
                &[item("Monster", Some("2004")), item("Monster", Some("2020"))]
            ),
            MetadataMatch::Ambiguous(vec![0, 1])
        );
        let mut source = source;
        source.external_ids.imdb = Some("tt123".into());
        let mut candidate = item("Different translated title", None);
        candidate.external_ids.imdb = Some("tt123".into());
        assert_eq!(
            match_metadata(&source, &[candidate]),
            MetadataMatch::Exact(0)
        );
        let mut candidate = item("Monster", None);
        candidate.external_ids.imdb = Some("tt456".into());
        assert_eq!(
            match_metadata(&source, &[candidate]),
            MetadataMatch::Missing
        );
    }
    #[test]
    fn tracker_conflicts_cannot_fall_back_to_title_matching() {
        let mut source = item("Monster", None);
        source.external_ids.typed = vec![
            crate::ExternalId::parse("mal:1").unwrap(),
            crate::ExternalId::parse("mal:2").unwrap(),
        ];
        assert_eq!(
            match_metadata(&source, &[item("Monster", None)]),
            MetadataMatch::Missing
        );
        source.external_ids.typed = vec![crate::ExternalId::parse("anilist:1").unwrap()];
        let mut candidate = item("Translated title", None);
        candidate.external_ids.anilist = Some("1".into());
        assert_eq!(
            match_metadata(&source, &[candidate]),
            MetadataMatch::Exact(0)
        );
    }
}
