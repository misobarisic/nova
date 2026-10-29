//! Viewing scope is independent of watched history and external tracker progress.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TitleProgress {
    pub include_specials: Option<bool>,
    pub excluded_episodes: BTreeSet<String>,
    pub included_episodes: BTreeSet<String>,
    pub excluded_seasons: BTreeSet<u32>,
}

#[derive(Clone, Debug, Default)]
pub struct ProgressPolicy {
    pub include_specials: bool,
    pub title: TitleProgress,
}

impl ProgressPolicy {
    pub fn includes(&self, id: &str, season: Option<u32>) -> bool {
        (self.include_specials || season != Some(0))
            && (self.title.included_episodes.contains(id)
                || (!self.title.excluded_episodes.contains(id)
                    && !season.is_some_and(|s| self.title.excluded_seasons.contains(&s))))
    }
}

impl crate::CacheSettings {
    pub fn progress_policy(&self, id: &str) -> ProgressPolicy {
        let title = self.title_progress.get(id).cloned().unwrap_or_default();
        ProgressPolicy {
            include_specials: title
                .include_specials
                .unwrap_or(self.include_specials_in_progress),
            title,
        }
    }
}

/// Read only this title's scope; episode hot paths must not clone all settings.
pub fn active_progress_policy(id: &str) -> ProgressPolicy {
    super::CURRENT_CACHE_SETTINGS
        .lock()
        .unwrap()
        .progress_policy(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_defaults_exclude_only_explicit_specials_and_overrides_round_trip() {
        let mut settings: crate::CacheSettings = serde_json::from_str("{}").unwrap();
        assert!(!settings.include_specials_in_progress);
        let policy = settings.progress_policy("show");
        assert!(!policy.includes("special", Some(0)));
        assert!(policy.includes("regular", Some(1)));
        assert!(policy.includes("unknown", None));
        settings.include_specials_in_progress = true;
        settings.title_progress.insert(
            "show".into(),
            TitleProgress {
                include_specials: Some(false),
                ..Default::default()
            },
        );
        assert!(
            !settings
                .progress_policy("show")
                .includes("special", Some(0))
        );
        assert!(
            settings
                .progress_policy("other")
                .includes("special", Some(0))
        );
        let title = settings.title_progress.get_mut("show").unwrap();
        title.include_specials = Some(true);
        title.excluded_episodes.insert("one".into());
        title.excluded_seasons.insert(3);
        let restored: crate::CacheSettings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        let policy = restored.progress_policy("show");
        assert!(policy.includes("special", Some(0)));
        assert!(!policy.includes("one", Some(1)));
        assert!(!policy.includes("three", Some(3)));
        assert!(policy.includes("two", Some(1)));
        settings
            .title_progress
            .get_mut("show")
            .unwrap()
            .included_episodes
            .insert("three".into());
        assert!(settings.progress_policy("show").includes("three", Some(3)));
    }
}
