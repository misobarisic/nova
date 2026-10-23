use crate::*;
use reqwest::{
    blocking::Client,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use std::{
    collections::HashMap,
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    net::{SocketAddr, TcpListener, TcpStream, UdpSocket},
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
    thread,
    time::{Duration, Instant},
};
use url::Url;

pub(crate) fn token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 24];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn check(cancel: &AtomicBool) -> Result<(), MediaError> {
    if cancel.load(Ordering::Acquire) {
        Err(MediaError::Cancelled)
    } else {
        Ok(())
    }
}
pub(crate) struct HttpSource {
    client: Client,
    url: Url,
    headers: HeaderMap,
    pub(crate) len: u64,
    pub(crate) is_hls: bool,
    validator: Option<HeaderValue>,
    modified: Option<HeaderValue>,
    cancel: Arc<AtomicBool>,
    deadline: Option<Instant>,
}
impl HttpSource {
    pub(crate) fn new(
        url: String,
        headers: Vec<(String, String)>,
        cancel: Arc<AtomicBool>,
    ) -> Result<Self, MediaError> {
        let url = Url::parse(&url).map_err(|_| MediaError::InvalidSource)?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(MediaError::InvalidSource);
        }
        let mut map = HeaderMap::new();
        for (name, value) in headers {
            let name =
                HeaderName::from_bytes(name.as_bytes()).map_err(|_| MediaError::InvalidSource)?;
            if matches!(
                name.as_str(),
                "host" | "range" | "content-length" | "connection" | "transfer-encoding"
            ) {
                continue;
            }
            map.insert(
                name,
                HeaderValue::from_str(&value).map_err(|_| MediaError::InvalidSource)?,
            );
        }
        map.insert("accept-encoding", HeaderValue::from_static("identity"));
        let builder = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(8))
            .timeout(Duration::from_secs(20));
        // Android's platform verifier requires an unavailable Kotlin/JNI runtime.
        // Match nova-media/download: verified Mozilla roots, no platform verifier.
        #[cfg(target_os = "android")]
        let builder = {
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::aws_lc_rs::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .map_err(|_| MediaError::Network)?
            .with_root_certificates(roots)
            .with_no_client_auth();
            builder.tls_backend_preconfigured(tls)
        };
        let client = builder.build().map_err(|_| MediaError::Network)?;
        Ok(Self {
            client,
            url,
            headers: map,
            len: 0,
            validator: None,
            modified: None,
            is_hls: false,
            cancel,
            deadline: None,
        })
    }
    pub(crate) fn request(
        &self,
        range: Option<(u64, u64)>,
    ) -> Result<reqwest::blocking::Response, MediaError> {
        check(&self.cancel)?;
        if self.deadline.is_some_and(|d| Instant::now() > d) {
            return Err(MediaError::ResourceLimit);
        }
        let mut url = self.url.clone();
        let mut headers = self.headers.clone();
        for _ in 0..=5 {
            let mut request = self.client.get(url.clone()).headers(headers.clone());
            if let Some((start, end)) = range {
                request = request.header("range", format!("bytes={start}-{end}"));
            }
            if let Some(value) = &self.validator {
                request = request.header("if-match", value.clone());
            } else if let Some(value) = &self.modified {
                request = request.header("if-unmodified-since", value.clone());
            }
            let response = request.send().map_err(|_| MediaError::Network)?;
            if response.status().is_redirection() {
                let location = response
                    .headers()
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .ok_or(MediaError::SourceUnavailable)?;
                let next = url.join(location).map_err(|_| MediaError::InvalidSource)?;
                if !matches!(next.scheme(), "http" | "https")
                    || !next.username().is_empty()
                    || next.password().is_some()
                    || (url.scheme() == "https" && next.scheme() != "https")
                {
                    return Err(MediaError::InvalidSource);
                }
                if url.origin() != next.origin() {
                    headers.clear();
                    headers.insert("accept-encoding", HeaderValue::from_static("identity"));
                }
                url = next;
                continue;
            }
            if !response.status().is_success() {
                return Err(MediaError::SourceUnavailable);
            }
            return Ok(response);
        }
        Err(MediaError::Network)
    }
    pub(crate) fn bounded_bytes(&self, max: usize) -> Result<Vec<u8>, MediaError> {
        let response = self.request(None)?;
        let mut bytes = Vec::new();
        response
            .take(max as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| MediaError::Network)?;
        if bytes.len() > max {
            return Err(MediaError::ResourceLimit);
        }
        Ok(bytes)
    }
    pub(crate) fn inspect(mut self) -> Result<Self, MediaError> {
        let response = self.request(Some((0, 0)))?;
        self.is_hls = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| {
                matches!(
                    v.split(';')
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_ascii_lowercase()
                        .as_str(),
                    "application/vnd.apple.mpegurl"
                        | "application/x-mpegurl"
                        | "audio/mpegurl"
                        | "audio/x-mpegurl"
                )
            });
        if self.is_hls {
            return Ok(self);
        }
        if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            return Err(MediaError::SourceUnavailable);
        }
        let value = response
            .headers()
            .get("content-range")
            .and_then(|v| v.to_str().ok())
            .ok_or(MediaError::SourceUnavailable)?;
        let (start, end, len) = parse_content_range(value).ok_or(MediaError::SourceUnavailable)?;
        if start != 0 || end != 0 || len == 0 {
            return Err(MediaError::SourceUnavailable);
        }
        self.len = len;
        self.modified = response.headers().get("last-modified").cloned();
        self.validator = response
            .headers()
            .get("etag")
            .filter(|v| !v.as_bytes().starts_with(b"W/"))
            .cloned();
        Ok(self)
    }
}
fn parse_content_range(value: &str) -> Option<(u64, u64, u64)> {
    let range = value.strip_prefix("bytes ")?;
    let (range, len) = range.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()?, len.parse().ok()?))
}
struct RemoteReader {
    source: Arc<HttpSource>,
    position: u64,
    cache_start: u64,
    cache: Vec<u8>,
}
impl RemoteReader {
    fn new(source: Arc<HttpSource>) -> Self {
        Self {
            source,
            position: 0,
            cache_start: 0,
            cache: Vec::new(),
        }
    }
}
impl Read for RemoteReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.source.cancel.load(Ordering::Acquire) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        if self.position >= self.source.len || buf.is_empty() {
            return Ok(0);
        }
        if self.position < self.cache_start
            || self.position >= self.cache_start + self.cache.len() as u64
        {
            let end = (self.position + 512 * 1024 - 1).min(self.source.len - 1);
            let response = self
                .source
                .request(Some((self.position, end)))
                .map_err(io::Error::other)?;
            if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
                return Err(io::ErrorKind::InvalidData.into());
            }
            let actual = response
                .headers()
                .get("content-range")
                .and_then(|v| v.to_str().ok())
                .and_then(parse_content_range);
            if actual != Some((self.position, end, self.source.len)) {
                return Err(io::ErrorKind::InvalidData.into());
            }
            self.cache.clear();
            response
                .take(end - self.position + 2)
                .read_to_end(&mut self.cache)?;
            if self.cache.len() as u64 != end - self.position + 1 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            self.cache_start = self.position;
        }
        let offset = (self.position - self.cache_start) as usize;
        let count = buf.len().min(self.cache.len() - offset);
        buf[..count].copy_from_slice(&self.cache[offset..offset + count]);
        self.position += count as u64;
        Ok(count)
    }
}
impl Seek for RemoteReader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let next = match pos {
            SeekFrom::Start(p) => i128::from(p),
            SeekFrom::End(n) => i128::from(self.source.len) + i128::from(n),
            SeekFrom::Current(n) => i128::from(self.position) + i128::from(n),
        };
        if !(0..=i128::from(u64::MAX)).contains(&next) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        self.position = next as u64;
        Ok(self.position)
    }
}

