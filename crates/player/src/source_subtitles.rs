//! Source-attached caption facts stay with the accepted playback session.
//! Remote captions are fetched outside mpv so video headers cannot follow them.
use nova_config::playback_preferences::normalize_language;
use std::path::PathBuf;

/// One caption request, with video authority kept separate from its destination.
pub struct SourceSubtitleRequest {
    pub url: String,
    pub video_url: String,
    pub video_headers: Vec<(String, String)>,
}

/// A private, session-owned caption file. Dropping a stale result removes it.
pub struct SourceSubtitleFile(PathBuf);

impl SourceSubtitleFile {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        use std::io::Write;
        if bytes.is_empty() || bytes.len() > 8 * 1024 * 1024 {
            return Err("invalid subtitle file size".into());
        }
        let directory = nova_config::app_cache_dir().join("subtitles");
        std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = directory.join(format!(
            "source-{}-{nonce}-{}.sub",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut output = options.open(&path).map_err(|error| error.to_string())?;
        let file = Self(path);
        output.write_all(bytes).map_err(|error| error.to_string())?;
        Ok(file)
    }
}

impl Drop for SourceSubtitleFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

struct PreparedSubtitle {
    url: String,
    file: SourceSubtitleFile,
    loaded: bool,
}

/// Runtime-only source metadata, never serialized into preferences or sync.
#[derive(Clone, PartialEq, Eq)]
pub struct SourceSubtitle {
    pub url: String,
    pub language: Option<String>,
}

// Caption URLs can contain credentials. Keep diagnostics semantic.
impl std::fmt::Debug for SourceSubtitle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceSubtitle")
            .field("language", &self.language)
            .finish_non_exhaustive()
    }
}

impl From<String> for SourceSubtitle {
    fn from(url: String) -> Self {
        Self {
            url,
            language: None,
        }
    }
}

impl From<&str> for SourceSubtitle {
    fn from(url: &str) -> Self {
        Self::from(url.to_owned())
    }
}

pub(super) fn bounded(subtitles: &[SourceSubtitle]) -> Vec<SourceSubtitle> {
    let mut total = 0usize;
    subtitles
        .iter()
        .take(20)
        .filter_map(|subtitle| {
            let url = &subtitle.url;
            total = total.saturating_add(url.len());
            if total > 32 * 1024
                || url.len() > 4096
                || !(url.starts_with("https://") || url.starts_with("http://"))
                || url.bytes().any(|byte| byte < b' ' || byte == 127)
            {
                return None;
            }
            Some(SourceSubtitle {
                url: url.clone(),
                language: subtitle
                    .language
                    .as_deref()
                    .filter(|language| language.len() <= 64)
                    .and_then(normalize_language),
            })
        })
        .collect()
}

#[derive(Default)]
pub(super) struct SourceSubtitles {
    source: String,
    session: u64,
    subtitles: Vec<SourceSubtitle>,
    headers: Vec<(String, String)>,
    files: Vec<PreparedSubtitle>,
}

impl SourceSubtitles {
    pub(super) fn begin(&mut self, source: &str, session: u64, subtitles: Vec<SourceSubtitle>) {
        self.clear();
        self.source = source.into();
        self.session = session;
        self.subtitles = subtitles;
    }

    pub(super) fn set_headers(&mut self, headers: &[(String, String)]) {
        self.headers = headers.to_vec();
    }

    pub(super) fn requests(&self) -> Vec<SourceSubtitleRequest> {
        let mut seen = std::collections::HashSet::new();
        self.subtitles
            .iter()
            .filter(|subtitle| seen.insert(&subtitle.url))
            .map(|subtitle| SourceSubtitleRequest {
                url: subtitle.url.clone(),
                video_url: self.source.clone(),
                video_headers: self.headers.clone(),
            })
            .collect()
    }

