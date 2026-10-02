//! Catalog identity is independent of the source's opaque playback identity.
use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdNamespace {
    MalAnime,
    MalManga,
    AnilistAnime,
    AnilistManga,
    Imdb,
    TmdbTv,
    TmdbMovie,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "namespace", content = "value", rename_all = "snake_case")]
pub enum ExternalId {
    MalAnime(NonZeroU32),
    MalManga(NonZeroU32),
    AnilistAnime(NonZeroU32),
    AnilistManga(NonZeroU32),
    Imdb(String),
    TmdbTv(NonZeroU32),
    TmdbMovie(NonZeroU32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdResolution {
    Missing,
    Unique(ExternalId),
    Conflict(Vec<ExternalId>),
}

impl ExternalId {
    pub fn namespace(&self) -> IdNamespace {
        match self {
            Self::MalAnime(_) => IdNamespace::MalAnime,
            Self::MalManga(_) => IdNamespace::MalManga,
            Self::AnilistAnime(_) => IdNamespace::AnilistAnime,
            Self::AnilistManga(_) => IdNamespace::AnilistManga,
            Self::Imdb(_) => IdNamespace::Imdb,
            Self::TmdbTv(_) => IdNamespace::TmdbTv,
            Self::TmdbMovie(_) => IdNamespace::TmdbMovie,
        }
    }

    /// Parse only explicitly namespaced IDs or recognized catalog URLs.
    /// A bare number is never sufficient evidence of a catalog identity.
    pub fn parse(value: &str) -> Option<Self> {
        if value.len() > 512 {
            return None;
        }
        let value = value.trim();
        if value.starts_with("tt") {
            return Self::in_namespace(IdNamespace::Imdb, value);
        }
        if !value.contains("://")
            && let Some((namespace, id)) = value.rsplit_once(':')
        {
            let namespace = match namespace {
                "mal:anime" | "mal" => IdNamespace::MalAnime,
                "mal:manga" => IdNamespace::MalManga,
                "anilist:anime" | "anilist" => IdNamespace::AnilistAnime,
                "anilist:manga" => IdNamespace::AnilistManga,
                "imdb" => IdNamespace::Imdb,
                "tmdb:tv" => IdNamespace::TmdbTv,
                "tmdb:movie" => IdNamespace::TmdbMovie,
                _ => return None,
            };
            return Self::in_namespace(namespace, id);
        }
        let url = url::Url::parse(value).ok()?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return None;
        }
        let mut path = url.path_segments()?;
        let kind = path.next()?;
        let id = path.next()?;
        let namespace = match (url.host_str()?, kind) {
            ("myanimelist.net" | "www.myanimelist.net", "anime") => IdNamespace::MalAnime,
            ("myanimelist.net" | "www.myanimelist.net", "manga") => IdNamespace::MalManga,
            ("anilist.co" | "www.anilist.co", "anime") => IdNamespace::AnilistAnime,
            ("anilist.co" | "www.anilist.co", "manga") => IdNamespace::AnilistManga,
            ("imdb.com" | "www.imdb.com", "title") => IdNamespace::Imdb,
            ("themoviedb.org" | "www.themoviedb.org", "tv") => IdNamespace::TmdbTv,
            ("themoviedb.org" | "www.themoviedb.org", "movie") => IdNamespace::TmdbMovie,
            _ => return None,
        };
        Self::in_namespace(namespace, id)
    }

    /// A metadata field supplies the namespace for a bare ID. Explicit forms
    /// must agree with it, so a manga URL cannot become an anime ID.
    pub fn from_field(namespace: IdNamespace, value: &str) -> Option<Self> {
        if value.len() > 512 {
            return None;
        }
        let parsed = Self::parse(value);
        if let Some(parsed) = parsed {
            return (parsed.namespace() == namespace).then_some(parsed);
        }
        Self::in_namespace(namespace, value.trim())
    }

    fn in_namespace(namespace: IdNamespace, value: &str) -> Option<Self> {
        if namespace == IdNamespace::Imdb {
            let digits = value.strip_prefix("tt")?;
            return (!digits.is_empty()
                && digits.len() <= 12
                && digits.bytes().all(|b| b.is_ascii_digit())
                && digits.bytes().any(|b| b != b'0'))
            .then(|| Self::Imdb(value.to_owned()));
        }
        // Signed 32-bit IDs are compatible with both trackers' numeric input
        // types. Do not accept signs, floating point, zero, or silent overflow.
        if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let id = value.parse::<u32>().ok()?;
        if id > i32::MAX as u32 {
            return None;
        }
        let id = NonZeroU32::new(id)?;
        Some(match namespace {
            IdNamespace::MalAnime => Self::MalAnime(id),
            IdNamespace::MalManga => Self::MalManga(id),
            IdNamespace::AnilistAnime => Self::AnilistAnime(id),
            IdNamespace::AnilistManga => Self::AnilistManga(id),
            IdNamespace::TmdbTv => Self::TmdbTv(id),
            IdNamespace::TmdbMovie => Self::TmdbMovie(id),
            IdNamespace::Imdb => unreachable!(),
        })
    }

    pub fn value(&self) -> String {
        match self {
            Self::Imdb(id) => id.clone(),
            Self::MalAnime(id)
            | Self::MalManga(id)
            | Self::AnilistAnime(id)
            | Self::AnilistManga(id)
            | Self::TmdbTv(id)
            | Self::TmdbMovie(id) => id.to_string(),
        }
    }
}

impl crate::ExternalIds {
    /// Resolve a destination independently, retaining conflicts rather than
    /// choosing an alias by field order. This establishes identity, not coverage.
    pub fn resolve_id(&self, namespace: IdNamespace) -> IdResolution {
        let mut ids: Vec<_> = self
            .typed
            .iter()
            .filter(|id| id.namespace() == namespace)
            .cloned()
            .collect();
        let legacy = match namespace {
            IdNamespace::Imdb => &self.imdb,
            IdNamespace::MalAnime => &self.mal,
            IdNamespace::AnilistAnime => &self.anilist,
            // A legacy untyped TMDB field cannot establish TV/movie identity.
            _ => &None,
        };
        if let Some(id) = legacy
            .as_deref()
            .and_then(|id| ExternalId::from_field(namespace, id))
        {
            ids.push(id);
        }
        ids.sort();
        ids.dedup();
        match ids.len() {
            0 => IdResolution::Missing,
            1 => IdResolution::Unique(ids.remove(0)),
            _ => IdResolution::Conflict(ids),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespace_and_media_kind_are_required() {
        assert_eq!(ExternalId::parse("123"), None);
        assert_eq!(ExternalId::parse("tmdb:123"), None);
        assert_eq!(
            ExternalId::parse("https://anilist.co/anime/123/title"),
            ExternalId::parse("anilist:anime:123")
        );
        assert_eq!(
            ExternalId::parse("https://myanimelist.net/manga/123/title"),
            ExternalId::parse("mal:manga:123")
        );
        assert_ne!(
            ExternalId::parse("tmdb:tv:123"),
            ExternalId::parse("tmdb:movie:123")
        );
        assert_eq!(
            ExternalId::from_field(IdNamespace::MalAnime, "mal:manga:123"),
            None
        );
        assert_eq!(
            ExternalId::parse("https://anilist.co.evil.test/anime/123"),
            None
        );
    }

    #[test]
    fn malformed_and_out_of_range_values_are_rejected() {
        for value in [
            "0",
            "-1",
            "+1",
            "1.0",
            "2147483648",
            "9999999999999999999999",
        ] {
            assert_eq!(ExternalId::from_field(IdNamespace::MalAnime, value), None);
        }
        for value in ["tt", "tt000", "tt12x", "imdb:123"] {
            assert_eq!(ExternalId::parse(value), None);
        }
    }

    #[test]
    fn legacy_metadata_and_conflicts_remain_usable() {
        let mut ids: crate::ExternalIds = serde_json::from_str(r#"{"mal":"123"}"#).unwrap();
        assert_eq!(
            ids.resolve_id(IdNamespace::MalAnime),
            IdResolution::Unique(ExternalId::parse("mal:123").unwrap())
        );
        assert_eq!(
            ids.resolve_id(IdNamespace::AnilistAnime),
            IdResolution::Missing
        );
        ids.typed.push(ExternalId::parse("mal:456").unwrap());
        assert!(matches!(
            ids.resolve_id(IdNamespace::MalAnime),
            IdResolution::Conflict(_)
        ));
    }
}
