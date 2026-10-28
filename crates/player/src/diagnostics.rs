//! On-demand, source-owned native facts for the player statistics overlay.
use crate::Player;
use libmpv2::{GetData, Mpv};
use serde_json::Value;
use std::ffi::CString;
use std::ptr::NonNull;
use std::sync::atomic::Ordering;

#[derive(Clone, Debug, Default)]
pub struct DiagnosticsSnapshot {
    pub session: u64,
    pub video_width: Option<u64>,
    pub video_height: Option<u64>,
    pub fps: Option<f64>,
    pub video_codec: Option<String>,
    pub video_profile: Option<String>,
    pub hwdec: Option<String>,
    pub video_bitrate_bps: Option<f64>,
    pub output_drops: Option<u64>,
    pub decoder_drops: Option<u64>,
    pub buffer_seconds: Option<f64>,
    pub buffer_bytes: Option<u64>,
    pub audio_codec: Option<String>,
    pub audio_sample_rate: Option<u64>,
    pub audio_channels: Option<String>,
    pub av_sync_seconds: Option<f64>,
    pub playback_speed: Option<f64>,
    pub renderer: Option<String>,
    pub display_fps: Option<f64>,
    pub viewport_width: Option<u64>,
    pub viewport_height: Option<u64>,
    pub via_network: Option<bool>,
}

fn finite_nonnegative(value: f64) -> Option<f64> {
    (value.is_finite() && value >= 0.0).then_some(value)
}

fn finite_positive(value: f64) -> Option<f64> {
    (value.is_finite() && value > 0.0).then_some(value)
}

fn positive(value: i64) -> Option<u64> {
    (value > 0).then_some(value as u64)
}

fn counter(value: i64) -> Option<u64> {
    (value >= 0).then_some(value as u64)
}

// Native codec/channel identifiers are short tokens, not arbitrary metadata.
// Reject paths, URLs, control characters and long descriptions entirely.
fn safe_label(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()
        && value.len() <= 64
        && value.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '-' | '_' | '+' | '(' | ')')
        }))
    .then(|| value.to_owned())
}

// This separately owned client keeps the core alive during worker sampling.
struct DiagnosticsClient(NonNull<libmpv2_sys::mpv_handle>);

impl DiagnosticsClient {
    fn new(mpv: &Mpv) -> Option<Self> {
        // SAFETY: the primary handle is live under the player mutex. The
        // returned client is separately owned, or null if creation failed.
        NonNull::new(unsafe {
            libmpv2_sys::mpv_create_client(mpv.ctx.as_ptr(), c"nova_diagnostics".as_ptr())
        })
        .map(Self)
    }
}

impl Drop for DiagnosticsClient {
    fn drop(&mut self) {
        // SAFETY: this client is uniquely owned and destroyed once.
        unsafe { libmpv2_sys::mpv_destroy(self.0.as_ptr()) };
    }
}

trait PropertyValue: Sized {
    fn read(client: &DiagnosticsClient, name: &str) -> libmpv2::Result<Self>;
}

macro_rules! native_value {
    ($type:ty, $format:expr) => {
        impl PropertyValue for $type {
            fn read(client: &DiagnosticsClient, name: &str) -> libmpv2::Result<Self> {
                let name = CString::new(name)?;
                <$type as GetData>::get_from_c_void(|out| {
                    // SAFETY: GetData supplies storage for the matching format;
                    // the client and property name remain live during the call.
                    let result = unsafe {
                        libmpv2_sys::mpv_get_property(
                            client.0.as_ptr(),
                            name.as_ptr(),
                            $format,
                            out,
                        )
                    };
                    if result < 0 {
                        Err(libmpv2::Error::Raw(result))
                    } else {
                        Ok(())
                    }
                })
            }
        }
    };
}
native_value!(String, libmpv2_sys::mpv_format_MPV_FORMAT_STRING);
native_value!(i64, libmpv2_sys::mpv_format_MPV_FORMAT_INT64);
native_value!(f64, libmpv2_sys::mpv_format_MPV_FORMAT_DOUBLE);