#[derive(Clone)]
pub(crate) enum Resource {
    File {
        path: PathBuf,
        mime: String,
        len: u64,
    },
    Remote {
        source: Arc<HttpSource>,
        mime: String,
    },
    Bytes {
        data: Vec<u8>,
        mime: String,
    },
    Shared {
        data: Arc<Vec<u8>>,
        mime: String,
    },
    RamPlaylist {
        store: Arc<crate::ram::Store>,
    },
}
impl Resource {
    fn len(&self) -> u64 {
        match self {
            Self::File { len, .. } => *len,
            Self::Remote { source, .. } => source.len,
            Self::Bytes { data, .. } => data.len() as u64,
            Self::Shared { data, .. } => data.len() as u64,
            Self::RamPlaylist { .. } => 0,
        }
    }
    fn mime(&self) -> &str {
        match self {
            Self::File { mime, .. }
            | Self::Remote { mime, .. }
            | Self::Bytes { mime, .. }
            | Self::Shared { mime, .. } => mime,
            Self::RamPlaylist { .. } => "application/vnd.apple.mpegurl",
        }
    }
}
pub(crate) struct Session {
    cancel: Arc<AtomicBool>,
    resources: HashMap<String, Resource>,
    prefix: String,
    active: AtomicUsize,
    max_readers: usize,
    _temporary: Option<tempfile::TempDir>,
}
impl Session {
    pub(crate) fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
        for resource in self.resources.values() {
            if let Resource::RamPlaylist { store } = resource {
                store.wake();
            }
        }
    }
    pub(crate) fn set_playhead(&self, position: f64) {
        for resource in self.resources.values() {
            if let Resource::RamPlaylist { store } = resource {
                store.set_playhead(position);
            }
        }
    }
    pub(crate) fn delivery_error(&self) -> Option<MediaError> {
        self.resources.values().find_map(|resource| {
            if let Resource::RamPlaylist { store } = resource {
                store.error()
            } else {
                None
            }
        })
    }
}

