//! Finite HLS manifests are rewritten as a bounded graph of session resources.
//! Encrypted presentations, byte ranges and live/delta playlists fail explicitly.
use crate::{
    http::{HttpSource, Resource},
    *,
};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Read,
    net::TcpListener,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
};
use url::Url;
const MAX_NODES: usize = 1024;
const MAX_PLAYLIST: usize = 512 * 1024;
const MAX_SAMPLE: usize = 16 * 1024 * 1024;
#[derive(Clone)]
enum Location {
    File(PathBuf),
    Remote(Url),
}
struct Graph {
    root: Option<PathBuf>,
    origin: Option<url::Origin>,
    headers: Vec<(String, String)>,
    cancel: Arc<AtomicBool>,
    resources: HashMap<String, Resource>,
    seen: HashMap<String, String>,
    visiting: HashSet<String>,
    base: String,
    started: std::time::Instant,
    total: usize,
    profile: OutputProfile,
    duration: Option<f64>,
    options: PrepareOptions,
}
impl Graph {
    fn key(location: &Location) -> String {
        match location {
            Location::File(p) => p.to_string_lossy().into_owned(),
            Location::Remote(u) => u.as_str().to_owned(),
        }
    }
    fn resolve(&self, base: &Location, reference: &str) -> Result<Location, MediaError> {
        if reference.is_empty() || reference.contains(['\r', '\n', '\\']) {
            return Err(MediaError::UnsupportedHls);
        }
        match base {
            Location::File(path) => {
                if reference.contains(['?', '#', ':']) || Path::new(reference).is_absolute() {
                    return Err(MediaError::UnsupportedHls);
                }
                let path = path
                    .parent()
                    .ok_or(MediaError::InvalidSource)?
                    .join(reference)
                    .canonicalize()
                    .map_err(|_| MediaError::SourceUnavailable)?;
                if !path.starts_with(self.root.as_ref().ok_or(MediaError::InvalidSource)?) {
                    return Err(MediaError::InvalidSource);
                }
                Ok(Location::File(path))
            }
            Location::Remote(url) => {
                let next = url.join(reference).map_err(|_| MediaError::InvalidSource)?;
                if !matches!(next.scheme(), "http" | "https")
                    || !next.username().is_empty()
                    || next.password().is_some()
                    || (url.scheme() == "https" && next.scheme() != "https")
                {
                    return Err(MediaError::InvalidSource);
                }
                Ok(Location::Remote(next))
            }
        }
    }
    fn source(&self, url: &Url) -> Result<HttpSource, MediaError> {
        HttpSource::new(
            url.to_string(),
            if self
                .origin
                .as_ref()
                .is_some_and(|origin| *origin == url.origin())
            {
                self.headers.clone()
            } else {
                vec![]
            },
            self.cancel.clone(),
        )
    }
    fn bytes(&self, location: &Location, max: usize) -> Result<Vec<u8>, MediaError> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(MediaError::Cancelled);
        }
        if self.started.elapsed() > std::time::Duration::from_secs(120) {
            return Err(MediaError::ResourceLimit);
        }
        match location {
            Location::File(path) => {
                let metadata = fs::metadata(path).map_err(|_| MediaError::SourceUnavailable)?;
                if !metadata.is_file() || metadata.len() > max as u64 {
                    return Err(MediaError::ResourceLimit);
                }
                fs::read(path).map_err(|_| MediaError::SourceUnavailable)
            }
            Location::Remote(url) => self.source(url)?.bounded_bytes(max),
        }
    }
    fn asset(&mut self, location: Location, mime: &str) -> Result<String, MediaError> {
        if self.started.elapsed() > std::time::Duration::from_secs(120) {
            return Err(MediaError::ResourceLimit);
        }
        let key = Self::key(&location);
        if let Some(id) = self.seen.get(&key) {
            return Ok(id.clone());
        }
        if self.seen.len() >= MAX_NODES {
            return Err(MediaError::ResourceLimit);
        }
        let id = http::token();
        let resource = match &location {
            Location::File(path) => {
                let metadata = fs::metadata(path).map_err(|_| MediaError::SourceUnavailable)?;
                if !metadata.is_file() {
                    return Err(MediaError::InvalidSource);
                }
                Resource::File {
                    path: path.clone(),
                    mime: mime.into(),
                    len: metadata.len(),
                }
            }
            Location::Remote(url) => {
                let source = self.source(url)?.inspect()?;
                if source.is_hls {
                    return Err(MediaError::UnsupportedHls);
                }
                Resource::Remote {
                    source: Arc::new(source),
                    mime: mime.into(),
                }
            }
        };
        self.seen.insert(key, id.clone());
        self.resources.insert(id.clone(), resource);
        Ok(id)
    }
    fn playlist(
        &mut self,
        location: Location,
        subtitle: bool,
        depth: usize,
    ) -> Result<String, MediaError> {
        let key = Self::key(&location);
        if self.visiting.contains(&key) || depth > 6 {
            return Err(MediaError::UnsupportedHls);
        }
        if let Some(id) = self.seen.get(&key) {
            return Ok(id.clone());
        }
        if self.seen.len() >= MAX_NODES {
            return Err(MediaError::ResourceLimit);
        }
        self.visiting.insert(key.clone());
        let (bytes, location) = match &location {
            Location::File(_) => (self.bytes(&location, MAX_PLAYLIST)?, location),
            Location::Remote(url) => {
                let response = self.source(url)?.request(None)?;
                let final_url = response.url().clone();
                let mut bytes = Vec::new();
                response
                    .take(MAX_PLAYLIST as u64 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|_| MediaError::Network)?;
                if bytes.len() > MAX_PLAYLIST {
                    return Err(MediaError::ResourceLimit);
                }
                (bytes, Location::Remote(final_url))
            }
        };
        self.total += bytes.len();
        if self.total > 4 * MAX_PLAYLIST {
            return Err(MediaError::ResourceLimit);
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| MediaError::UnsupportedHls)?;
        if !text.trim_start().starts_with("#EXTM3U") {
            return Err(MediaError::UnsupportedHls);
        }
        let master = text.lines().any(|l| l.starts_with("#EXT-X-STREAM-INF:"));
        if !master && !text.lines().any(|l| l.trim() == "#EXT-X-ENDLIST") {
            return Err(MediaError::UnsupportedHls);
        }
        let mut output = String::new();
        let mut next_playlist = false;
        let mut sample = None;
        let mut initialization = None;
        let mut duration = 0.0;
        for raw in text.lines() {
            let line = raw.trim();
            // Ordinary comments and non-playback metadata may contain private source URLs.
            if (line.starts_with('#') && !line.starts_with("#EXT"))
                || line.starts_with("#EXT-X-SESSION-DATA:")
            {
                continue;
            }
            if line.starts_with("#EXT-X-BYTERANGE")
                || line.contains("BYTERANGE=")
                || line.starts_with("#EXT-X-SESSION-KEY")
                || line.starts_with("#EXT-X-PART")
                || line.starts_with("#EXT-X-PRELOAD-HINT")
                || line.starts_with("#EXT-X-SKIP")
                || line.starts_with("#EXT-X-DEFINE")
                || line.starts_with("#EXT-X-CONTENT-STEERING")
            {
                return Err(MediaError::UnsupportedHls);
            }
            if line.starts_with("#EXT-X-KEY:")
                && !line.split(':').nth(1).is_some_and(|s| s == "METHOD=NONE")
            {
                return Err(MediaError::UnsupportedHls);
            }
            if let Some(seconds) = line
                .strip_prefix("#EXTINF:")
                .and_then(|s| s.split(',').next())
                .and_then(|s| s.parse::<f64>().ok())
            {
                if !seconds.is_finite() || seconds < 0.0 {
                    return Err(MediaError::UnsupportedHls);
                }
                duration += seconds;
            }
            if !line.starts_with('#') && !line.is_empty() {
                let child = self.resolve(&location, line)?;
                let id = if next_playlist {
                    self.playlist(child, subtitle, depth + 1)?
                } else {
                    if sample.is_none() {
                        sample = Some(child.clone())
                    }
                    self.asset(
                        child,
                        if subtitle {
                            "text/vtt"
                        } else if initialization.is_some() {
                            "video/mp4"
                        } else {
                            "video/mp2t"
                        },
                    )?
                };
                output.push_str(&format!("{}/{id}\n", self.base));
                next_playlist = false;
                continue;
            }
            if line.starts_with("#EXT-X-STREAM-INF:") {
                next_playlist = true;
            }
            let mut rewritten = line.to_owned();
            if let Some(index) = line.find("URI=\"") {
                let start = index + 5;
                let end = line[start..]
                    .find('"')
                    .map(|n| n + start)
                    .ok_or(MediaError::UnsupportedHls)?;
                if line[end + 1..].contains("URI=") {
                    return Err(MediaError::UnsupportedHls);
                }
                let child = self.resolve(&location, &line[start..end])?;
                let id = if line.starts_with("#EXT-X-MEDIA:")
                    || line.starts_with("#EXT-X-I-FRAME-STREAM-INF:")
                {
                    self.playlist(child, line.contains("TYPE=SUBTITLES"), depth + 1)?
                } else if line.starts_with("#EXT-X-MAP:") {
                    initialization = Some(child.clone());
                    self.asset(child, "video/mp4")?
                } else {
                    return Err(MediaError::UnsupportedHls);
                };
                rewritten.replace_range(start..end, &format!("{}/{id}", self.base));
            } else if line.contains("URI=") {
                return Err(MediaError::UnsupportedHls);
            }
            output.push_str(&rewritten);
            output.push('\n');
        }
        if !master && !subtitle {
            let sample = sample.ok_or(MediaError::UnsupportedHls)?;
            let mut bytes = if let Some(init) = initialization {
                self.bytes(&init, MAX_SAMPLE)?
            } else {
                vec![]
            };
            let segment = self.bytes(&sample, MAX_SAMPLE)?;
            if bytes.len() + segment.len() > MAX_SAMPLE {
                return Err(MediaError::ResourceLimit);
            }
            bytes.extend_from_slice(&segment);
            let probe = nova_remux::probe_reader(&mut std::io::Cursor::new(bytes), &self.cancel)?;
            if !matches!(probe.container, Container::Mp4 | Container::MpegTs) {
                return Err(MediaError::UnsupportedHls);
            }
            http::validate_codecs(&probe, &self.profile, self.options.allow_unverified)?;
            if self.duration.is_none() {
                self.duration = Some(duration)
            }
        }
        let id = http::token();
        self.resources.insert(
            id.clone(),
            Resource::Bytes {
                data: output.into_bytes(),
                mime: "application/vnd.apple.mpegurl".into(),
            },
        );
        self.seen.insert(key.clone(), id.clone());
        self.visiting.remove(&key);
        Ok(id)
    }
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_bundle(
    root: PathBuf,
    playlist: PathBuf,
    profile: OutputProfile,
    options: PrepareOptions,
    cancel: Arc<AtomicBool>,
    listener: TcpListener,
    base: String,
    prefix: String,
) -> Result<PreparedMedia, MediaError> {
    let root = root
        .canonicalize()
        .map_err(|_| MediaError::SourceUnavailable)?;
    let playlist = if playlist.is_absolute() {
        playlist
    } else {
        root.join(playlist)
    }
    .canonicalize()
    .map_err(|_| MediaError::SourceUnavailable)?;
    if !playlist.starts_with(&root) {
        return Err(MediaError::InvalidSource);
    }
    let graph = Graph {
        root: Some(root),
        origin: None,
        headers: vec![],
        cancel,
        resources: HashMap::new(),
        seen: HashMap::new(),
        visiting: HashSet::new(),
        base,
        started: std::time::Instant::now(),
        total: 0,
        profile,
        duration: None,
        options,
    };
    finish(graph, Location::File(playlist), listener, prefix)
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_remote(
    url: String,
    headers: Vec<(String, String)>,
    profile: OutputProfile,
    options: PrepareOptions,
    cancel: Arc<AtomicBool>,
    listener: TcpListener,
    base: String,
    prefix: String,
) -> Result<PreparedMedia, MediaError> {
    let url = Url::parse(&url).map_err(|_| MediaError::InvalidSource)?;
    let graph = Graph {
        root: None,
        origin: Some(url.origin()),
        headers,
        cancel,
        resources: HashMap::new(),
        seen: HashMap::new(),
        visiting: HashSet::new(),
        base,
        started: std::time::Instant::now(),
        total: 0,
        profile,
        duration: None,
        options,
    };
    finish(graph, Location::Remote(url), listener, prefix)
}
fn finish(
    mut graph: Graph,
    location: Location,
    listener: TcpListener,
    prefix: String,
) -> Result<PreparedMedia, MediaError> {
    if graph.options.selected_audio_stream.is_some() {
        return Err(MediaError::UnsupportedHls);
    }
    if !graph.profile.hls {
        return Err(MediaError::UnsupportedHls);
    }
    let main = graph.playlist(location, false, 0)?;
    let (text_tracks, text_track_warnings) = http::prepare_text_tracks(
        &graph.options.text_tracks,
        &mut graph.resources,
        &graph.base,
        0.0,
        graph.cancel.clone(),
    );
    let mut prepared = http::start(
        listener,
        graph.base,
        main,
        "application/vnd.apple.mpegurl".into(),
        graph.duration,
        true,
        DeliveryMode::HlsRelay,
        graph.resources,
        prefix,
        graph.cancel,
        graph.options.max_readers,
        None,
    )?;
    prepared.text_tracks = text_tracks;
    prepared.text_track_warnings = text_track_warnings;
    Ok(prepared)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn graph(root: &Path) -> Graph {
        let profile = OutputProfile {
            hls: true,
            ..Default::default()
        };
        Graph {
            root: Some(root.to_owned()),
            origin: None,
            headers: vec![],
            cancel: Arc::new(AtomicBool::new(false)),
            resources: HashMap::new(),
            seen: HashMap::new(),
            visiting: HashSet::new(),
            base: "http://192.168.1.5:9999/private-token".into(),
            started: std::time::Instant::now(),
            total: 0,
            profile,
            duration: None,
            options: PrepareOptions {
                cache_dir: root.join("cache"),
                ..Default::default()
            },
        }
    }
    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("segment.ts"),
            include_bytes!("../tests/fixtures/h264-aac.ts"),
        )
        .unwrap();
        dir
    }
    #[test]
    fn nested_finite_playlist_is_probed_and_every_uri_becomes_opaque() {
        let dir = fixture();
        fs::write(
            dir.path().join("master.m3u8"),
            "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=100000\nmedia.m3u8\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("media.m3u8"),
            "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\nsegment.ts\n#EXT-X-ENDLIST\n",
        )
        .unwrap();
        let mut graph = graph(dir.path());
        let main = graph
            .playlist(Location::File(dir.path().join("master.m3u8")), false, 0)
            .unwrap();
        assert_eq!(graph.resources.len(), 3);
        assert!(matches!(
            graph.resources.get(&main),
            Some(Resource::Bytes { .. })
        ));
        assert_eq!(graph.duration, Some(1.0));
        for resource in graph.resources.values() {
            if let Resource::Bytes { data, .. } = resource {
                let text = std::str::from_utf8(data).unwrap();
                assert!(!text.contains("segment.ts"));
                assert!(!text.contains("media.m3u8"));
                for line in text
                    .lines()
                    .filter(|line| !line.starts_with('#') && !line.is_empty())
                {
                    assert!(line.starts_with("http://192.168.1.5:9999/private-token/"));
                }
            }
        }
    }
    #[test]
    fn rejects_live_encryption_ranges_loops_and_outside_bundle() {
        let dir = fixture();
        let mut graph = graph(dir.path());
        let path = dir.path().join("media.m3u8");
        for text in [
            "#EXTM3U\n#EXTINF:1,\nsegment.ts\n",
            "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"key\"\n#EXTINF:1,\nsegment.ts\n#EXT-X-ENDLIST\n",
            "#EXTM3U\n#EXT-X-BYTERANGE:2@0\n#EXTINF:1,\nsegment.ts\n#EXT-X-ENDLIST\n",
            "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nmedia.m3u8\n",
        ] {
            fs::write(&path, text).unwrap();
            graph.visiting.clear();
            assert!(
                graph
                    .playlist(Location::File(path.clone()), false, 0)
                    .is_err()
            );
        }
        assert!(
            graph
                .resolve(&Location::File(path), "../outside.ts")
                .is_err()
        );
    }
    #[test]
    fn cancellation_prevents_playlist_reads() {
        let dir = fixture();
        let mut graph = graph(dir.path());
        graph.cancel.store(true, Ordering::Release);
        assert!(matches!(
            graph.playlist(Location::File(dir.path().join("anything")), false, 0),
            Err(MediaError::Cancelled)
        ));
    }
}