impl PropertyValue for bool {
    fn read(client: &DiagnosticsClient, name: &str) -> libmpv2::Result<Self> {
        let name = CString::new(name)?;
        let mut value: std::os::raw::c_int = 0;
        // SAFETY: mpv FLAG writes a C int to this live, correctly sized slot.
        let result = unsafe {
            libmpv2_sys::mpv_get_property(
                client.0.as_ptr(),
                name.as_ptr(),
                libmpv2_sys::mpv_format_MPV_FORMAT_FLAG,
                std::ptr::from_mut(&mut value).cast(),
            )
        };
        if result < 0 {
            Err(libmpv2::Error::Raw(result))
        } else {
            Ok(value != 0)
        }
    }
}

trait PropertyReader {
    fn get_property<T: PropertyValue + GetData>(&self, name: &str) -> libmpv2::Result<T>;
}
impl PropertyReader for DiagnosticsClient {
    fn get_property<T: PropertyValue + GetData>(&self, name: &str) -> libmpv2::Result<T> {
        T::read(self, name)
    }
}
#[cfg(test)]
impl PropertyReader for Mpv {
    fn get_property<T: PropertyValue + GetData>(&self, name: &str) -> libmpv2::Result<T> {
        Mpv::get_property(self, name)
    }
}

fn node(mpv: &impl PropertyReader, property: &str) -> Value {
    mpv.get_property::<String>(property)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or(Value::Null)
}

fn node_positive(value: &Value) -> Option<u64> {
    value.as_i64().and_then(positive)
}

fn label_property(mpv: &impl PropertyReader, property: &str) -> Option<String> {
    mpv.get_property::<String>(property)
        .ok()
        .and_then(|s| safe_label(&s))
}

