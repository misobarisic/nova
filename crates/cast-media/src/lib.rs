//! Session-owned LAN delivery. Source URLs and credentials never enter receiver payloads.
mod hls;
mod http;
mod ram;
pub use nova_cast::TextTrack;
pub use nova_remux::{Codec, Container};
use std::{
    net::IpAddr,
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
};

/// Safety ceiling for retained remux output. Native and callback fragment buffers
/// remain separately capped at 16 MiB each, regardless of the retained setting.
pub const MAX_RAM_BUFFER_BYTES: usize = 256 * 1024 * 1024;
// Intentionally no Debug/Serialize: source URLs, headers and local paths are private.
#[derive(Clone)]
pub enum SourceDescriptor {
    Http {
        url: String,
        headers: Vec<(String, String)>,
        private: bool,
    },
    LocalFile {
        path: PathBuf,
    },
    LocalHlsBundle {
        root: PathBuf,
        playlist: PathBuf,
    },
    LoopbackTorrent {
        url: String,
    },
}
#[derive(Clone, Debug)]
pub struct OutputProfile {
    pub containers: Vec<Container>,
    pub video_codecs: Vec<Codec>,
    pub audio_codecs: Vec<Codec>,
    pub hls: bool,
    pub codecs_verified: bool,
}
impl Default for OutputProfile {
    fn default() -> Self {
        Self {
            containers: vec![Container::Mp4],
            video_codecs: vec![Codec::H264],
            audio_codecs: vec![Codec::Aac, Codec::Mp3],
            hls: false,
            codecs_verified: false,
        }
    }
}
/// Provider text tracks stay private until rewritten into a session resource.
#[derive(Clone)]
pub struct SourceTextTrack {
    pub id: i64,
    pub name: String,
    pub language: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
}
#[derive(Clone)]
pub struct PrepareOptions {
    pub cache_dir: PathBuf,
    /// Maximum retained packet-copy output; an additional single fragment is bounded separately.
    pub max_buffer_bytes: usize,
    pub start_position: f64,
    pub max_readers: usize,
    /// Native FFmpeg stream index, from mpv track ff-index. A non-default selection requires a selected-audio remux.
    pub selected_audio_stream: Option<usize>,
    /// Explicit user opt-in to try known codecs absent from receiver advertisements.
    pub allow_unverified: bool,
    pub text_tracks: Vec<SourceTextTrack>,
}
impl Default for PrepareOptions {
    fn default() -> Self {
        Self {
            cache_dir: std::env::temp_dir().join("nova-cast"),
            max_buffer_bytes: 64 * 1024 * 1024,
            start_position: 0.0,
            max_readers: 8,
            selected_audio_stream: None,
            allow_unverified: false,
            text_tracks: Vec::new(),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryMode {
    DirectRelay,
    PacketCopyHls,
    HlsRelay,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaError {
    Cancelled,
    InvalidSource,
    Network,
    SourceUnavailable,
    UnsupportedCodec,
    UnverifiedFormat,
    UnsupportedContainer,
    UnsupportedHls,
    InvalidTimestamps,
    ResourceLimit,
    Bind,
    Native,
}
impl std::fmt::Display for MediaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {Self::Cancelled=>"Casting was cancelled",Self::InvalidSource=>"This source cannot be shared with the receiver",Self::Network=>"The media source could not be reached",Self::SourceUnavailable=>"The media source is unavailable",Self::UnsupportedCodec=>"This receiver does not support the source codecs; choose another source or play locally",Self::UnverifiedFormat=>"This source format has not been verified for this receiver; choose another source or play locally",Self::UnsupportedContainer=>"This receiver does not support this media format; choose another source or play locally",Self::UnsupportedHls=>"This HLS source cannot be safely shared; choose another source or play locally",Self::InvalidTimestamps=>"This source cannot be remuxed safely; choose another source or play locally",Self::ResourceLimit=>"The casting preparation limit was reached; choose another source or play locally",Self::Bind=>"A local network route to the receiver is unavailable",Self::Native=>"The media format could not be inspected; choose another source or play locally"})
    }
}
impl std::error::Error for MediaError {}
impl From<nova_remux::Error> for MediaError {
    fn from(e: nova_remux::Error) -> Self {
        match e {
            nova_remux::Error::Cancelled => Self::Cancelled,
            nova_remux::Error::OutputLimit => Self::ResourceLimit,
            nova_remux::Error::Native(-1094995529) => Self::InvalidTimestamps,
            _ => Self::Native,
        }
    }
}
pub struct PreparedMedia {
    pub receiver_url: String,
    pub mime_type: String,
    pub duration: Option<f64>,
    /// Source position = receiver position + timeline_offset. Duration stays source-total.
    pub timeline_offset: f64,
    pub seekable: bool,
    pub text_tracks: Vec<TextTrack>,
    /// Optional text-track errors do not stop video preparation.
    pub text_track_warnings: Vec<(i64, MediaError)>,
    pub mode: DeliveryMode,
    pub guard: MediaGuard,
}
/// Keep this guard in the cast session, independently of the picker/activity UI.
/// Dropping it cancels delivery, closes the listener and releases private artifacts.
pub struct MediaGuard {
    inner: Arc<http::Session>,
}
impl MediaGuard {
    pub fn cancel(&self) {
        self.inner.cancel();
    }
    /// Receiver-relative, confirmed playback position; also set before an initial LOAD.
    pub fn set_playhead(&self, position: f64) {
        self.inner.set_playhead(position);
    }
    pub fn delivery_error(&self) -> Option<MediaError> {
        self.inner.delivery_error()
    }
}
impl Drop for MediaGuard {
    fn drop(&mut self) {
        self.inner.cancel();
    }
}
/// Blocking preparation: invoke from a worker, never the UI/event-loop thread.
pub fn prepare(
    source: SourceDescriptor,
    receiver_ip: IpAddr,
    profile: OutputProfile,
    options: PrepareOptions,
    cancel: Arc<AtomicBool>,
) -> Result<PreparedMedia, MediaError> {
    // The caller's token is read-only. A session guard revokes only its own routes;
    // replacing prepared media must not cancel the app worker or its next attempt.
    let owned = owned_cancellation(cancel)?;
    let result = http::prepare(source, receiver_ip, profile, options, owned.clone());
    if result.is_err() {
        owned.store(true, std::sync::atomic::Ordering::Release);
    }
    result
}
fn owned_cancellation(parent: Arc<AtomicBool>) -> Result<Arc<AtomicBool>, MediaError> {
    use std::sync::atomic::Ordering;
    let owned = Arc::new(AtomicBool::new(parent.load(Ordering::Acquire)));
    let watcher = owned.clone();
    std::thread::Builder::new()
        .name("cast-media-cancellation".into())
        .spawn(move || {
            while !watcher.load(Ordering::Acquire) {
                if parent.load(Ordering::Acquire) {
                    watcher.store(true, Ordering::Release);
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
        })
        .map_err(|_| MediaError::ResourceLimit)?;
    Ok(owned)
}
