use crate::{ApiError, Projection, Resume};
use base64::{Engine, engine::general_purpose::STANDARD};
use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
};

pub const MAX_VIDEOS: usize = 100_000;
const MAX_BITFIELD_BYTES: usize = MAX_VIDEOS / 8;

pub fn released_timestamp_ms(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|d| d.timestamp_millis())
        .or_else(|| {
            chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .ok()?
                .and_hms_opt(0, 0, 0)
                .map(|d| d.and_utc().timestamp_millis())
        })
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Video {
    pub id: String,
    pub season: Option<u32>,
    pub episode: Option<u32>,
    pub released_ms: Option<i64>,
}
fn sorted(videos: &[Video]) -> Result<Vec<&Video>, ApiError> {
    if videos.len() > MAX_VIDEOS || videos.iter().any(|v| v.id.is_empty() || v.id.len() > 4096) {
        return Err(ApiError::InvalidInput);
    }
    let mut videos: Vec<_> = videos.iter().collect();
    videos.sort_by_key(|v| (v.season, v.episode, v.released_ms));
    let mut seen = std::collections::HashSet::new();
    if videos.iter().any(|v| !seen.insert(&v.id)) {
        return Err(ApiError::InvalidInput);
    }
    Ok(videos)
}

/// Keeps every unknown item/state property when projecting Nova-owned fields.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LibraryItem(pub Map<String, Value>);
impl LibraryItem {
    pub fn new(id: &str, name: &str, media_type: &str, modified_ms: u64) -> Self {
        let mut item=Self(json!({"_id":id,"name":name,"type":media_type,"removed":false,"temp":false,"posterShape":"poster","state": {"timeWatched":0,"overallTimeWatched":0,"timeOffset":0,"duration":0,"timesWatched":0,"flaggedWatched":0}}).as_object().unwrap().clone());
        item.set_modified(modified_ms);
        item
    }
    pub fn id(&self) -> &str {
        self.0
            .get("_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }
    pub fn name(&self) -> &str {
        self.0
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }
    pub fn media_type(&self) -> &str {
        self.0
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }
    pub fn modified_ms(&self) -> u64 {
        self.0
            .get("_mtime")
            .and_then(|v| {
                v.as_u64().or_else(|| {
                    v.as_str()
                        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                        .and_then(|d| u64::try_from(d.timestamp_millis()).ok())
                })
            })
            .unwrap_or_default()
    }
    fn set_modified(&mut self, now: u64) {
        if let Some(date) = i64::try_from(now)
            .ok()
            .and_then(chrono::DateTime::from_timestamp_millis)
        {
            self.0.insert(
                "_mtime".into(),
                json!(date.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
            );
        }
    }
    pub fn projection(&self, videos: &[Video]) -> Result<Projection, ApiError> {
        if self.id().is_empty() {
            return Err(ApiError::InvalidResponse);
        }
        let state = self
            .0
            .get("state")
            .and_then(Value::as_object)
            .ok_or(ApiError::InvalidResponse)?;
        let mut watched = if let Some(encoded) = state
            .get("watched")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            decode_watched(encoded, videos)?
        } else {
            sorted(videos)?
                .into_iter()
                .map(|v| (v.id.clone(), false))
                .collect()
        };
        if videos.is_empty() && self.media_type() == "movie" {
            watched.insert(
                self.id().into(),
                state
                    .get("timesWatched")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    > 0,
            );
        }
        let position = state.get("timeOffset").and_then(Value::as_u64).unwrap_or(0);
        let duration = state.get("duration").and_then(Value::as_u64).unwrap_or(0);
        let resume = if position > 0 {
            Some(Resume {
                video_id: state
                    .get("video_id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .unwrap_or(self.id())
                    .into(),
                position_ms: position,
                duration_ms: duration,
            })
        } else {
            None
        };
        Ok(Projection {
            saved: !self
                .0
                .get("removed")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                && !self.0.get("temp").and_then(Value::as_bool).unwrap_or(false),
            watched,
            resume,
        })
    }
    pub fn apply_projection(
        &mut self,
        value: &Projection,
        videos: &[Video],
        now_ms: u64,
    ) -> Result<(), ApiError> {
        let encoded = if !videos.is_empty() {
            Some(encode_watched(&value.watched, videos)?)
        } else {
            None
        };
        let movie_watched = value.watched.get(self.id()).copied();
        self.0.insert("removed".into(), json!(!value.saved));
        if value.saved {
            self.0.insert("temp".into(), json!(false));
        }
        let state = self
            .0
            .entry("state")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or(ApiError::InvalidResponse)?;
        if let Some(encoded) = encoded {
            state.insert("watched".into(), json!(encoded));
        }
        // Without episode metadata preserve the opaque bitmap verbatim.
        if let Some(watched) = movie_watched.filter(|_| videos.is_empty()) {
            state.insert("timesWatched".into(), json!(u32::from(watched)));
            state.insert("flaggedWatched".into(), json!(u32::from(watched)));
        }
        match &value.resume {
            Some(resume) => {
                state.insert("video_id".into(), json!(resume.video_id));
                state.insert("timeOffset".into(), json!(resume.position_ms));
                state.insert("duration".into(), json!(resume.duration_ms));
            }
            None => {
                state.insert("timeOffset".into(), json!(0));
            }
        }
        self.set_modified(now_ms);
        Ok(())
    }

    /// Change membership without interpreting or rewriting opaque progress.
    pub fn apply_membership(&mut self, saved: bool, modified_ms: u64) {
        self.0.insert("removed".into(), json!(!saved));
        self.0.insert("temp".into(), json!(false));
        self.set_modified(modified_ms);
    }
}

pub fn decode_watched(encoded: &str, videos: &[Video]) -> Result<BTreeMap<String, bool>, ApiError> {
    if encoded.len() > MAX_BITFIELD_BYTES * 2 + 8192 {
        return Err(ApiError::InvalidResponse);
    }
    let (prefix, payload) = encoded.rsplit_once(':').ok_or(ApiError::InvalidResponse)?;
    let (anchor, length) = prefix.rsplit_once(':').ok_or(ApiError::InvalidResponse)?;
    let length: usize = length.parse().map_err(|_| ApiError::InvalidResponse)?;
    if length == 0 || length > MAX_VIDEOS {
        return Err(ApiError::InvalidResponse);
    }
    let compressed = STANDARD
        .decode(payload)
        .map_err(|_| ApiError::InvalidResponse)?;
    let mut bytes = Vec::new();
    ZlibDecoder::new(compressed.as_slice())
        .take(MAX_BITFIELD_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ApiError::InvalidResponse)?;
    if bytes.len() > MAX_BITFIELD_BYTES {
        return Err(ApiError::InvalidResponse);
    }
    let videos = sorted(videos)?;
    // Stremio emits an undefined anchor when saving an empty inventory. Once
    // episodes become available, an empty bitmap still proves every bit false
    // without needing an anchor to align it. Nonempty history stays opaque
    // when its anchor is missing so a metadata change cannot erase watched bits.
    let Some(anchor_index) = videos.iter().position(|v| v.id == anchor) else {
        if bytes.iter().all(|byte| *byte == 0) {
            return Ok(videos
                .iter()
                .map(|video| (video.id.clone(), false))
                .collect());
        }
        return Err(ApiError::InvalidResponse);
    };
    let offset = length as i64 - anchor_index as i64 - 1;
    Ok(videos
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let old = i as i64 + offset;
            let watched = usize::try_from(old)
                .ok()
                .filter(|old| *old < videos.len())
                .and_then(|old| bytes.get(old / 8).map(|byte| byte & (1 << (old % 8)) != 0))
                .unwrap_or(false);
            (v.id.clone(), watched)
        })
        .collect())
}
pub fn encode_watched(
    watched: &BTreeMap<String, bool>,
    videos: &[Video],
) -> Result<String, ApiError> {
    let videos = sorted(videos)?;
    let mut bytes = vec![0u8; videos.len().div_ceil(8)];
    let mut last = 0;
    for (i, video) in videos.iter().enumerate() {
        if watched.get(&video.id).copied().unwrap_or(false) {
            bytes[i / 8] |= 1 << (i % 8);
            last = i;
        }
    }
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(6));
    encoder
        .write_all(&bytes)
        .map_err(|_| ApiError::InvalidInput)?;
    let payload = encoder.finish().map_err(|_| ApiError::InvalidInput)?;
    Ok(format!(
        "{}:{}:{}",
        videos.get(last).map_or("undefined", |v| v.id.as_str()),
        last + 1,
        STANDARD.encode(payload)
    ))
}
