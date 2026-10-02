//! Search evidence ranks candidates but never activates a writable binding.
use crate::Media;
fn normalized(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
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
        let similar = titles
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
}
