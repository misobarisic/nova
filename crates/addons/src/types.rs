//! Typed models for the Stremio addon protocol responses.
//!
//! Field names follow the JSON payloads (`camelCase` in the wire format is
//! mapped to `snake_case`). Unknown keys are preserved through
//! `#[serde(flatten)]` so that addons may send additional metadata without
//! breaking deserialisation.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

fn default_false() -> bool {
    false
}

/// The `manifest.json` document describing an addon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub id: String,
    pub version: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub resources: Vec<Resource>,
    #[serde(default)]
    pub types: Vec<String>,
    #[serde(default)]
    pub catalogs: Vec<Catalog>,
    #[serde(default)]
    pub id_prefixes: Vec<String>,
    #[serde(default)]
    pub logo: Option<String>,
    #[serde(default)]
    pub background: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

impl Manifest {
    /// Whether this addon implements a resource whose name starts with
    /// `resource` (handles `"stream"` and `{"name": "stream/…"}` alike).
    pub fn has_resource(&self, resource: &str) -> bool {
        self.resources.iter().any(|r| {
            let name = r.name();
            name == resource || name.starts_with(&format!("{resource}/"))
        })
    }

    /// Convenience checks for the three well-known resource kinds.
    pub fn has_catalogs(&self) -> bool {
        self.has_resource("catalog")
    }

    pub fn has_meta(&self) -> bool {
        self.has_resource("meta")
    }

    pub fn has_streams(&self) -> bool {
        self.has_resource("stream")
    }

    /// The catalogs this addon declares for `type_` (e.g. `"movie"`).
    pub fn catalogs_for_type(&self, type_: &str) -> impl Iterator<Item = &Catalog> {
        self.catalogs.iter().filter(move |c| c.type_ == type_)
    }

    /// Find the catalog with the given media type and catalog id.
    pub fn catalog_for(&self, type_: &str, id: &str) -> Option<&Catalog> {
        self.catalogs
            .iter()
            .find(|c| c.type_ == type_ && c.id == id)
    }
}

/// An entry in `manifest.resources` — either a plain name string
/// (`"catalog"`, `"stream"`) or a detailed object with extra properties.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Resource {
    Plain(String),
    Detailed(ResourceDetail),
}

impl Resource {
    pub fn name(&self) -> &str {
        match self {
            Resource::Plain(name) => name,
            Resource::Detailed(detail) => &detail.name,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceDetail {
    pub name: String,
    #[serde(default)]
    pub id_prefixes: Vec<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

/// One catalog entry from `manifest.catalogs`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    #[serde(rename = "type")]
    pub type_: String,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub extra: Vec<CatalogExtra>,
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(flatten)]
    pub extra_fields: HashMap<String, Value>,
}

impl Catalog {
    /// Does this catalog accept the extra parameter `name` (e.g. `"search"`
    /// or `"genre"`)?
    pub fn supports_extra(&self, name: &str) -> bool {
        self.extra.iter().any(|e| e.name == name)
    }
}

/// A declared extra parameter of a catalog (search, genre, skip, …).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CatalogExtra {
    pub name: String,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default = "default_false")]
    pub is_required: bool,
    #[serde(default = "default_false")]
    pub options_required: bool,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

/// Top-level shape of a catalog response: `{"metas": […]}`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MetaDetail {
    #[serde(default)]
    pub metas: Vec<MetaPreview>,
}

/// A catalog entry / light metadata card, as returned by
/// `GET /catalog/{type}/{id}.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MetaPreview {
    pub id: String,
    #[serde(rename = "type", alias = "type_")]
    pub type_: String,
    /// Display title. Some addons emit `title` (and some emit both, so the
    /// two must not share a serde alias — see [`MetaPreview::title`]).
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "title")]
    pub title_legacy: Option<String>,
    #[serde(default)]
    pub poster: Option<String>,
    #[serde(default)]
    pub background: Option<String>,
    #[serde(default)]
    pub logo: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub genres: Vec<String>,
    /// Human-readable release info such as `"1994"` or `"2017-2018"`.
    /// Some addons send a number, so keep the raw value. (Some addons, e.g.
    /// Cinemeta, also send a separate `year` key — that one lands in `extra`
    /// and is consulted by [`MetaPreview::year_str`] as a fallback.)
    #[serde(default)]
    pub release_info: Option<Value>,
    #[serde(default)]
    pub runtime: Option<String>,
    #[serde(default)]
    pub imdb_rating: Option<Value>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