fn mime(container: Container, video: bool) -> &'static str {
    match container {
        Container::Mp4 => {
            if video {
                "video/mp4"
            } else {
                "audio/mp4"
            }
        }
        Container::Webm => {
            if video {
                "video/webm"
            } else {
                "audio/webm"
            }
        }
        Container::Matroska => "video/x-matroska",
        Container::MpegTs => "video/mp2t",
        Container::Mp3 => "audio/mpeg",
        Container::Ogg => "application/ogg",
        Container::Unknown => "application/octet-stream",
    }
}
pub(crate) fn supported(probe: &nova_remux::Probe, profile: &OutputProfile) -> bool {
    probe
        .video
        .is_none_or(|c| c != Codec::Unknown && profile.video_codecs.contains(&c))
        && probe
            .audio
            .is_none_or(|c| c != Codec::Unknown && profile.audio_codecs.contains(&c))
        && (probe.video.is_some() || probe.audio.is_some())
}
pub(crate) fn validate_codecs(
    probe: &nova_remux::Probe,
    profile: &OutputProfile,
    allow_unverified: bool,
) -> Result<(), MediaError> {
    if probe.video == Some(Codec::Unknown)
        || probe.audio == Some(Codec::Unknown)
        || (probe.video.is_none() && probe.audio.is_none())
    {
        return Err(MediaError::UnsupportedCodec);
    }
    if supported(probe, profile) || (!profile.codecs_verified && allow_unverified) {
        Ok(())
    } else {
        Err(if profile.codecs_verified {
            MediaError::UnsupportedCodec
        } else {
            MediaError::UnverifiedFormat
        })
    }
}
fn route(receiver: IpAddr) -> Result<(TcpListener, String), MediaError> {
    if receiver.is_loopback() || receiver.is_unspecified() || receiver.is_multicast() {
        return Err(MediaError::Bind);
    }
    let udp = UdpSocket::bind(SocketAddr::new(
        if receiver.is_ipv4() {
            "0.0.0.0".parse().unwrap()
        } else {
            "::".parse().unwrap()
        },
        0,
    ))
    .map_err(|_| MediaError::Bind)?;
    udp.connect(SocketAddr::new(receiver, 9))
        .map_err(|_| MediaError::Bind)?;
    let local = udp.local_addr().map_err(|_| MediaError::Bind)?.ip();
    if local.is_loopback() || local.is_unspecified() {
        return Err(MediaError::Bind);
    }
    let listener = TcpListener::bind(SocketAddr::new(local, 0)).map_err(|_| MediaError::Bind)?;
    let address = listener.local_addr().map_err(|_| MediaError::Bind)?;
    listener
        .set_nonblocking(true)
        .map_err(|_| MediaError::Bind)?;
    Ok((listener, format!("http://{address}")))
}
pub(crate) fn prepare(
    source: SourceDescriptor,
    receiver: IpAddr,
    profile: OutputProfile,
    options: PrepareOptions,
    cancel: Arc<AtomicBool>,
) -> Result<PreparedMedia, MediaError> {
    check(&cancel)?;
    let (listener, base) = route(receiver)?;
    let prefix = token();
    let main = token();
    let base = format!("{base}/{prefix}");
    let mut resources = HashMap::new();
    let (resource, probe, mode) = match source {
        SourceDescriptor::LocalHlsBundle { root, playlist } => {
            return crate::hls::prepare_bundle(
                root, playlist, profile, options, cancel, listener, base, prefix,
            );
        }
        SourceDescriptor::Http {
            url,
            headers,
            private: _,
        } if Url::parse(&url)
            .ok()
            .is_some_and(|u| u.path().to_ascii_lowercase().ends_with(".m3u8")) =>
        {
            return crate::hls::prepare_remote(
                url, headers, profile, options, cancel, listener, base, prefix,
            );
        }
        SourceDescriptor::LocalFile { path } => {
            if path
                .extension()
                .is_some_and(|s| s.eq_ignore_ascii_case("m3u8"))
            {
                return Err(MediaError::UnsupportedHls);
            }
            let mut file = File::open(&path).map_err(|_| MediaError::SourceUnavailable)?;
            let info = file.metadata().map_err(|_| MediaError::SourceUnavailable)?;
            if !info.is_file() {
                return Err(MediaError::InvalidSource);
            }
            let probe = nova_remux::probe_reader_selected(
                &mut file,
                &cancel,
                options.selected_audio_stream,
            )?;
            validate_codecs(&probe, &profile, options.allow_unverified)?;
            if (options.selected_audio_stream.is_none()
                || options.selected_audio_stream == probe.default_audio_stream)
                && profile.containers.contains(&probe.container)
            {
                (
                    Resource::File {
                        path,
                        mime: mime(probe.container, probe.video.is_some()).into(),
                        len: info.len(),
                    },
                    probe,
                    DeliveryMode::DirectRelay,
                )
            } else {
                return crate::ram::prepare(
                    Box::new(file),
                    probe,
                    profile,
                    options,
                    cancel,
                    listener,
                    base,
                    prefix,
                    main,
                );
            }
        }
        source => {
            let (url, headers) = match source {
                SourceDescriptor::Http { url, headers, .. } => (url, headers),
                SourceDescriptor::LoopbackTorrent { url } => {
                    let parsed = Url::parse(&url).map_err(|_| MediaError::InvalidSource)?;
                    let parts: Vec<_> = parsed.path().trim_start_matches('/').split('/').collect();
                    let file_route = parts.len() == 4
                        && parts[0] == "torrents"
                        && !parts[1].is_empty()
                        && parts[1].bytes().all(|b| b.is_ascii_hexdigit())
                        && parts[2] == "stream"
                        && parts[3].parse::<usize>().is_ok();
                    if parsed.scheme() != "http"
                        || parsed.query().is_some()
                        || !parsed
                            .host_str()
                            .and_then(|h| h.parse::<IpAddr>().ok())
                            .is_some_and(|ip| ip.is_loopback())
                        || !file_route
                    {
                        return Err(MediaError::InvalidSource);
                    }
                    (url, vec![])
                }
                _ => return Err(MediaError::InvalidSource),
            };
            let remote =
                HttpSource::new(url.clone(), headers.clone(), cancel.clone())?.inspect()?;
            if remote.is_hls {
                return crate::hls::prepare_remote(
                    url, headers, profile, options, cancel, listener, base, prefix,
                );
            }
            let remote = Arc::new(remote);
            let mut reader = RemoteReader::new(remote.clone());
            let mut signature = [0u8; 7];
            if reader.read_exact(&mut signature).is_ok() && signature == *b"#EXTM3U" {
                return crate::hls::prepare_remote(
                    url, headers, profile, options, cancel, listener, base, prefix,
                );
            }
            let probe = nova_remux::probe_reader_selected(
                &mut reader,
                &cancel,
                options.selected_audio_stream,
            )?;
            validate_codecs(&probe, &profile, options.allow_unverified)?;
            if (options.selected_audio_stream.is_none()
                || options.selected_audio_stream == probe.default_audio_stream)
                && profile.containers.contains(&probe.container)
            {
                (
                    Resource::Remote {
                        source: remote,
                        mime: mime(probe.container, probe.video.is_some()).into(),
                    },
                    probe,
                    DeliveryMode::DirectRelay,
                )
            } else {
                return crate::ram::prepare(
                    Box::new(reader),
                    probe,
                    profile,
                    options,
                    cancel,
                    listener,
                    base,
                    prefix,
                    main,
                );
            }
        }
    };
    let mime = resource.mime().to_owned();
    resources.insert(main.clone(), resource);
    let (text_tracks, text_track_warnings) = prepare_text_tracks(
        &options.text_tracks,
        &mut resources,
        &base,
        0.0,
        cancel.clone(),
    );
    let mut prepared = start(
        listener,
        base,
        main,
        mime,
        probe.duration,
        true,
        mode,
        resources,
        prefix,
        cancel,
        options.max_readers,
        None,
    )?;
    prepared.text_tracks = text_tracks;
    prepared.text_track_warnings = text_track_warnings;
    Ok(prepared)
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn start(
    listener: TcpListener,
    base: String,
    main: String,
    mime_type: String,
    duration: Option<f64>,
    seekable: bool,
    mode: DeliveryMode,
    resources: HashMap<String, Resource>,
    prefix: String,
    cancel: Arc<AtomicBool>,
    max_readers: usize,
    temporary: Option<tempfile::TempDir>,
) -> Result<PreparedMedia, MediaError> {
    check(&cancel)?;
    let session = Arc::new(Session {
        cancel,
        resources,
        prefix,
        active: AtomicUsize::new(0),
        max_readers: max_readers.clamp(1, 8),
        _temporary: temporary,
    });
    let worker = session.clone();
    thread::Builder::new()
        .name("cast-media-listener".into())
        .spawn(move || {
            while !worker.cancel.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        if worker.active.fetch_add(1, Ordering::AcqRel) >= worker.max_readers {
                            worker.active.fetch_sub(1, Ordering::AcqRel);
                            let _ = simple(&mut stream, 503, "Service Unavailable", None);
                            continue;
                        }
                        let request = worker.clone();
                        if thread::Builder::new()
                            .name("cast-media-reader".into())
                            .spawn(move || {
                                let _ = serve(&mut stream, &request);
                                request.active.fetch_sub(1, Ordering::AcqRel);
                            })
                            .is_err()
                        {
                            worker.active.fetch_sub(1, Ordering::AcqRel);
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(25))
                    }
                    Err(_) => break,
                }
            }
        })
        .map_err(|_| MediaError::Bind)?;
    Ok(PreparedMedia {
        receiver_url: format!("{base}/{main}"),
        mime_type,
        duration,
        timeline_offset: 0.0,
        seekable,
        text_tracks: Vec::new(),
        text_track_warnings: Vec::new(),
        mode,
        guard: MediaGuard { inner: session },
    })
}
/// Fetch optional text documents into private bounded memory. No source URL survives.
pub(crate) fn prepare_text_tracks(
    tracks: &[SourceTextTrack],
    resources: &mut HashMap<String, Resource>,
    base: &str,
    timeline_offset: f64,
    cancel: Arc<AtomicBool>,
) -> (Vec<TextTrack>, Vec<(i64, MediaError)>) {
    let mut prepared = Vec::new();
    let mut warnings = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(45);
    for track in tracks.iter().take(8) {
        let result = (|| {
            check(&cancel)?;
            if Instant::now() > deadline {
                return Err(MediaError::ResourceLimit);
            }
            let source = HttpSource::new(track.url.clone(), track.headers.clone(), cancel.clone())?;
            let bytes = source.bounded_bytes(2 * 1024 * 1024)?;
            let data = rebase_text(&text_document(&bytes)?, timeline_offset)?;
            if data.len() > 2 * 1024 * 1024 {
                return Err(MediaError::ResourceLimit);
            }
            let id = token();
            resources.insert(
                id.clone(),
                Resource::Bytes {
                    data,
                    mime: "text/vtt; charset=utf-8".into(),
                },
            );
            Ok(TextTrack {
                id: track.id,
                url: format!("{base}/{id}"),
                name: track.name.clone(),
                language: track.language.clone(),
                mime_type: "text/vtt".into(),
            })
        })();
        match result {
            Ok(track) => prepared.push(track),
            Err(error) => warnings.push((track.id, error)),
        }
    }
    if let Some(track) = tracks.get(8) {
        warnings.push((track.id, MediaError::ResourceLimit));
    }
    (prepared, warnings)
}
fn vtt_time(value: &str) -> Result<i64, MediaError> {
    let fields: Vec<_> = value.split(':').collect();
    let (hours, minutes, seconds) = match fields.as_slice() {
        [minutes, seconds] => ("0", *minutes, *seconds),
        [hours, minutes, seconds] => (*hours, *minutes, *seconds),
        _ => return Err(MediaError::UnsupportedContainer),
    };
    let parse = |s: &str| {
        s.parse::<i64>()
            .ok()
            .filter(|_| !s.is_empty() && s.len() <= 9 && s.bytes().all(|b| b.is_ascii_digit()))
            .ok_or(MediaError::UnsupportedContainer)
    };
    let hours = parse(hours)?;
    let minutes = parse(minutes)?;
    let (seconds, millis) = seconds
        .split_once('.')
        .ok_or(MediaError::UnsupportedContainer)?;
    let seconds = parse(seconds)?;
    let ms = parse(millis)?;
    if minutes >= 60 || seconds >= 60 || millis.len() != 3 {
        return Err(MediaError::UnsupportedContainer);
    }
    Ok(((hours * 60 + minutes) * 60 + seconds) * 1000 + ms)
}
fn vtt_format(ms: i64) -> String {
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}
fn rebase_text(bytes: &[u8], offset: f64) -> Result<Vec<u8>, MediaError> {
    if !offset.is_finite() || offset.abs() > 10_000_000_000.0 {
        return Err(MediaError::InvalidSource);
    }
    if offset.abs() < 0.0005 {
        return Ok(bytes.to_vec());
    }
    let shift = (offset * 1000.0).round() as i64;
    let text = std::str::from_utf8(bytes)
        .map_err(|_| MediaError::UnsupportedContainer)?
        .replace("\r\n", "\n");
    let mut output = String::new();
    for (block_index, block) in text
        .split("\n\n")
        .filter(|b| !b.trim().is_empty())
        .enumerate()
    {
        if block_index == 0 {
            output.push_str(
                &block
                    .lines()
                    .filter(|line| !line.starts_with("X-TIMESTAMP-MAP"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
            output.push_str("\n\n");
            continue;
        }
        if ["NOTE", "STYLE", "REGION"]
            .iter()
            .any(|prefix| block.starts_with(prefix))
        {
            output.push_str(block);
            output.push_str("\n\n");
            continue;
        }
        let lines: Vec<_> = block.lines().collect();
        let index = lines
            .iter()
            .position(|l| l.contains(" --> "))
            .filter(|i| *i <= 1)
            .ok_or(MediaError::UnsupportedContainer)?;
        let (start, end) = lines[index]
            .split_once(" --> ")
            .ok_or(MediaError::UnsupportedContainer)?;
        let (end, settings) = end
            .trim()
            .split_once(char::is_whitespace)
            .map_or((end.trim(), ""), |(time, settings)| (time, settings));
        let start = vtt_time(start.trim())? - shift;
        let end = vtt_time(end)? - shift;
        if end <= 0 {
            continue;
        }
        if end <= start {
            return Err(MediaError::UnsupportedContainer);
        }
        if index == 1 {
            output.push_str(lines[0]);
            output.push('\n');
        }
        output.push_str(&format!(
            "{} --> {}",
            vtt_format(start.max(0)),
            vtt_format(end)
        ));
        if !settings.is_empty() {
            output.push(' ');
            output.push_str(settings);
        }
        output.push('\n');
        output.push_str(&lines[index + 1..].join("\n"));
        output.push_str("\n\n");
    }
    Ok(output.into_bytes())
}
// Text-document conversion only; audio/video packets are never decoded or encoded.
fn text_document(bytes: &[u8]) -> Result<Vec<u8>, MediaError> {
    if let Ok(vtt) = webvtt(bytes) {
        return Ok(vtt);
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| MediaError::UnsupportedContainer)?
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n");
    if text.contains(['\0', '\r']) {
        return Err(MediaError::InvalidSource);
    }
    let mut output = String::from("WEBVTT\n\n");
    let mut cues = 0;
    for block in text.split("\n\n").filter(|b| !b.trim().is_empty()) {
        cues += 1;
        if cues > 10000 {
            return Err(MediaError::ResourceLimit);
        }
        let mut lines = block.lines();
        let first = lines.next().ok_or(MediaError::UnsupportedContainer)?;
        let timing = if !first.is_empty() && first.bytes().all(|b| b.is_ascii_digit()) {
            lines.next().ok_or(MediaError::UnsupportedContainer)?
        } else {
            first
        };
        let (start, end) = timing
            .split_once(" --> ")
            .ok_or(MediaError::UnsupportedContainer)?;
        let (start_ms, start) = srt_timestamp(start)?;
        let (end_ms, end) = srt_timestamp(end)?;
        if end_ms <= start_ms {
            return Err(MediaError::UnsupportedContainer);
        }
        let body: Vec<_> = lines.collect();
        if body.is_empty() {
            return Err(MediaError::UnsupportedContainer);
        }
        output.push_str(&format!("{start} --> {end}\n{}\n\n", body.join("\n")));
    }
    if cues == 0 {
        return Err(MediaError::UnsupportedContainer);
    }
    Ok(output.into_bytes())
}
fn srt_timestamp(value: &str) -> Result<(u64, String), MediaError> {
    let value = value.trim();
    let mut parts = value.split(':');
    let (hours, minutes, seconds) = (parts.next(), parts.next(), parts.next());
    if parts.next().is_some() {
        return Err(MediaError::UnsupportedContainer);
    }
    let hours = hours
        .filter(|s| s.len() >= 2 && s.len() <= 3 && s.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or(MediaError::UnsupportedContainer)?;
    let minutes = minutes
        .filter(|s| s.len() == 2 && s.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|n| *n < 60)
        .ok_or(MediaError::UnsupportedContainer)?;
    let (seconds, milliseconds) = seconds
        .and_then(|s| s.split_once(','))
        .ok_or(MediaError::UnsupportedContainer)?;
    let seconds = seconds
        .parse::<u64>()
        .ok()
        .filter(|n| *n < 60 && seconds.len() == 2 && seconds.bytes().all(|b| b.is_ascii_digit()))
        .ok_or(MediaError::UnsupportedContainer)?;
    let milliseconds = milliseconds
        .parse::<u64>()
        .ok()
        .filter(|_| milliseconds.len() == 3 && milliseconds.bytes().all(|b| b.is_ascii_digit()))
        .ok_or(MediaError::UnsupportedContainer)?;
    let time = ((hours * 60 + minutes) * 60 + seconds) * 1000 + milliseconds;
    Ok((
        time,
        format!("{hours:02}:{minutes:02}:{seconds:02}.{milliseconds:03}"),
    ))
}
fn webvtt(bytes: &[u8]) -> Result<Vec<u8>, MediaError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| MediaError::UnsupportedCodec)?
        .trim_start_matches('\u{feff}');
    if text.contains('\0') {
        return Err(MediaError::InvalidSource);
    }
    let header = text
        .strip_prefix("WEBVTT")
        .ok_or(MediaError::UnsupportedContainer)?;
    if !header.starts_with(['\r', '\n', ' ', '\t']) {
        return Err(MediaError::UnsupportedContainer);
    }
    Ok(text.as_bytes().to_vec())
}
const CORS: &str = "Access-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, HEAD, OPTIONS\r\nAccess-Control-Allow-Headers: Range, If-Range, If-None-Match, Content-Type\r\nAccess-Control-Expose-Headers: Content-Length, Content-Range, Accept-Ranges, ETag\r\n";
fn simple(stream: &mut TcpStream, code: u16, reason: &str, extra: Option<&str>) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {code} {reason}\r\n{CORS}{}Content-Length: 0\r\nConnection: close\r\n\r\n",
        extra.unwrap_or("")
    )
}
#[derive(Debug, PartialEq, Eq)]
enum Range {
    Full,
    Bytes(u64, u64),
    Invalid,
}
fn range(value: Option<&str>, len: u64) -> Range {
    let Some(value) = value else {
        return Range::Full;
    };
    let Some(value) = value.strip_prefix("bytes=") else {
        return Range::Invalid;
    };
    if value.contains(',') || len == 0 {
        return Range::Invalid;
    }
    let Some((start, end)) = value.split_once('-') else {
        return Range::Invalid;
    };
    if start.is_empty() {
        let Ok(count) = end.parse::<u64>() else {
            return Range::Invalid;
        };
        if count == 0 {
            return Range::Invalid;
        }
        return Range::Bytes(len.saturating_sub(count), len - 1);
    }
    let Ok(start) = start.parse::<u64>() else {
        return Range::Invalid;
    };
    if start >= len {
        return Range::Invalid;
    }
    let end = if end.is_empty() {
        len - 1
    } else {
        match end.parse::<u64>() {
            Ok(end) => end.min(len - 1),
            Err(_) => return Range::Invalid,
        }
    };
    if end < start {
        Range::Invalid
    } else {
        Range::Bytes(start, end)
    }
}
fn serve(stream: &mut TcpStream, session: &Session) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut bytes = Vec::new();
    let mut byte = [0u8; 1];
    let header_deadline = Instant::now() + Duration::from_secs(5);
    while bytes.len() < 8192 {
        if Instant::now() > header_deadline {
            return simple(stream, 408, "Request Timeout", None);
        }
        if stream.read(&mut byte)? == 0 {
            return Ok(());
        }
        bytes.push(byte[0]);
        if bytes.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    if !bytes.ends_with(b"\r\n\r\n") {
        return simple(stream, 431, "Request Header Fields Too Large", None);
    }
    let Ok(request) = std::str::from_utf8(&bytes) else {
        return simple(stream, 400, "Bad Request", None);
    };
    let mut lines = request.split("\r\n");
    let mut first = lines.next().unwrap_or("").split_whitespace();
    let method = first.next().unwrap_or("");
    let target = first.next().unwrap_or("");
    let headers: HashMap<String, &str> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.to_ascii_lowercase(), v.trim()))
        .collect();
    let path = format!("/{}/", session.prefix);
    let Some(id) = target
        .strip_prefix(&path)
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit()))
    else {
        return simple(stream, 404, "Not Found", None);
    };
    let mut etag = format!("\"{id}\"");
    let mut completed_segment = None;
    let resource = match session.resources.get(id) {
        Some(Resource::RamPlaylist { store }) => {
            let (data, revision) = match store.playlist() {
                Ok(snapshot) => snapshot,
                Err(_) => return simple(stream, 503, "Service Unavailable", None),
            };
            etag = format!("\"{id}-{revision:x}\"");
            Resource::Bytes {
                data,
                mime: "application/vnd.apple.mpegurl".into(),
            }
        }
        Some(resource) => resource.clone(),
        None => {
            let found = session.resources.values().find_map(|resource| {
                if let Resource::RamPlaylist { store } = resource {
                    store
                        .segment(id)
                        .map(|(data, sequence)| (data, sequence, store.clone()))
                } else {
                    None
                }
            });
            let Some((data, sequence, store)) = found else {
                return simple(stream, 404, "Not Found", None);
            };
            completed_segment = Some((store, sequence));
            Resource::Shared {
                data,
                mime: "video/mp4".into(),
            }
        }
    };
    if session.cancel.load(Ordering::Acquire) {
        return simple(stream, 410, "Gone", None);
    }
    if method == "OPTIONS" {
        return simple(stream, 204, "No Content", None);
    }
    if method != "GET" && method != "HEAD" {
        return simple(
            stream,
            405,
            "Method Not Allowed",
            Some("Allow: GET, HEAD, OPTIONS\r\n"),
        );
    }
    if headers.get("if-none-match").is_some_and(|v| *v == etag) {
        return simple(
            stream,
            304,
            "Not Modified",
            Some(&format!("ETag: {etag}\r\n")),
        );
    }
    let len = resource.len();
    let requested = range(
        if headers.get("if-range").is_some_and(|v| *v != etag) {
            None
        } else {
            headers.get("range").copied()
        },
        len,
    );
    let (start, end, partial) = match requested {
        Range::Full => (0, len.saturating_sub(1), false),
        Range::Bytes(a, b) => (a, b, true),
        Range::Invalid => {
            return simple(
                stream,
                416,
                "Range Not Satisfiable",
                Some(&format!("Content-Range: bytes */{len}\r\n")),
            );
        }
    };
    let count = if len == 0 { 0 } else { end - start + 1 };
    // Open/validate upstream before committing a successful receiver response.
    let mut input: Box<dyn Read> = match &resource {
        Resource::File { path, .. } => {
            let mut file = File::open(path)?;
            if file.metadata()?.len() != len {
                return simple(stream, 502, "Bad Gateway", None);
            }
            file.seek(SeekFrom::Start(start))?;
            Box::new(file)
        }
        Resource::Remote { source, .. } => {
            let mut reader = RemoteReader::new(source.clone());
            reader.seek(SeekFrom::Start(start))?;
            if count > 0 {
                let mut first = [0u8; 1];
                if reader.read_exact(&mut first).is_err() {
                    return simple(stream, 502, "Bad Gateway", None);
                }
                reader.seek(SeekFrom::Start(start))?;
            }
            Box::new(reader)
        }
        Resource::Bytes { data, .. } => {
            let mut reader = io::Cursor::new(data.clone());
            reader.set_position(start);
            Box::new(reader)
        }
        Resource::Shared { data, .. } => Box::new(SharedReader {
            data: data.clone(),
            position: start as usize,
        }),
        Resource::RamPlaylist { .. } => return simple(stream, 503, "Service Unavailable", None),
    };
    let (code, reason) = if partial {
        (206, "Partial Content")
    } else {
        (200, "OK")
    };
    write!(
        stream,
        "HTTP/1.1 {code} {reason}\r\n{CORS}Content-Type: {}\r\nContent-Length: {count}\r\nAccept-Ranges: bytes\r\nETag: {etag}\r\nCache-Control: private, no-cache\r\nConnection: close\r\n",
        resource.mime()
    )?;
    if partial {
        write!(stream, "Content-Range: bytes {start}-{end}/{len}\r\n")?;
    }
    write!(stream, "\r\n")?;
    if method == "HEAD" {
        return Ok(());
    }
    let mut remaining = count;
    let mut buf = [0u8; 32768];
    while remaining > 0 && !session.cancel.load(Ordering::Acquire) {
        let n = input.read(&mut buf[..remaining.min(32768) as usize])?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        stream.write_all(&buf[..n])?;
        remaining -= n as u64;
    }
    if remaining == 0
        && start == 0
        && count == len
        && let Some((store, sequence)) = completed_segment
    {
        store.fetched(sequence);
    }
    Ok(())
}
struct SharedReader {
    data: Arc<Vec<u8>>,
    position: usize,
}
impl Read for SharedReader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = bytes
            .len()
            .min(self.data.len().saturating_sub(self.position));
        bytes[..count].copy_from_slice(&self.data[self.position..self.position + count]);
        self.position += count;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn session() -> Session {
        let mut resources = HashMap::new();
        resources.insert(
            "aabb".into(),
            Resource::Bytes {
                data: b"0123456789".to_vec(),
                mime: "video/mp4".into(),
            },
        );
        Session {
            cancel: Arc::new(AtomicBool::new(false)),
            resources,
            prefix: "secret".into(),
            active: AtomicUsize::new(0),
            max_readers: 2,
            _temporary: None,
        }
    }
    fn request(session: Session, request: &str) -> Vec<u8> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            serve(&mut stream, &session).unwrap();
        });
        let mut client = TcpStream::connect(address).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        client.write_all(request.as_bytes()).unwrap();
        let mut result = vec![];
        client.read_to_end(&mut result).unwrap();
        worker.join().unwrap();
        result
    }
    fn body(response: &[u8]) -> &[u8] {
        let i = response.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        &response[i + 4..]
    }
    #[test]
    fn ranges_cover_suffix_open_clamped_and_invalid() {
        assert_eq!(range(Some("bytes=-3"), 10), Range::Bytes(7, 9));
        assert_eq!(range(Some("bytes=3-"), 10), Range::Bytes(3, 9));
        assert_eq!(range(Some("bytes=3-999"), 10), Range::Bytes(3, 9));
        for value in [
            "bytes=10-",
            "bytes=3-2",
            "bytes=-0",
            "bytes=0-1,4-5",
            "bad=1-2",
            "bytes=x-2",
        ] {
            assert_eq!(range(Some(value), 10), Range::Invalid, "{value}")
        }
        assert_eq!(range(Some("bytes=0-"), 0), Range::Invalid);
    }
    #[test]
    fn actual_http_head_partial_cors_and_validator() {
        let response = request(
            session(),
            "GET /secret/aabb HTTP/1.1\r\nHost: receiver\r\nRange: bytes=2-5\r\n\r\n",
        );
        let text = String::from_utf8_lossy(&response);
        assert!(text.starts_with("HTTP/1.1 206"));
        assert!(text.contains("Content-Range: bytes 2-5/10"));
        assert!(text.contains("Access-Control-Expose-Headers:"));
        assert_eq!(body(&response), b"2345");
        let response = request(
            session(),
            "HEAD /secret/aabb HTTP/1.1\r\nHost: receiver\r\nRange: bytes=-2\r\n\r\n",
        );
        let text = String::from_utf8_lossy(&response);
        assert!(text.contains("Content-Length: 2"));
        assert!(body(&response).is_empty());
        let response = request(
            session(),
            "GET /secret/aabb HTTP/1.1\r\nIf-None-Match: \"aabb\"\r\n\r\n",
        );
        assert!(response.starts_with(b"HTTP/1.1 304"));
        let response = request(
            session(),
            "OPTIONS /secret/aabb HTTP/1.1\r\nOrigin: https://receiver\r\n\r\n",
        );
        assert!(response.starts_with(b"HTTP/1.1 204"));
    }
    #[test]
    fn tokens_paths_unsatisfiable_and_cancellation_are_enforced() {
        for path in [
            "/wrong/aabb",
            "/secret/../aabb",
            "/secret/%61abb",
            "/secret/aabb?url=http://private",
            "/secret/aabb/other",
        ] {
            let response = request(session(), &format!("GET {path} HTTP/1.1\r\n\r\n"));
            assert!(response.starts_with(b"HTTP/1.1 404"), "{path}");
        }
        let response = request(
            session(),
            "GET /secret/aabb HTTP/1.1\r\nRange: bytes=10-\r\n\r\n",
        );
        assert!(response.starts_with(b"HTTP/1.1 416"));
        assert!(String::from_utf8_lossy(&response).contains("Content-Range: bytes */10"));
        let s = session();
        s.cancel();
        assert!(request(s, "GET /secret/aabb HTTP/1.1\r\n\r\n").starts_with(b"HTTP/1.1 410"));
    }
    #[test]
    fn redirect_drops_credentials_when_origin_changes() {
        let destination = TcpListener::bind("127.0.0.1:0").unwrap();
        let destination_address = destination.local_addr().unwrap();
        let destination_worker = thread::spawn(move || {
            let (mut stream, _) = destination.accept().unwrap();
            let mut bytes = vec![];
            let mut b = [0u8; 1];
            while !bytes.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut b).unwrap();
                bytes.push(b[0]);
            }
            let request = String::from_utf8(bytes).unwrap().to_ascii_lowercase();
            assert!(!request.contains("authorization:"));
            assert!(!request.contains("x-private:"));
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
            )
            .unwrap();
        });
        let origin = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin_address = origin.local_addr().unwrap();
        let origin_worker = thread::spawn(move || {
            let (mut stream, _) = origin.accept().unwrap();
            let mut bytes = vec![];
            let mut b = [0u8; 1];
            while !bytes.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut b).unwrap();
                bytes.push(b[0]);
            }
            assert!(
                String::from_utf8(bytes)
                    .unwrap()
                    .to_ascii_lowercase()
                    .contains("authorization: bearer private")
            );
            write!(stream,"HTTP/1.1 302 Found\r\nLocation: http://{destination_address}/file\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        let source = HttpSource::new(
            format!("http://{origin_address}/private"),
            vec![
                ("Authorization".into(), "Bearer private".into()),
                ("X-Private".into(), "secret".into()),
            ],
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        assert_eq!(source.bounded_bytes(100).unwrap(), b"ok");
        origin_worker.join().unwrap();
        destination_worker.join().unwrap();
    }
    #[test]
    fn ranged_upstream_failure_is_reported_before_success_headers() {
        let remote = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = remote.local_addr().unwrap();
        let worker = thread::spawn(move || {
            let (mut stream, _) = remote.accept().unwrap();
            let mut bytes = [0u8; 1024];
            let _ = stream.read(&mut bytes).unwrap();
            write!(
                stream,
                "HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
        });
        let mut source = HttpSource::new(
            format!("http://{address}/file"),
            vec![],
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        source.len = 10;
        let mut s = session();
        s.resources.insert(
            "aabb".into(),
            Resource::Remote {
                source: Arc::new(source),
                mime: "video/mp4".into(),
            },
        );
        assert!(request(s, "GET /secret/aabb HTTP/1.1\r\n\r\n").starts_with(b"HTTP/1.1 502"));
        worker.join().unwrap();
    }
    #[test]
    fn external_text_tracks_follow_ram_timeline_offset_and_keep_settings() {
        let source=b"WEBVTT\n\nold\n00:00:01.000 --> 00:00:02.000\nOld\n\ncurrent\n00:00:03.000 --> 00:00:05.000 line:20%\nHello\n";
        let text = String::from_utf8(rebase_text(source, 3.5).unwrap()).unwrap();
        assert!(!text.contains("Old"));
        assert!(text.contains("00:00:00.000 --> 00:00:01.500 line:20%"));
        let text = String::from_utf8(
            rebase_text(b"WEBVTT\n\n00:00.000 --> 00:01.000\nHello\n", -0.167).unwrap(),
        )
        .unwrap();
        assert!(text.contains("00:00:00.167 --> 00:00:01.167"));
    }
    #[test]
    fn bounded_srt_conversion_keeps_cues_and_rejects_invalid_timing() {
        let result=text_document(b"1\r\n00:00:00,000 --> 00:00:01,500\r\nHello\r\n\r\n2\r\n00:00:02,000 --> 00:00:03,000\r\n<i>World</i>\r\n").unwrap();
        let text = String::from_utf8(result).unwrap();
        assert!(text.starts_with("WEBVTT\n\n00:00:00.000 --> 00:00:01.500\nHello"));
        assert!(text.contains("<i>World</i>"));
        for input in [
            "1\n00:00:02,000 --> 00:00:01,000\nBackwards",
            "1\n00:61:00,000 --> 00:62:00,000\nInvalid",
            "[Script Info]\nASS file",
            "",
        ] {
            assert!(text_document(input.as_bytes()).is_err());
        }
    }
    #[test]
    fn optional_webvtt_is_validated_and_failure_does_not_create_routes() {
        assert_eq!(
            webvtt(b"WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nHello\n").unwrap(),
            b"WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nHello\n"
        );
        assert!(webvtt(b"WEBVTTevil").is_err());
        assert!(webvtt(b"1\n00:00:00,000 --> 00:00:01,000\nSRT").is_err());
        assert!(webvtt(&[255]).is_err());
        let mut resources = HashMap::new();
        let tracks = vec![SourceTextTrack {
            id: 1,
            name: "English".into(),
            language: "en".into(),
            url: "file:///private".into(),
            headers: vec![],
        }];
        let (prepared, warnings) = prepare_text_tracks(
            &tracks,
            &mut resources,
            "http://192.168.1.2/secret",
            0.0,
            Arc::new(AtomicBool::new(false)),
        );
        assert!(prepared.is_empty());
        assert_eq!(warnings, vec![(1, MediaError::InvalidSource)]);
        assert!(resources.is_empty());
    }
    #[test]
    fn replacing_guard_does_not_cancel_parent_or_next_attempt() {
        let parent = Arc::new(AtomicBool::new(false));
        let first_cancel = crate::owned_cancellation(parent.clone()).unwrap();
        let next_cancel = crate::owned_cancellation(parent.clone()).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let s = session();
        let prepared = start(
            listener,
            format!("http://{address}/secret"),
            "aabb".into(),
            "video/mp4".into(),
            None,
            true,
            DeliveryMode::DirectRelay,
            s.resources,
            s.prefix,
            first_cancel.clone(),
            2,
            None,
        )
        .unwrap();
        drop(prepared);
        assert!(first_cancel.load(Ordering::Acquire));
        assert!(!parent.load(Ordering::Acquire));
        assert!(!next_cancel.load(Ordering::Acquire));
        parent.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(1);
        while !next_cancel.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(next_cancel.load(Ordering::Acquire));
    }
    #[test]
    fn guard_drop_revokes_listener_and_removes_owned_artifacts() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_owned();
        let s = session();
        let prepared = start(
            listener,
            format!("http://{address}/secret"),
            "aabb".into(),
            "video/mp4".into(),
            None,
            true,
            DeliveryMode::DirectRelay,
            s.resources,
            s.prefix,
            s.cancel,
            2,
            Some(dir),
        )
        .unwrap();
        drop(prepared);
        let deadline = Instant::now() + Duration::from_secs(3);
        while path.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(!path.exists());
        assert!(TcpStream::connect(address).is_err());
    }
}
