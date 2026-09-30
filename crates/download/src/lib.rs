use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use reqwest::Url;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

mod http;

pub use http::{
    download_http, download_http_for_job, download_http_for_job_blocking, download_http_resume,
    download_http_with_options, download_http_with_request, is_manifest_content_type,
    is_manifest_url, new_http_client,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DownloadSource {
    #[serde(alias = "Http")]
    Http { url: String },
    #[serde(alias = "Torrent")]
    Torrent {
        info_hash: String,
        #[serde(default)]
        file_idx: Option<u32>,
    },
}

impl Default for DownloadSource {
    fn default() -> Self {
        Self::Http { url: String::new() }
    }
}

impl DownloadSource {
    pub fn http(url: impl Into<String>) -> Self {
        Self::Http { url: url.into() }
    }

    pub fn torrent(info_hash: impl Into<String>, file_idx: Option<u32>) -> Self {
        Self::Torrent {
            info_hash: info_hash.into(),
            file_idx,
        }
    }

    pub fn url(&self) -> Option<&str> {
        match self {
            Self::Http { url } => Some(url),
            Self::Torrent { .. } => None,
        }
    }

    pub fn is_http(&self) -> bool {
        matches!(self, Self::Http { .. })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DownloadPhase {
    #[default]
    #[serde(alias = "Queued")]
    Queued,
    #[serde(alias = "Resolving")]
    Resolving,
    #[serde(alias = "Downloading")]
    Downloading,
    #[serde(alias = "Paused")]
    Paused,
    #[serde(alias = "Completed")]
    Completed,
    #[serde(alias = "Failed")]
    Failed,
}

impl DownloadPhase {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DownloadJob {
    pub id: String,
    pub media_type: String,
    pub media_id: String,
    pub request_id: String,
    pub title: String,
    pub year: Option<String>,
    pub display: String,
    pub addon: String,
    pub source: DownloadSource,
    pub created_at: u64,
    pub updated_at: u64,
    pub phase: DownloadPhase,
    pub bytes_downloaded: u64,
    pub total_bytes: Option<u64>,
    pub bytes_per_second: u64,
    pub artifact_path: Option<PathBuf>,
    pub file_name: Option<String>,
    pub error: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

impl Default for DownloadJob {
    fn default() -> Self {
        Self {
            id: String::new(),
            media_type: String::new(),
            media_id: String::new(),
            request_id: String::new(),
            title: String::new(),
            year: None,
            display: String::new(),
            addon: String::new(),
            source: DownloadSource::default(),
            created_at: 0,
            updated_at: 0,
            phase: DownloadPhase::Queued,
            bytes_downloaded: 0,
            total_bytes: None,
            bytes_per_second: 0,
            artifact_path: None,
            file_name: None,
            error: None,
            etag: None,
            last_modified: None,
        }
    }
}

impl DownloadJob {
    pub fn new(id: impl Into<String>, source: DownloadSource) -> Self {
        let now = unix_timestamp();
        Self {
            id: id.into(),
            created_at: now,
            updated_at: now,
            source,
            ..Self::default()
        }
    }

    pub fn http_request(
        &self,
        destination: impl Into<PathBuf>,
    ) -> Result<HttpDownloadRequest, DownloadError> {
        let url = self.source.url().ok_or_else(|| {
            DownloadError::InvalidSource("torrent downloads are not HTTP downloads".into())
        })?;
        let resume = DownloadResume {
            bytes_downloaded: self.bytes_downloaded,
            etag: self.etag.clone(),
            last_modified: self.last_modified.clone(),
        };
        Ok(HttpDownloadRequest {
            url: url.to_string(),
            destination: destination.into(),
            resume: (!resume.is_empty()).then_some(resume),
        })
    }

    pub fn resume_state(&self) -> DownloadResume {
        DownloadResume {
            bytes_downloaded: self.bytes_downloaded,
            etag: self.etag.clone(),
            last_modified: self.last_modified.clone(),
        }
    }

    pub fn display_name(&self) -> &str {
        &self.display
    }

    pub fn addon_name(&self) -> &str {
        &self.addon
    }

    pub fn apply_progress(&mut self, progress: &DownloadProgress) {
        self.bytes_downloaded = progress.bytes_downloaded;
        self.total_bytes = progress.total_bytes;
        self.bytes_per_second = progress.bytes_per_second;
        self.phase = DownloadPhase::Downloading;
        self.updated_at = unix_timestamp();
    }

    pub fn apply_outcome(&mut self, outcome: &DownloadOutcome) {
        self.bytes_downloaded = outcome.bytes_downloaded;
        self.total_bytes = outcome.total_bytes;
        self.bytes_per_second = outcome.bytes_per_second;
        self.artifact_path = Some(outcome.path.clone());
        self.file_name = Some(outcome.file_name.clone());
        self.etag = outcome.etag.clone();
        self.last_modified = outcome.last_modified.clone();
        self.error = None;
        self.phase = DownloadPhase::Completed;
        self.updated_at = unix_timestamp();
    }

    pub fn apply_error(&mut self, error: &DownloadError) {
        self.error = Some(error.to_string());
        self.phase = if error.is_cancelled() {
            DownloadPhase::Paused
        } else {
            DownloadPhase::Failed
        };
        self.updated_at = unix_timestamp();
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DownloadManifest {
    pub version: u32,
    pub jobs: Vec<DownloadJob>,
}

impl Default for DownloadManifest {
    fn default() -> Self {
        Self {
            version: 1,
            jobs: Vec::new(),
        }
    }
}

impl DownloadManifest {
    pub fn new(jobs: Vec<DownloadJob>) -> Self {
        Self { version: 1, jobs }
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DownloadResume {
    pub bytes_downloaded: u64,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

impl DownloadResume {
    pub fn new(bytes_downloaded: u64, etag: Option<String>, last_modified: Option<String>) -> Self {
        Self {
            bytes_downloaded,
            etag,
            last_modified,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.bytes_downloaded == 0 && self.etag.is_none() && self.last_modified.is_none()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpDownloadRequest {
    pub url: String,
    pub destination: PathBuf,
    pub resume: Option<DownloadResume>,
}

impl Default for HttpDownloadRequest {
    fn default() -> Self {
        Self {
            url: String::new(),
            destination: PathBuf::new(),
            resume: None,
        }
    }
}

impl HttpDownloadRequest {
    pub fn new(url: impl Into<String>, destination: impl Into<PathBuf>) -> Self {
        Self {
            url: url.into(),
            destination: destination.into(),
            resume: None,
        }
    }

    pub fn with_resume(mut self, resume: DownloadResume) -> Self {
        self.resume = Some(resume);
        self
    }

    pub fn from_job(
        job: &DownloadJob,
        destination: impl Into<PathBuf>,
    ) -> Result<Self, DownloadError> {
        job.http_request(destination)
    }
}

impl From<(String, PathBuf)> for HttpDownloadRequest {
    fn from((url, destination): (String, PathBuf)) -> Self {
        Self::new(url, destination)
    }
}

impl From<(&str, PathBuf)> for HttpDownloadRequest {
    fn from((url, destination): (&str, PathBuf)) -> Self {
        Self::new(url, destination)
    }
}

pub type DownloadHttpRequest = HttpDownloadRequest;
pub type DownloadHttpOptions = HttpDownloadRequest;
pub type DownloadOptions = HttpDownloadRequest;
pub type DownloadResult = DownloadOutcome;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DownloadProgress {
    pub bytes_downloaded: u64,
    pub total_bytes: Option<u64>,
    pub bytes_per_second: u64,
}

impl DownloadProgress {
    pub fn new(bytes_downloaded: u64, total_bytes: Option<u64>, bytes_per_second: u64) -> Self {
        Self {
            bytes_downloaded,
            total_bytes,
            bytes_per_second,
        }
    }

    pub fn fraction(&self) -> Option<f64> {
        self.total_bytes.map(|total| {
            if total == 0 {
                1.0
            } else {
                (self.bytes_downloaded as f64 / total as f64).clamp(0.0, 1.0)
            }
        })
    }

    pub fn percent(&self) -> Option<f64> {
        self.fraction().map(|fraction| fraction * 100.0)
    }

    pub fn is_complete(&self) -> bool {
        self.total_bytes
            .is_some_and(|total| self.bytes_downloaded >= total)
    }

    pub fn formatted_bytes(&self) -> String {
        format_bytes(self.bytes_downloaded)
    }

    pub fn formatted_speed(&self) -> String {
        format_speed(self.bytes_per_second)
    }

    pub fn formatted_percent(&self) -> String {
        format_percent(self.bytes_downloaded, self.total_bytes)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadOutcome {
    pub path: PathBuf,
    pub part_path: PathBuf,
    pub file_name: String,
    pub bytes_downloaded: u64,
    pub total_bytes: Option<u64>,
    pub bytes_per_second: u64,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

#[derive(Clone, Default)]
pub struct CancellationToken {
    flag: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl fmt::Debug for CancellationToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CancellationToken")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        if !self.flag.swap(true, Ordering::AcqRel) {
            self.notify.notify_waiters();
            self.notify.notify_one();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    pub fn cancelled(&self) -> bool {
        self.is_cancelled()
    }

    pub async fn wait_cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        self.notify.notified().await;
    }
}

pub type CancellationControl = CancellationToken;
pub type DownloadCancellation = CancellationToken;
pub type CancelToken = CancellationToken;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DownloadError {
    Cancelled,
    InvalidUrl(String),
    InvalidSource(String),
    UnsupportedUrl(String),
    UnsupportedContentType(String),
    HttpStatus { status: u16, message: String },
    InvalidResponse(String),
    InvalidResume(String),
    Io(String),
}

impl DownloadError {
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }

    pub fn message(&self) -> String {
        self.to_string()
    }
}

impl fmt::Display for DownloadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("download cancelled"),
            Self::InvalidUrl(url) => write!(formatter, "invalid download URL: {url}"),
            Self::InvalidSource(message) => formatter.write_str(message),
            Self::UnsupportedUrl(url) => write!(formatter, "unsupported download URL: {url}"),
            Self::UnsupportedContentType(content_type) => {
                write!(
                    formatter,
                    "unsupported download content type: {content_type}"
                )
            }
            Self::HttpStatus { status, message } => {
                if message.is_empty() {
                    write!(formatter, "HTTP {status}")
                } else {
                    write!(formatter, "HTTP {status}: {message}")
                }
            }
            Self::InvalidResponse(message) => {
                write!(formatter, "invalid download response: {message}")
            }
            Self::InvalidResume(message) => {
                write!(formatter, "invalid download resume state: {message}")
            }
            Self::Io(message) => write!(formatter, "download I/O error: {message}"),
        }
    }
}

impl std::error::Error for DownloadError {}

impl From<std::io::Error> for DownloadError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

pub fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 10.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn format_speed(bytes_per_second: u64) -> String {
    format!("{}/s", format_bytes(bytes_per_second))
}

pub fn format_percent(downloaded: u64, total: Option<u64>) -> String {
    let Some(total) = total else {
        return "—".into();
    };
    if total == 0 {
        return "100%".into();
    }
    let percent = (downloaded as f64 / total as f64 * 100.0).clamp(0.0, 100.0);
    format!("{percent:.0}%")
}

pub fn format_progress(downloaded: u64, total: Option<u64>) -> String {
    match total {
        Some(total) => format!(
            "{} / {} ({})",
            format_bytes(downloaded),
            format_bytes(total),
            format_percent(downloaded, Some(total))
        ),
        None => format_bytes(downloaded),
    }
}

pub fn sanitize_filename(input: &str) -> String {
    let basename = input.rsplit(['/', '\\']).next().unwrap_or(input);
    let mut cleaned = String::with_capacity(basename.len());
    for character in basename.chars() {
        if character.is_control()
            || matches!(
                character,
                '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
            )
        {
            cleaned.push('_');
        } else {
            cleaned.push(character);
        }
    }
    let trimmed = cleaned
        .trim_matches(|character: char| character == ' ' || character == '.')
        .to_string();
    if trimmed.is_empty() {
        return "download".into();
    }
    let device_stem = trimmed.split('.').next().unwrap_or(trimmed.as_str());
    if is_reserved_name(device_stem) {
        let suffix = trimmed
            .find('.')
            .map(|index| &trimmed[index..])
            .unwrap_or("");
        return limit_filename(&format!("{device_stem}_{suffix}"));
    }
    limit_filename(&trimmed)
}

pub fn sanitize_file_name(input: &str) -> String {
    sanitize_filename(input)
}

pub fn safe_filename(input: &str) -> String {
    sanitize_filename(input)
}

fn split_extension(name: &str) -> (&str, &str) {
    let Some((stem, extension)) = name.rsplit_once('.') else {
        return (name, "");
    };
    if stem.is_empty() || extension.is_empty() || extension.len() > 20 {
        return (name, "");
    }
    (stem, extension)
}

fn is_reserved_name(stem: &str) -> bool {
    let upper = stem.to_ascii_uppercase();
    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (upper.len() == 4
            && (upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.as_bytes()[3].is_ascii_digit())
}

fn limit_filename(name: &str) -> String {
    const LIMIT: usize = 240;
    if name.len() <= LIMIT {
        return name.to_string();
    }
    let (stem, extension) = split_extension(name);
    let suffix = if extension.is_empty() {
        String::new()
    } else {
        format!(".{extension}")
    };
    let available = LIMIT.saturating_sub(suffix.len());
    if available == 0 {
        return "download".into();
    }
    let mut end = available.min(stem.len());
    while end > 0 && !stem.is_char_boundary(end) {
        end -= 1;
    }
    let mut limited = format!("{}{}", &stem[..end], suffix);
    limited = limited.trim_end_matches([' ', '.']).to_string();
    if limited.is_empty() {
        "download".into()
    } else {
        limited
    }
}

pub fn is_reserved_filename(input: &str) -> bool {
    let basename = input.rsplit(['/', '\\']).next().unwrap_or(input);
    let stem = basename.split('.').next().unwrap_or(basename);
    is_reserved_name(stem)
}

pub fn validate_download_url(url: &str) -> Result<Url, DownloadError> {
    let trimmed = url.trim();
    let parsed =
        Url::parse(trimmed).map_err(|error| DownloadError::InvalidUrl(error.to_string()))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(DownloadError::UnsupportedUrl(trimmed.to_string()));
    }
    if is_manifest_url(trimmed) {
        return Err(DownloadError::UnsupportedUrl(trimmed.to_string()));
    }
    Ok(parsed)
}

pub fn part_path(destination: impl AsRef<Path>) -> PathBuf {
    let destination = destination.as_ref();
    let mut part = destination.as_os_str().to_os_string();
    part.push(".part");
    PathBuf::from(part)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_round_trip_and_defaults_are_stable() {
        let source = DownloadSource::torrent("abc", Some(2));
        let json = serde_json::to_string(&source).unwrap();
        assert_eq!(json, "{\"torrent\":{\"info_hash\":\"abc\",\"file_idx\":2}}");
        assert_eq!(
            serde_json::from_str::<DownloadSource>(&json).unwrap(),
            source
        );

        let manifest: DownloadManifest = serde_json::from_str("{}").unwrap();
        assert_eq!(manifest.version, 1);
        assert!(manifest.jobs.is_empty());

        let job: DownloadJob = serde_json::from_str("{}").unwrap();
        assert_eq!(job.phase, DownloadPhase::Queued);
        assert_eq!(job.bytes_downloaded, 0);
        assert_eq!(job.total_bytes, None);
        assert!(job.etag.is_none());
    }

    #[test]
    fn sanitizer_handles_paths_controls_and_reserved_names() {
        assert_eq!(sanitize_filename("../../bad/name?.mp4"), "name_.mp4");
        assert_eq!(sanitize_filename("CON.txt"), "CON_.txt");
        assert_eq!(sanitize_filename("..."), "download");
        assert_eq!(sanitize_filename("a\nb"), "a_b");
        assert!(sanitize_filename(&"x".repeat(400)).len() <= 240);
    }

    #[test]
    fn progress_helpers_are_bounded_and_formatted() {
        let progress = DownloadProgress::new(512, Some(1024), 2048);
        assert_eq!(progress.fraction(), Some(0.5));
        assert_eq!(progress.percent(), Some(50.0));
        assert_eq!(progress.formatted_bytes(), "512 B");
        assert_eq!(progress.formatted_speed(), "2.0 KiB/s");
        assert_eq!(progress.formatted_percent(), "50%");
        assert_eq!(format_percent(2, Some(1)), "100%");
        assert_eq!(format_bytes(0), "0 B");
    }

    #[test]
    fn cancellation_is_shared_by_clones() {
        let token = CancellationToken::new();
        let clone = token.clone();
        assert!(!clone.is_cancelled());
        token.cancel();
        assert!(clone.cancelled());
    }
}