impl MetaPreview {
    /// The display title: the SDK `name`, or the legacy `title` key when an
    /// addon only sends that.
    pub fn title(&self) -> String {
        if !self.name.is_empty() {
            self.name.clone()
        } else {
            self.title_legacy.clone().unwrap_or_default()
        }
    }

    /// Release info as a plain string, if present. Prefers `releaseInfo`,
    /// falls back to the plain `year` key some addons send instead.
    pub fn year_str(&self) -> Option<String> {
        if let Some(info) = self.release_info.as_ref().and_then(stringify) {
            return Some(info);
        }
        self.extra.get("year").and_then(stringify)
    }

    /// IMDb rating as a plain string, if present.
    pub fn rating_str(&self) -> Option<String> {
        self.imdb_rating.as_ref().and_then(stringify)
    }
}

/// A piece of media — catalog entry with optional extra detail
/// (`videos` for series episodes, cast, etc.).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MetaItem {
    #[serde(flatten)]
    pub preview: MetaPreview,
    #[serde(default)]
    pub videos: Vec<Video>,
    #[serde(default)]
    pub cast: Vec<String>,
    #[serde(default)]
    pub director: Vec<String>,
    #[serde(default)]
    pub writer: Vec<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub trailer_streams: Vec<Stream>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

/// An episode/entry inside `meta.videos`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Video {
    pub id: String,
    /// Display title. Cinemeta and many addons send `name`; others send
    /// `title`. Prefer `name` when both are present (see [`Video::label`]).
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub released: Option<String>,
    #[serde(default)]
    pub season: Option<u32>,
    #[serde(default)]
    pub episode: Option<u32>,
    /// Position within the season; some addons send `number` instead of
    /// (or in addition to) `episode`.
    #[serde(default)]
    pub number: Option<u32>,
    #[serde(default)]
    pub overview: Option<String>,
    #[serde(default)]
    pub thumbnail: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

impl Video {
    /// The display title: the `name` key (Cinemeta-style), or the `title`
    /// key when an addon only sends that.
    pub fn label(&self) -> String {
        if !self.name.is_empty() {
            self.name.clone()
        } else {
            self.title.clone()
        }
    }

    /// The episode position within its season, whatever the addon called it.
    pub fn episode_number(&self) -> Option<u32> {
        self.episode.or(self.number)
    }
}

/// One item of `GET /stream/{type}/{id}.json` — a possible way to watch.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Stream {
    /// Display name. Some addons emit `title` (and some emit both, so the two
    /// must not share a serde alias — see [`Stream::label`]).
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, rename = "title")]
    pub title_legacy: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// Direct playable URL (HLS/MP4/…).
    #[serde(default)]
    pub url: Option<String>,
    /// YouTube video id.
    #[serde(default)]
    pub yt_id: Option<String>,
    /// BitTorrent magnet-less infohash; combine with a tracker or a torrent
    /// client to play.
    #[serde(default)]
    pub info_hash: Option<String>,
    #[serde(default)]
    pub file_idx: Option<u32>,
    #[serde(default)]
    pub subtitles: Vec<Subtitle>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

impl Stream {
    /// A label suitable for a UI row (falls back to a placeholder).
    pub fn label(&self) -> String {
        self.name
            .clone()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| self.title_legacy.clone().filter(|s| !s.trim().is_empty()))
            .unwrap_or_else(|| {
                if self.url.is_some() {
                    "Stream".into()
                } else if self.info_hash.is_some() {
                    "Torrent".into()
                } else if self.yt_id.is_some() {
                    "YouTube".into()
                } else {
                    "Stream".into()
                }
            })
    }

    /// A URL that can be opened directly in a browser/player, if the addon
    /// provides one (direct URL or YouTube).
    pub fn web_url(&self) -> Option<String> {
        if let Some(url) = &self.url {
            return Some(url.clone());
        }
        self.yt_id
            .as_ref()
            .map(|id| format!("https://www.youtube.com/watch?v={id}"))
    }
}

/// A subtitle track attached to a [`Stream`].
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Subtitle {
    pub id: String,
    pub url: String,
    #[serde(default)]
    pub lang: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

fn stringify(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}