impl Player {
    /// Read a bounded set of native statistics on a worker while the overlay is visible.
    /// Contended locks and sources in transition yield no sample.
    pub fn diagnostics_snapshot(&self) -> Option<DiagnosticsSnapshot> {
        let state = &self.state;
        let session = state.screenshot_session.load(Ordering::SeqCst);
        if !state.started.load(Ordering::SeqCst) || state.source_waiting.load(Ordering::SeqCst) {
            return None;
        }
        if state.pending.try_lock().ok()?.is_some() {
            return None;
        }
        let source = state.preference_source.try_lock().ok()?.clone();
        if source.is_empty() {
            return None;
        }
        let renderer = state.underlay.try_lock().ok()?.as_ref().map(|_| {
            if cfg!(target_os = "android") {
                "OpenGL ES"
            } else {
                "OpenGL"
            }
            .to_owned()
        });
        let client = {
            let mpv_guard = state.mpv.try_lock().ok()?;
            DiagnosticsClient::new(mpv_guard.as_ref()?)?
        };
        let mpv = &client;
        if mpv.get_property::<String>("path").ok().as_deref() != Some(source.as_str())
            || mpv.get_property::<bool>("idle-active").ok() != Some(false)
        {
            return None;
        }
        let video = node(mpv, "video-params");
        let audio = node(mpv, "audio-params");
        let track = node(mpv, "current-tracks/audio");
        let cache = node(mpv, "demuxer-cache-state");
        let snapshot = DiagnosticsSnapshot {
            session,
            video_width: node_positive(&video["w"]),
            video_height: node_positive(&video["h"]),
            fps: mpv
                .get_property::<f64>("container-fps")
                .ok()
                .and_then(finite_positive)
                .or_else(|| {
                    mpv.get_property::<f64>("estimated-vf-fps")
                        .ok()
                        .and_then(finite_positive)
                }),
            video_codec: label_property(mpv, "video-format"),
            video_profile: video["profile"].as_str().and_then(safe_label),
            hwdec: label_property(mpv, "hwdec-current"),
            video_bitrate_bps: mpv
                .get_property::<f64>("video-bitrate")
                .ok()
                .and_then(finite_nonnegative),
            output_drops: mpv
                .get_property::<i64>("frame-drop-count")
                .ok()
                .and_then(counter),
            decoder_drops: mpv
                .get_property::<i64>("decoder-frame-drop-count")
                .ok()
                .and_then(counter),
            buffer_seconds: mpv
                .get_property::<f64>("demuxer-cache-duration")
                .ok()
                .and_then(finite_nonnegative),
            buffer_bytes: cache["fw-bytes"].as_i64().and_then(counter),
            audio_codec: label_property(mpv, "audio-codec-name")
                .or_else(|| track["codec"].as_str().and_then(safe_label)),
            audio_sample_rate: node_positive(&audio["samplerate"]),
            audio_channels: audio["channels"].as_str().and_then(safe_label),
            av_sync_seconds: mpv
                .get_property::<f64>("avsync")
                .ok()
                .filter(|v| v.is_finite()),
            playback_speed: mpv
                .get_property::<f64>("speed")
                .ok()
                .and_then(finite_positive),
            renderer,
            display_fps: mpv
                .get_property::<f64>("display-fps")
                .ok()
                .and_then(finite_positive),
            viewport_width: mpv.get_property::<i64>("osd-width").ok().and_then(positive),
            viewport_height: mpv
                .get_property::<i64>("osd-height")
                .ok()
                .and_then(positive),
            via_network: mpv.get_property::<bool>("demuxer-via-network").ok(),
        };
        if state.screenshot_session.load(Ordering::SeqCst) != session
            || !state.started.load(Ordering::SeqCst)
            || state.source_waiting.load(Ordering::SeqCst)
            || state.pending.try_lock().ok()?.is_some()
            || *state.preference_source.try_lock().ok()? != source
            || mpv.get_property::<String>("path").ok().as_deref() != Some(source.as_str())
        {
            return None;
        }
        Some(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_numbers_and_preserves_zero_counters() {
        assert_eq!(finite_nonnegative(f64::NAN), None);
        assert_eq!(finite_positive(f64::INFINITY), None);
        assert_eq!(finite_nonnegative(-1.0), None);
        assert_eq!(finite_positive(0.0), None);
        assert_eq!(counter(-1), None);
        assert_eq!(counter(0), Some(0));
        assert_eq!(node_positive(&serde_json::json!(-4)), None);
    }

    #[test]
    fn labels_exclude_paths_urls_and_unbounded_metadata() {
        assert_eq!(safe_label("  5.1(side)  "), Some("5.1(side)".into()));
        assert_eq!(safe_label("https://example.com/video"), None);
        assert_eq!(safe_label("/private/file"), None);
        assert_eq!(safe_label(&"a".repeat(65)), None);
        assert_eq!(safe_label("aac\nsecret"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn native_paused_snapshot_is_read_only_and_source_guarded() {
        const CHILD: &str = "NOVA_PLAYER_DIAGNOSTICS_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(CHILD, "1")
                .args([
                    "--exact",
                    "diagnostics::tests::native_paused_snapshot_is_read_only_and_source_guarded",
                    "--nocapture",
                ])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        use crate::AppWindow;
        use std::time::{Duration, Instant};

        i_slint_backend_testing::init_integration_test_with_mock_time();
        let app = AppWindow::new().unwrap();
        let player = Player::setup(&app);
        let directory =
            std::env::temp_dir().join(format!("nova-diagnostics-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(directory.clone());
        let path = directory.join("fixture.y4m");
        let mut video = b"YUV4MPEG2 W32 H18 F30:1 Ip A1:1 C420jpeg\n".to_vec();
        for _ in 0..300 {
            video.extend(b"FRAME\n");
            video.extend([128u8; 32 * 18 * 3 / 2]);
        }
        std::fs::write(&path, video).unwrap();
        let source = path.to_str().unwrap().to_owned();
        let mpv = Mpv::with_initializer(|init| {
            init.set_property("config", false)?;
            init.set_property("vo", "null")?;
            init.set_property("ao", "null")?;
            init.set_property("pause", true)?;
            init.set_property("keep-open", true)
        })
        .unwrap();
        mpv.set_property("speed", 1.25_f64).unwrap();
        mpv.command("loadfile", &[&source]).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while node_positive(&node(&mpv, "video-params")["w"]) != Some(32) {
            assert!(Instant::now() < deadline, "video fixture did not load");
            std::thread::sleep(Duration::from_millis(10));
        }
        *player.state.mpv.lock().unwrap() = Some(mpv);
        *player.state.preference_source.lock().unwrap() = source.clone();
        player.state.started.store(true, Ordering::SeqCst);
        let client = {
            let guard = player.state.mpv.lock().unwrap();
            DiagnosticsClient::new(guard.as_ref().unwrap()).unwrap()
        };
        {
            let _guard = player
                .state
                .mpv
                .try_lock()
                .expect("client releases player mutex");
            assert_eq!(client.get_property::<f64>("speed").unwrap(), 1.25);
        }
        drop(client);
        let before = {
            let guard = player.state.mpv.lock().unwrap();
            let mpv = guard.as_ref().unwrap();
            (
                mpv.get_property::<bool>("pause").unwrap(),
                mpv.get_property::<f64>("time-pos").unwrap(),
                label_property(mpv, "video-format"),
                mpv.get_property::<i64>("frame-drop-count")
                    .ok()
                    .and_then(counter),
                mpv.get_property::<i64>("decoder-frame-drop-count")
                    .ok()
                    .and_then(counter),
                node(mpv, "demuxer-cache-state")["fw-bytes"]
                    .as_i64()
                    .and_then(counter),
            )
        };
        let snapshot = player
            .diagnostics_snapshot()
            .expect("loaded source snapshot");
        assert_eq!(
            (snapshot.video_width, snapshot.video_height),
            (Some(32), Some(18))
        );
        assert_eq!(snapshot.fps, Some(30.0));
        assert!(before.2.is_some(), "fixture must expose an actual codec");
        assert_eq!(snapshot.video_codec, before.2);
        assert_eq!(snapshot.output_drops, before.3);
        assert_eq!(snapshot.decoder_drops, before.4);
        assert_eq!(snapshot.buffer_bytes, before.5);
        assert_eq!(snapshot.playback_speed, Some(1.25));
        assert_eq!(snapshot.via_network, Some(false));
        {
            let guard = player.state.mpv.lock().unwrap();
            let mpv = guard.as_ref().unwrap();
            assert_eq!(mpv.get_property::<bool>("pause").unwrap(), before.0);
            assert_eq!(mpv.get_property::<f64>("time-pos").unwrap(), before.1);
            assert_eq!(mpv.get_property::<f64>("speed").unwrap(), 1.25);
            assert!(
                player.diagnostics_snapshot().is_none(),
                "contended engine must be skipped"
            );
        }
        *player.state.preference_source.lock().unwrap() = "different source".into();
        assert!(player.diagnostics_snapshot().is_none());
        *player.state.preference_source.lock().unwrap() = source;
        *player.state.pending.lock().unwrap() = Some("pending source".into());
        assert!(player.diagnostics_snapshot().is_none());
        *player.state.pending.lock().unwrap() = None;
        player.state.source_waiting.store(true, Ordering::SeqCst);
        assert!(player.diagnostics_snapshot().is_none());
        player.state.source_waiting.store(false, Ordering::SeqCst);
        let session = snapshot.session;
        player.close();
        assert_ne!(player.screenshot_session(), session);
        assert!(player.diagnostics_snapshot().is_none());
    }
}
