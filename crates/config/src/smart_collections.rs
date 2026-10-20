//! Profile settings for automatic Library collections. Rules contain no title
//! membership: the app re-evaluates them against current progress and metadata.
use serde::{Deserialize, Deserializer, Serialize};

pub const MAX_SMART_COLLECTIONS: usize = 32;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmartMediaType {
    #[default]
    Any,
    Movies,
    Series,
}

impl SmartMediaType {
    pub fn index(self) -> i32 {
        match self {
            Self::Any => 0,
            Self::Movies => 1,
            Self::Series => 2,
        }
    }
    pub fn from_index(index: i32) -> Self {
        match index {
            1 => Self::Movies,
            2 => Self::Series,
            _ => Self::Any,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmartWatchState {
    #[default]
    Any,
    Unwatched,
    Watched,
}

impl SmartWatchState {
    pub fn index(self) -> i32 {
        match self {
            Self::Any => 0,
            Self::Unwatched => 1,
            Self::Watched => 2,
        }
    }
    pub fn from_index(index: i32) -> Self {
        match index {
            1 => Self::Unwatched,
            2 => Self::Watched,
            _ => Self::Any,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SmartCollection {
    pub id: String,
    pub name: String,
    pub media_type: SmartMediaType,
    pub watch_state: SmartWatchState,
    /// One genre, matched exactly after case folding and common sci-fi aliases.
    pub genre: String,
    /// Exclusive upper bound. Unknown runtime never satisfies this predicate.
    pub max_runtime_minutes: Option<u32>,
}

impl SmartCollection {
    pub fn valid(&self) -> bool {
        !self.id.is_empty()
            && self.id.len() <= 96
            && self
                .id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
            && !self.name.trim().is_empty()
            && self.name.chars().count() <= 80
            && !self.name.chars().any(char::is_control)
            && self.genre.chars().count() <= 80
            && !self.genre.chars().any(char::is_control)
            && self
                .max_runtime_minutes
                .is_none_or(|minutes| (1..=1440).contains(&minutes))
    }

    pub fn matches(
        &self,
        media_type: &str,
        watched: bool,
        genres: &[String],
        runtime_minutes: Option<u32>,
    ) -> bool {
        let type_matches = match self.media_type {
            SmartMediaType::Any => true,
            SmartMediaType::Movies => media_type == "movie",
            SmartMediaType::Series => media_type == "series",
        };
        let watch_matches = match self.watch_state {
            SmartWatchState::Any => true,
            SmartWatchState::Unwatched => !watched,
            SmartWatchState::Watched => watched,
        };
        type_matches
            && watch_matches
            && (self.genre.trim().is_empty()
                || genres
                    .iter()
                    .any(|genre| genre_key(genre) == genre_key(&self.genre)))
            && self.max_runtime_minutes.is_none_or(|maximum| {
                runtime_minutes.is_some_and(|minutes| minutes > 0 && minutes < maximum)
            })
    }
}

fn genre_key(genre: &str) -> String {
    let folded = genre.trim().to_lowercase();
    let compact: String = folded
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect();
    if matches!(compact.as_str(), "scifi" | "sciencefiction") {
        "sciencefiction".into()
    } else {
        folded
    }
}

/// Bound settings from local storage and sync before any UI model is created.
/// Returning an error lets the app's existing unsupported-field recovery retain
/// the original value instead of replacing future/invalid rules with defaults.
pub fn deserialize_collections<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<SmartCollection>, D::Error> {
    let collections = Vec::<SmartCollection>::deserialize(deserializer)?;
    let mut seen = std::collections::HashSet::new();
    if collections.len() > MAX_SMART_COLLECTIONS
        || collections
            .iter()
            .any(|rule| !rule.valid() || !seen.insert(rule.id.clone()))
    {
        return Err(serde::de::Error::custom("invalid smart collection rules"));
    }
    Ok(collections)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scifi() -> SmartCollection {
        SmartCollection {
            id: "stable-id".into(),
            name: "Short sci-fi".into(),
            media_type: SmartMediaType::Movies,
            watch_state: SmartWatchState::Unwatched,
            genre: "Sci-fi".into(),
            max_runtime_minutes: Some(120),
        }
    }
    #[test]
    fn short_unwatched_scifi_movies_require_every_rule_and_known_runtime() {
        let rule = scifi();
        let genres = vec!["Science Fiction".into(), "Adventure".into()];
        assert!(rule.matches("movie", false, &genres, Some(119)));
        assert!(!rule.matches("movie", false, &genres, Some(120)));
        assert!(!rule.matches("movie", false, &genres, None));
        assert!(!rule.matches("movie", false, &genres, Some(0)));
        assert!(!rule.matches("movie", true, &genres, Some(90)));
        assert!(!rule.matches("series", false, &genres, Some(90)));
        assert!(!rule.matches("movie", false, &["Drama".into()], Some(90)));
    }
    #[test]
    fn changed_watch_state_and_metadata_automatically_change_membership() {
        let mut rule = scifi();
        assert!(!rule.matches("movie", false, &[], Some(90)));
        assert!(rule.matches("movie", false, &["SCI FI".into()], Some(90)));
        rule.watch_state = SmartWatchState::Watched;
        assert!(rule.matches("movie", true, &["sci-fi".into()], Some(90)));
        rule.genre.clear();
        rule.max_runtime_minutes = None;
        rule.media_type = SmartMediaType::Any;
        assert!(rule.matches("series", true, &[], None));
    }
    #[test]
    fn settings_decoder_rejects_duplicate_ids_and_oversized_rule_sets() {
        #[derive(Deserialize)]
        struct Rules {
            #[serde(deserialize_with = "deserialize_collections")]
            rules: Vec<SmartCollection>,
        }
        let valid = serde_json::json!({"rules": [scifi()]});
        assert_eq!(
            serde_json::from_value::<Rules>(valid).unwrap().rules.len(),
            1
        );
        let duplicate = serde_json::json!({"rules": [scifi(), scifi()]});
        assert!(serde_json::from_value::<Rules>(duplicate).is_err());
        let oversized = serde_json::json!({"rules": vec![scifi(); MAX_SMART_COLLECTIONS + 1]});
        assert!(serde_json::from_value::<Rules>(oversized).is_err());
    }

    #[test]
    fn rules_round_trip_and_reject_unsafe_or_unbounded_edits() {
        let mut rule = scifi();
        assert!(rule.valid());
        assert_eq!(
            serde_json::from_str::<SmartCollection>(&serde_json::to_string(&rule).unwrap())
                .unwrap(),
            rule
        );
        rule.name = "   ".into();
        assert!(!rule.valid());
        rule.name = "Name".into();
        rule.max_runtime_minutes = Some(0);
        assert!(!rule.valid());
        rule.max_runtime_minutes = Some(90);
        rule.id = "bad:id".into();
        assert!(!rule.valid());
    }
}
