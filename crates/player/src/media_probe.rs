//! Best-effort duration metadata for completed local downloads, off the UI thread.
use libmpv2::Mpv;
use std::ffi::CString;
use std::path::Path;
use std::time::{Duration, Instant};

const PROBE_TIMEOUT: Duration = Duration::from_secs(8);

/// Read a local file's duration without using the shared player or a renderer.
///
/// This blocks the calling worker for up to eight seconds of event polling.
/// Container metadata is read before track selection; neither playback decoders
/// nor `avformat_find_stream_info` are started. Files whose duration needs more
/// analysis return `None`. Local HLS playlists may reference local files and AES
/// keys, but the demuxer's protocol whitelist excludes network transports.
/// No application, window, Android JVM, or GPU initialization is required.
pub fn probe_local_duration(path: &Path) -> Option<f64> {
    // An absolute filesystem path cannot be reinterpreted as an mpv URL, stdin,
    // or a protocol prefix. Reject directories/devices/FIFOs before opening.
    let path = path.canonicalize().ok()?;
    if !path.is_file() {
        return None;
    }
    #[cfg(target_os = "windows")]
    if matches!(
        path.components().next(),
        Some(std::path::Component::Prefix(prefix))
            if matches!(prefix.kind(), std::path::Prefix::UNC(..) | std::path::Prefix::VerbatimUNC(..))
    ) {
        return None;
    }
    let filename = CString::new(path.to_str()?).ok()?;
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let mpv = Mpv::with_initializer(|init| {
        for (name, value) in [
            ("config", "no"),
            ("load-scripts", "no"),
            ("scripts", ""),
            ("ytdl", "no"),
            ("osc", "no"),
            ("terminal", "no"),
            ("input-default-bindings", "no"),
            ("input-vo-keyboard", "no"),
            ("autoload-files", "no"),
            ("vo", "null"),
            ("ao", "null"),
            ("hwdec", "no"),
            ("vid", "no"),
            ("aid", "no"),
            ("sid", "no"),
            ("pause", "yes"),
            ("idle", "yes"),
            ("keep-open", "yes"),
            // Force lavf so mpv's own playlist loader cannot follow URLs.
            ("demuxer", "lavf"),
            ("demuxer-lavf-probe-info", "no"),
            // HLS needs references enabled to open its local segments/keys.
            // Only file/crypto are permitted, including nested opens.
            ("access-references", "yes"),
            ("demuxer-lavf-o", "protocol_whitelist=[file,crypto]"),
            ("demuxer-lavf-propagate-opts", "yes"),
        ] {
            init.set_property(name, value)?;
        }
        Ok(())
    })
    .ok()?;
    let handle = mpv.ctx.as_ptr();
    // on_preloaded runs after demuxing and before track selection/decoder
    // creation. Without this barrier, selecting no tracks can unload the file
    // before a worker observes its duration. This core belongs only to us.
    if unsafe { libmpv2_sys::mpv_hook_add(handle, 1, c"on_preloaded".as_ptr(), 0) } < 0 {
        return None;
    }
    let mut args = [
        c"loadfile".as_ptr(),
        filename.as_ptr(),
        c"replace".as_ptr(),
        std::ptr::null(),
    ];
    // Async load keeps even a slow open inside the event-poll deadline.
    if unsafe { libmpv2_sys::mpv_command_async(handle, 2, args.as_mut_ptr()) } < 0 {
        return None;
    }
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        // Event storage remains valid until this handle's next wait_event.
        let event =
            unsafe { &*libmpv2_sys::mpv_wait_event(handle, remaining.as_secs_f64().min(0.1)) };
        if event.event_id == libmpv2_sys::mpv_event_id_MPV_EVENT_HOOK {
            if event.data.is_null() {
                return None;
            }
            let hook = unsafe { &*event.data.cast::<libmpv2_sys::mpv_event_hook>() };
            let hook_id = hook.id;
            let duration = mpv.get_property::<f64>("duration").ok();
            // Always release the barrier before the last client is destroyed.
            // All tracks remain disabled, so continuation cannot decode media.
            unsafe { libmpv2_sys::mpv_hook_continue(handle, hook_id) };
            return duration.filter(|duration| duration.is_finite() && *duration > 10.0);
        }
        if event.error < 0
            || matches!(
                event.event_id,
                libmpv2_sys::mpv_event_id_MPV_EVENT_END_FILE
                    | libmpv2_sys::mpv_event_id_MPV_EVENT_SHUTDOWN
            )
        {
            return None;
        }
    }
    // Dropping this sole libmpv client cancels its pending async load and tears
    // down its isolated core; the application's Player is never accessed.
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io;
    use std::path::PathBuf;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "nova-duration-probe-{}-{unique}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn wav(&self, name: &str, seconds: u32) -> PathBuf {
            // PCM WAV encodes duration in its ordinary container headers, so
            // the test needs no external fixture, codec encoder or renderer.
            let rate = 8_000_u32;
            let data_len = seconds * rate * 2;
            let mut bytes = Vec::new();
            bytes.extend(b"RIFF");
            bytes.extend((36 + data_len).to_le_bytes());
            bytes.extend(b"WAVEfmt ");
            bytes.extend(16_u32.to_le_bytes());
            bytes.extend(1_u16.to_le_bytes()); // PCM
            bytes.extend(1_u16.to_le_bytes()); // mono
            bytes.extend(rate.to_le_bytes());
            bytes.extend((rate * 2).to_le_bytes());
            bytes.extend(2_u16.to_le_bytes());
            bytes.extend(16_u16.to_le_bytes());
            bytes.extend(b"data");
            bytes.extend(data_len.to_le_bytes());
            bytes.resize(44 + data_len as usize, 0);
            let path = self.0.join(name);
            fs::write(&path, bytes).unwrap();
            path
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn native_probe_reads_wav_and_local_hls_without_playback() {
        let fixture = Fixture::new();
        let wav = fixture.wav("episode.wav", 12);
        let duration = probe_local_duration(&wav).expect("native WAV metadata");
        assert!((duration - 12.0).abs() < 0.01);
        let playlist = fixture.0.join("episode.m3u8");
        fs::write(&playlist, b"#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:12\n#EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:12.000,\nepisode.wav\n#EXT-X-ENDLIST\n").unwrap();
        let duration = probe_local_duration(&playlist).expect("local HLS metadata");
        assert!((duration - 12.0).abs() < 0.01);
        assert_eq!(probe_local_duration(&fixture.wav("short.wav", 10)), None);
        assert_eq!(probe_local_duration(&fixture.0), None);
        assert_eq!(probe_local_duration(&fixture.0.join("missing.mp4")), None);
    }

    #[test]
    fn local_hls_cannot_open_a_network_segment() {
        let fixture = Fixture::new();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let playlist = fixture.0.join("remote.m3u8");
        fs::write(&playlist, format!("#EXTM3U\n#EXT-X-TARGETDURATION:12\n#EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:12.000,\nhttp://{}/episode.ts\n#EXT-X-ENDLIST\n", listener.local_addr().unwrap())).unwrap();
        assert_eq!(probe_local_duration(&playlist), None);
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }
}