    pub(super) fn attach(&mut self, session: u64, url: &str, file: SourceSubtitleFile) -> bool {
        if self.session != session
            || !self.subtitles.iter().any(|subtitle| subtitle.url == url)
            || self.files.iter().any(|subtitle| subtitle.url == url)
        {
            return false;
        }
        self.files.push(PreparedSubtitle {
            url: url.into(),
            file,
            loaded: false,
        });
        true
    }

    #[cfg(target_os = "android")]
    pub(super) fn local_paths(&self) -> Vec<String> {
        self.files
            .iter()
            .map(|subtitle| subtitle.file.0.to_string_lossy().into_owned())
            .collect()
    }

    #[cfg(target_os = "android")]
    pub(super) fn is_empty(&self) -> bool {
        self.subtitles.is_empty()
    }

    pub(super) fn load_pending(
        &mut self,
        mpv: &libmpv2::Mpv,
        source: &str,
        session: u64,
        manual: bool,
    ) {
        if self.source != source || self.session != session {
            return;
        }
        let raw = mpv.get_property::<String>("track-list").unwrap_or_default();
        let tracks: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
        for subtitle in self.files.iter_mut().filter(|subtitle| !subtitle.loaded) {
            let path = subtitle.file.0.to_string_lossy();
            // Android recovery may already have loaded the retained local path
            // through sub-files. Never add that file a second time.
            if tracks.as_array().is_some_and(|tracks| {
                tracks
                    .iter()
                    .any(|track| track["external-filename"].as_str() == Some(path.as_ref()))
            }) {
                subtitle.loaded = true;
                continue;
            }
            let primary = mpv.get_property::<String>("sid").ok();
            let secondary = mpv.get_property::<String>("secondary-sid").ok();
            // A malformed caption must not retry on every tick or prevent the
            // independent addon fallback from supplying a usable language.
            let _ = mpv.command("sub-add", &[&path, "auto"]);
            subtitle.loaded = true;
            if manual && let Some(primary) = primary {
                let _ = mpv.set_property("sid", primary);
            }
            if let Some(secondary) = secondary {
                let _ = mpv.set_property("secondary-sid", secondary);
            }
        }
    }

    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }

    pub(super) fn enrich(&self, source: &str, session: u64, raw: String) -> String {
        if self.subtitles.is_empty() || source != self.source || session != self.session {
            return raw;
        }
        let Ok(serde_json::Value::Array(mut tracks)) = serde_json::from_str(&raw) else {
            return raw;
        };
        for track in &mut tracks {
            if track["type"] != "sub" || track["external"].as_bool() != Some(true) {
                continue;
            }
            let Some(url) = track["external-filename"].as_str() else {
                continue;
            };
            let matching = self
                .subtitles
                .iter()
                .filter(|subtitle| {
                    subtitle.url == url
                        || self.files.iter().any(|file| {
                            file.url == subtitle.url && file.file.0.to_string_lossy() == url
                        })
                })
                .collect::<Vec<_>>();
            if matching.is_empty() {
                continue;
            }
            // Never strip URL queries or infer language from filenames/titles.
            // Conflicting declarations for one URL do not establish a language.
            let languages = matching
                .iter()
                .filter_map(|subtitle| subtitle.language.as_deref().and_then(normalize_language))
                .collect::<std::collections::BTreeSet<_>>();
            let native_language = track["lang"].as_str().and_then(normalize_language);
            if native_language.is_none() && languages.len() == 1 {
                track["lang"] = serde_json::json!(languages.into_iter().next().unwrap());
            }
            // Menu provenance is separate from title/kind, so saved semantic
            // choices never acquire a URL, source identity or translated label.
            track["nova-source-subtitle"] = serde_json::Value::Bool(true);
        }
        serde_json::to_string(&tracks).unwrap_or(raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nova_config::playback_preferences::{TrackChoice, select_track};
    use serde_json::json;

    fn source(url: &str, language: Option<&str>) -> SourceSubtitle {
        SourceSubtitle {
            url: url.into(),
            language: language.map(str::to_owned),
        }
    }

    #[test]
    fn exact_url_language_and_origin_survive_same_session_recovery_but_not_replacement() {
        let url = "https://example.invalid/eng.vtt?edition=one&token=ephemeral";
        let raw = json!([
            {"id":1,"type":"sub","external":true,"external-filename":url},
            {"id":2,"type":"sub","external":true,"external-filename":"https://example.invalid/eng.vtt?edition=two&token=ephemeral"},
            {"id":3,"type":"sub","external":false,"title":"eng.vtt"},
            {"id":4,"type":"sub","external":true,"external-filename":url,"lang":"hr"}
        ]).to_string();
        let mut subtitles = SourceSubtitles::default();
        subtitles.begin("video", 7, vec![source(url, Some("ENG"))]);
        let enriched = subtitles.enrich("video", 7, raw.clone());
        let tracks: serde_json::Value = serde_json::from_str(&enriched).unwrap();
        assert_eq!(tracks[0]["lang"], "en");
        assert_eq!(tracks[0]["nova-source-subtitle"], true);
        assert!(tracks[1]["lang"].is_null());
        assert!(tracks[1]["nova-source-subtitle"].is_null());
        assert!(tracks[2]["nova-source-subtitle"].is_null());
        assert_eq!(tracks[3]["lang"], "hr", "native known language wins");
        assert_eq!(subtitles.enrich("video", 7, raw.clone()), enriched);
        assert_eq!(subtitles.enrich("different video", 7, raw.clone()), raw);
        assert_eq!(subtitles.enrich("video", 8, raw.clone()), raw);
        subtitles.begin("video", 8, Vec::new());
        assert_eq!(subtitles.enrich("video", 8, raw.clone()), raw);
        subtitles.clear();
        assert_eq!(subtitles.enrich("video", 7, raw.clone()), raw);
    }

    #[test]
    fn provided_main_and_second_languages_select_independently_without_persisting_origin() {
        let mut subtitles = SourceSubtitles::default();
        subtitles.begin(
            "video",
            1,
            vec![
                source("https://x/en", Some("eng")),
                source("https://x/hr", Some("hrv")),
            ],
        );
        let raw = json!([
            {"id":5,"type":"sub","external":true,"external-filename":"https://x/en"},
            {"id":9,"type":"sub","external":true,"external-filename":"https://x/hr"}
        ])
        .to_string();
        let raw = subtitles.enrich("video", 1, raw);
        let tracks = super::super::track_preferences::parse_tracks(&raw);
        assert_eq!(select_track(&tracks, false, None, "en"), Some(5));
        assert_eq!(select_track(&tracks, false, None, "hr"), Some(9));
        assert_eq!(
            select_track(&tracks, false, Some(&TrackChoice::Off), "en"),
            Some(-1)
        );
        assert!(tracks.iter().all(|track| track.descriptor.title.is_none()));
        assert!(
            !serde_json::to_string(&tracks[0].descriptor)
                .unwrap()
                .contains("https")
        );
    }

    #[test]
    fn unknown_and_conflicting_languages_stay_unknown_and_url_limits_still_apply() {
        let mut subtitles = SourceSubtitles::default();
        subtitles.begin(
            "video",
            1,
            vec![
                source("https://x/eng.vtt", None),
                source("https://x/conflict", Some("en")),
                source("https://x/conflict", Some("hr")),
            ],
        );
        let raw = subtitles.enrich(
            "video",
            1,
            json!([
                {"id":1,"type":"sub","external":true,"external-filename":"https://x/eng.vtt"},
                {"id":2,"type":"sub","external":true,"external-filename":"https://x/conflict"}
            ])
            .to_string(),
        );
        assert!(
            super::super::track_preferences::parse_tracks(&raw)
                .iter()
                .all(|track| track.descriptor.language.is_none())
        );
        let entries = bounded(&[
            source("file:///tmp/sub.vtt", Some("en")),
            source("https://x/sub\n", Some("en")),
            source("https://x/sub", Some("ENG")),
        ]);
        assert_eq!(entries, vec![source("https://x/sub", Some("en"))]);
        assert!(!format!("{:?}", entries[0]).contains("https"));
    }

    #[test]
    fn downloaded_source_captions_keep_exact_provenance_and_session_owned_files() {
        let url = "https://caption.invalid/sub?edition=one&token=private";
        let mut metadata = SourceSubtitles::default();
        metadata.begin("video", 7, vec![source(url, Some("eng"))]);
        let stale = SourceSubtitleFile::from_bytes(b"WEBVTT\n").unwrap();
        let stale_path = stale.0.clone();
        assert!(!metadata.attach(6, url, stale));
        assert!(!stale_path.exists(), "stale worker files must be removed");
        let wrong = SourceSubtitleFile::from_bytes(b"WEBVTT\n").unwrap();
        assert!(!metadata.attach(
            7,
            "https://caption.invalid/sub?edition=two&token=private",
            wrong
        ));
        let file = SourceSubtitleFile::from_bytes(b"WEBVTT\n").unwrap();
        let path = file.0.clone();
        assert!(metadata.attach(7, url, file));
        let raw = json!([{"id":4,"type":"sub","external":true,"external-filename":path.to_string_lossy()}]).to_string();
        let enriched: serde_json::Value =
            serde_json::from_str(&metadata.enrich("video", 7, raw.clone())).unwrap();
        assert_eq!(enriched[0]["lang"], "en");
        assert_eq!(enriched[0]["nova-source-subtitle"], true);
        assert_eq!(metadata.enrich("other-video", 7, raw.clone()), raw);
        assert!(path.exists());
        metadata.begin("video", 8, vec![source(url, Some("hr"))]);
        assert!(
            !path.exists(),
            "replacement retires the old session's files"
        );
        assert_eq!(metadata.enrich("video", 8, raw.clone()), raw);
    }

    #[test]
    fn native_local_source_captions_load_and_recover_declared_facts() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
        struct Scratch(std::path::PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "nova-source-captions-{}-{}", std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        )));
        std::fs::create_dir(&scratch.0).unwrap();
        let video = scratch.0.join("audio.wav");
        let size = 80_000_u32;
        let mut wav = b"RIFF".to_vec();
        wav.extend((size + 36).to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16_u32.to_le_bytes());
        wav.extend(1_u16.to_le_bytes());
        wav.extend(1_u16.to_le_bytes());
        wav.extend(8000_u32.to_le_bytes());
        wav.extend(8000_u32.to_le_bytes());
        wav.extend(1_u16.to_le_bytes());
        wav.extend(8_u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend(size.to_le_bytes());
        wav.resize(wav.len() + size as usize, 128);
        std::fs::write(&video, wav).unwrap();
        let caption = SourceSubtitleFile::from_bytes(
            b"WEBVTT\n\n00:00.000 --> 00:05.000\nLocal caption fixture\n",
        )
        .unwrap();
        let video = video.to_string_lossy().into_owned();
        let path = caption.0.to_string_lossy().into_owned();
        let url = "https://caption.invalid/opaque?token=private";
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let requested = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let observed = requested.clone();
        let stopped = stop.clone();
        let worker = std::thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        observed.store(true, Ordering::SeqCst);
                        // Respond if the native whitelist ever regresses so
                        // this security assertion fails instead of hanging.
                        socket
                            .set_read_timeout(Some(Duration::from_secs(1)))
                            .unwrap();
                        let _ = socket.read(&mut [0; 4096]);
                        let _ = socket.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("caption listener failed: {error}"),
                }
            }
        });
        struct ListenerWorker(Arc<AtomicBool>, Option<std::thread::JoinHandle<()>>);
        impl Drop for ListenerWorker {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
                if let Some(worker) = self.1.take() {
                    worker.join().unwrap();
                }
            }
        }
        let _listener = ListenerWorker(stop, Some(worker));
        // This is the same syntactically valid static MPD as the native trace
        // that reached DASH nested_io_open (probe score 100), then denied HTTP
        // against the local file's file,crypto,data protocol whitelist. Plain
        // M3U/HLS alone would only test mpv's earlier playlist rejection.
        let dash = format!(
            r#"<?xml version="1.0" encoding="utf-8"?><MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT5S" minBufferTime="PT1S" profiles="urn:mpeg:dash:profile:isoff-on-demand:2011"><Period duration="PT5S"><AdaptationSet mimeType="video/mp4" contentType="video" segmentAlignment="true"><Representation id="1" bandwidth="10000" codecs="avc1.42c01e" width="16" height="16"><BaseURL>http://{address}/dash.mp4</BaseURL><SegmentBase indexRange="0-100"><Initialization range="0-100"/></SegmentBase></Representation></AdaptationSet></Period></MPD>"#
        );
        let reference = SourceSubtitleFile::from_bytes(dash.as_bytes()).unwrap();
        let reference_path = reference.0.to_string_lossy().into_owned();
        let reference_url = "https://caption.invalid/reference?token=private";
        let mpv = libmpv2::Mpv::with_initializer(|init| {
            init.set_property("config", false)?;
            init.set_property("vo", "null")?;
            init.set_property("ao", "null")?;
            init.set_property("pause", true)
        })
        .unwrap();
        super::super::set_http_header_fields(
            &mpv,
            &[
                (
                    "Authorization".into(),
                    "Bearer NOVA_TEST_SYNTHETIC_TOKEN".into(),
                ),
                ("Cookie".into(), "nova_test=synthetic".into()),
            ],
        )
        .unwrap();
        let mut metadata = SourceSubtitles::default();
        metadata.begin(
            &video,
            42,
            vec![source(url, Some("eng")), source(reference_url, None)],
        );
        assert!(metadata.attach(42, url, caption));
        assert!(metadata.attach(42, reference_url, reference));
        for reload in 0..2 {
            // The initial async fetch attaches through sub-add after video
            // starts; Android recovery instead retains local sub-files.
            if reload > 0 {
                super::super::set_string_list(
                    &mpv,
                    c"sub-files",
                    vec![path.clone(), reference_path.clone()],
                )
                .unwrap();
            }
            mpv.command("loadfile", &[&video]).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                assert!(Instant::now() < deadline, "local fixture did not load");
                // This private fixture owns its event queue. No media is fetched
                // over the network, and a second FileLoaded proves a real reload.
                let event = unsafe { &*libmpv2_sys::mpv_wait_event(mpv.ctx.as_ptr(), 0.01) };
                if event.event_id == libmpv2_sys::mpv_event_id_MPV_EVENT_FILE_LOADED {
                    break;
                }
            }
            metadata.load_pending(&mpv, &video, 42, false);
            assert!(
                !requested.load(Ordering::SeqCst),
                "cached DASH captions must not open a nested network request with video credentials (reload {reload})"
            );
            let raw = mpv.get_property::<String>("track-list").unwrap();
            let native = super::super::track_preferences::parse_tracks(&raw);
            let untagged = native.iter().find(|track| !track.audio).unwrap();
            assert!(
                untagged.descriptor.language.is_none(),
                "local files do not carry the source's declared language"
            );
            let enriched = metadata.enrich(&video, 42, raw.clone());
            let tracks = super::super::track_preferences::parse_tracks(&enriched);
            assert_eq!(select_track(&tracks, false, None, "en"), Some(untagged.id));
            let listed: serde_json::Value = serde_json::from_str(&enriched).unwrap();
            assert!(
                listed
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|track| track["nova-source-subtitle"] == true)
            );
            mpv.set_property("sid", "no").unwrap();
            metadata.enrich(&video, 42, raw);
            assert_eq!(
                mpv.get_property::<String>("sid").unwrap(),
                "no",
                "enrichment never changes a viewer selection"
            );
            assert!(mpv.get_property::<bool>("pause").unwrap());
        }
        mpv.command("stop", &[]).unwrap();
    }
}
