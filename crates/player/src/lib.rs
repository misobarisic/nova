//! In-app video playback through **mpv** (`libmpv2`) in the **same window**
//! as the catalog UI — mirroring the `slint_stremio_player` reference
//! prototype. The video renders *underneath* the whole window: mpv draws into
//! the same OpenGL framebuffer the Slint scene is composited onto, at
//! `RenderingState::BeforeRendering`. While a stream plays, the catalog is
//! hidden, the window background turns transparent, and the Slint scene only
//! paints the player overlay (status text + OSD) on top of the video.
//!
//! The same architecture runs on Android, against source-built `libmpv.so`
//! provided by the target Nix shell (recipe in `nix/native-media`, link
//! search path in `build.rs`): Slint's Android backend renders with Skia on its
//! own EGL/GLES surface and hands the rendering notifier a real
//! `GraphicsAPI::NativeOpenGL`, so mpv composites into the same default
//! framebuffer underneath the overlay — no `SurfaceView`, no z-order juggling.
//! Two things are Android-specific: function-pointer lookup ([`android_gl`],
//! because EGL only guarantees *extension* entry points) and the mpv options /
//! hardware-decoder handling in [`Player::setup`] and [`Player::tick`].
//!
//! One [`Player`] is created at app startup (it owns the mpv core + render
//! context for the whole session). Picking a stream calls [`Player::play`],
//! which shows the overlay and queues a `loadfile` for the next frame;
//! [`Player::tick`] (driven by a 250 ms timer in `desktop::run`) mirrors mpv
//! state into the overlay properties, detects end-of-stream and surfaces a
//! timeout error when playback never starts. [`Player::close`] stops mpv and
//! returns to the catalog.

#[cfg(any(target_os = "android", test))]
mod android_pip;
#[cfg(any(target_os = "android", test))]
mod android_recovery;
#[cfg(target_os = "android")]
pub use android_pip::note_android_pip_event;
#[cfg(any(target_os = "android", test))]
mod mpv_load;

mod audio_processing;
mod chapters;
mod media_badges;
mod media_probe;
pub use media_probe::probe_local_duration;
#[cfg(target_os = "linux")]
pub mod mpris;

mod sleep_timer;
pub use sleep_timer::elapsed_realtime_ms;

mod screenshot;
pub use screenshot::{ScreenshotContext, ScreenshotError, ScreenshotSaved};
#[cfg(any(target_os = "android", all(test, target_os = "linux")))]
mod gpu_screenshot;

mod source_subtitles;
pub use source_subtitles::{SourceSubtitle, SourceSubtitleFile, SourceSubtitleRequest};

mod addon_subtitles;
#[cfg(any(target_os = "windows", target_os = "linux", test))]
mod desktop_decoder;
mod subtitle_position;
mod subtitle_sync;
pub use addon_subtitles::OnlineSubtitle;
mod secondary_subtitle_position;
mod secondary_subtitles;
mod track_preferences;
use libmpv2::Mpv;
use nova_config::playback_preferences::{
    SessionPreferences, TitlePreferences, TrackChoice, select_track,
};
use nova_ui::{AppWindow, TrackRow};
use slint::{ComponentHandle, GraphicsAPI, Model, RenderingState, VecModel};
use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_void};
use std::rc::Rc;
#[cfg(target_os = "android")]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[cfg(not(target_os = "android"))]
fn configure_desktop_decoder(mpv: &Mpv) -> libmpv2::Result<()> {
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    {
        desktop_decoder::configure(mpv, nova_config::active_cache_settings().desktop_hwdec)
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    mpv.set_property("hwdec", "auto")
}

/// Slint owns the window's presentation clock. mpv must not wait for a video
/// deadline inside BeforeRendering, or every UI animation inherits video FPS.
/// Zero offset schedules the video update at its actual deadline, avoiding the
/// early presentation/A-V offset from disabling the render wait on its own.
/// See mpv's render.h, MPV_RENDER_PARAM_BLOCK_FOR_TARGET_TIME and the render()
/// contract: https://github.com/mpv-player/mpv/blob/master/include/mpv/render.h
fn configure_render_timing(mpv: &Mpv) -> libmpv2::Result<()> {
    mpv.set_property("video-timing-offset", 0.0_f64)
}

fn mpv_http_header_fields(headers: &[(String, String)]) -> Vec<String> {
    let mut fields = Vec::new();
    let mut total_bytes = 0usize;
    for (name, value) in headers.iter().take(16) {
        let valid_name = !name.is_empty()
            && name.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(
                        byte,
                        b'!' | b'#'
                            | b'$'
                            | b'%'
                            | b'&'
                            | b'\''
                            | b'*'
                            | b'+'
                            | b'-'
                            | b'.'
                            | b'^'
                            | b'_'
                            | b'`'
                            | b'|'
                            | b'~'
                    )
            });
        if !valid_name
            || value.bytes().any(|byte| byte < b' ' || byte == 127)
            || matches!(
                name.to_ascii_lowercase().as_str(),
                "host" | "content-length" | "connection" | "proxy-authorization"
            )
        {
            continue;
        }
        let size = name.len().saturating_add(value.len()).saturating_add(2);
        if total_bytes.saturating_add(size) > 8 * 1024 {
            break;
        }
        total_bytes += size;
        fields.push(format!("{name}: {value}"));
    }
    fields
}

fn set_http_header_fields(mpv: &Mpv, headers: &[(String, String)]) -> libmpv2::Result<()> {
    set_string_list(mpv, c"http-header-fields", mpv_http_header_fields(headers))
}

fn set_string_list(
    mpv: &Mpv,
    property: &std::ffi::CStr,
    fields: Vec<String>,
) -> libmpv2::Result<()> {
    use libmpv2_sys::{mpv_node, mpv_node__bindgen_ty_1, mpv_node_list};
    let fields = fields
        .into_iter()
        .map(std::ffi::CString::new)
        .collect::<Result<Vec<_>, _>>()?;
    let mut values = fields
        .iter()
        .map(|field| mpv_node {
            format: libmpv2::mpv_format::String,
            u: mpv_node__bindgen_ty_1 {
                string: field.as_ptr().cast_mut(),
            },
        })
        .collect::<Vec<_>>();
    let mut list = mpv_node_list {
        num: values.len() as c_int,
        values: values.as_mut_ptr(),
        keys: std::ptr::null_mut(),
    };
    let mut node = mpv_node {
        format: libmpv2::mpv_format::Array,
        u: mpv_node__bindgen_ty_1 { list: &mut list },
    };
    // A node array preserves commas and backslashes inside list values.
    // The synchronous API copies the strings; all pointers remain valid for
    // this call, and mpv must not free the memory owned by these Rust values.
    let status = unsafe {
        libmpv2_sys::mpv_set_property(
            mpv.ctx.as_ptr(),
            property.as_ptr(),
            libmpv2::mpv_format::Node,
            (&mut node as *mut mpv_node).cast(),
        )
    };
    if status < 0 {
        Err(libmpv2::Error::Raw(status))
    } else {
        Ok(())
    }
}

// Android-only helpers that live outside mpv (JNI glue).
#[cfg(target_os = "android")]
mod external;

// Desktop idle inhibition while video plays (D-Bus ScreenSaver / logind).
#[cfg(target_os = "linux")]
mod idle;
#[cfg(target_os = "linux")]
pub use crate::idle::converge_idle_inhibit;

// Android-only helpers that live outside mpv: handing a stream to the system
// (`ACTION_VIEW`) when in-app playback is unavailable, and the JVM/activity
// pointers that JNI needs. Re-exported here so `src/app.rs` and `android_main`
// keep talking to `crate::player` on every platform.
#[cfg(target_os = "android")]
pub use crate::external::android_activity_ptr;
#[cfg(target_os = "android")]
pub use crate::external::converge_screen_on;
#[cfg(target_os = "android")]
pub use crate::external::device_model;
#[cfg(target_os = "android")]
pub use crate::external::move_task_to_back;
#[cfg(target_os = "android")]
pub use crate::external::network_changed;
#[cfg(target_os = "android")]
pub use crate::external::open_browser;
#[cfg(target_os = "android")]
pub use crate::external::open_external;
#[cfg(target_os = "android")]
pub use crate::external::picture_in_picture_keeps_playing;
#[cfg(target_os = "android")]
pub use crate::external::reassert_system_bars;
#[cfg(target_os = "android")]
pub use crate::external::set_android_app;
#[cfg(target_os = "android")]
pub use crate::external::set_android_runtime;
#[cfg(target_os = "android")]
pub use crate::external::set_clipboard;
#[cfg(target_os = "android")]
pub use crate::external::set_content_frame_rate;
#[cfg(target_os = "android")]
pub use crate::external::set_system_bars_hidden;

/// Direct MediaCodec interop ("HW+"): decoder output stays on the GPU as
/// EGLImages. [`State::hwdec_checked`] watches whether it actually engaged; if
/// not, [`State::advance_decoder`] steps to the next chain entry.
#[cfg(target_os = "android")]
const HWDEC_PREFERRED: &str = "mediacodec";
/// Copy-back fallback ("HW"): hardware decode, frames bounce through system
/// memory.
#[cfg(target_os = "android")]
const HWDEC_FALLBACK: &str = "mediacodec-copy";
/// Software decode ("SW"): the terminal fallback, and the whole chain when the
/// user explicitly selects SW.
#[cfg(target_os = "android")]
const HWDEC_SOFTWARE: &str = "no";

/// Overlay badge for the active decoder, shown from mpv's `hwdec-current`:
/// `HW+` = direct MediaCodec (frames stay on the GPU), `HW` = copy-back
/// MediaCodec, `SW` = software (including mpv reporting no hardware decoder).
#[cfg(target_os = "android")]
fn decode_label(hwdec_current: &str) -> &'static str {
    if hwdec_current == HWDEC_PREFERRED {
        "HW+"
    } else if hwdec_current.contains("mediacodec") {
        "HW"
    } else {
        "SW"
    }
}

/// Decoder label → runtime-picker index (0 = HW+, 1 = HW, 2 = SW). Mirrors
/// [`nova_config::AndroidHwdec::index`] and drives the badge/menu selection
/// from mpv's actual `hwdec-current`.
#[cfg(target_os = "android")]
fn decode_index(hwdec_current: &str) -> i32 {
    match decode_label(hwdec_current) {
        "HW+" => 0,
        "HW" => 1,
        _ => 2,
    }
}

#[cfg(target_os = "android")]
#[link(name = "log")]
unsafe extern "C" {
    /// bionic `liblog`'s write entry point (declared at module scope so the
    /// linker definitely pulls in `liblog`).
    fn __android_log_write(prio: c_int, tag: *const c_char, text: *const c_char) -> c_int;
}

/// Player-scoped logcat line (tag `nova-player`). Native stderr never reaches
/// logcat on-device, so the Android readiness decisions below are logged here
/// instead (and by `player_external_android`'s fallback). Filter with:
/// `adb logcat -s nova-player`
#[cfg(target_os = "android")]
pub(crate) fn alog(msg: &str) {
    const ANDROID_LOG_INFO: c_int = 4;
    if let Ok(text) = std::ffi::CString::new(msg) {
        unsafe {
            __android_log_write(
                ANDROID_LOG_INFO,
                c"nova-player".as_ptr() as *const c_char,
                text.as_ptr(),
            )
        };
    }
}

// The listener and rendering notifier run on the Slint event-loop thread.
// Keep only a weak reference so activity events address the current player
// immediately, rather than a global resume flag that can leak to a later file.
#[cfg(target_os = "android")]
static ANDROID_PLAYER: Mutex<Option<std::sync::Weak<State>>> = Mutex::new(None);
#[cfg(target_os = "android")]
static ANDROID_FOREGROUND: AtomicBool = AtomicBool::new(true);

#[cfg(target_os = "android")]
pub fn note_android_pause() {
    ANDROID_FOREGROUND.store(false, Ordering::SeqCst);
    if let Some(state) = ANDROID_PLAYER
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|s| s.upgrade())
    {
        // Pause the engine immediately; the UI timer may stop completely
        // after Home/screen-off. Also cover a load waiting for its renderer.
        state.pending_paused.store(true, Ordering::SeqCst);
        let mut observation = (None, None);
        if let Ok(mpv) = state.mpv.lock()
            && let Some(mpv) = mpv.as_ref()
        {
            if state.pending.lock().unwrap().is_none() {
                observation = (
                    mpv.get_property("time-pos").ok(),
                    mpv.get_property("pause").ok(),
                );
            }
            if let Err(error) = mpv.set_property("pause", true) {
                alog(&format!("lifecycle pause failed: {error}"));
            }
        }
        if let Some(app) = state.app.upgrade()
            && app.get_player_open()
        {
            app.set_is_paused(true);
            // The activity listener checkpoints immediately after this call;
            // its last timer mirror can lag the position observed above.
            if let Some(position) = observation.0 {
                app.set_position(position as f32);
            }
        }
        converge_screen_on(false);
        let mut recovery = state.recovery.lock().unwrap();
        recovery.observe(observation.0, observation.1);
        recovery.pause();
        alog(&format!("lifecycle pause: {recovery:?}"));
    }
}

#[cfg(target_os = "android")]
pub fn note_android_resume() {
    resume_android_playback(nova_config::active_cache_settings().android_auto_continue);
}

#[cfg(target_os = "android")]
fn resume_android_playback(auto_continue: bool) {
    ANDROID_FOREGROUND.store(true, Ordering::SeqCst);
    if let Some(state) = ANDROID_PLAYER
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|s| s.upgrade())
    {
        // Never briefly auto-continue audio after a deadline elapsed in suspend.
        if state.enforce_sleep_deadline() {
            return;
        }
        let mut recovery = state.recovery.lock().unwrap();
        let play = resume_android_recovery(
            &mut recovery,
            auto_continue,
            *state.party_playback_rate.lock().unwrap(),
        );
        alog(&format!("lifecycle resume: {recovery:?}"));
        drop(recovery);
        if play && !state.start_pause_hold.load(Ordering::SeqCst) {
            state.pending_paused.store(false, Ordering::SeqCst);
            if let Ok(mpv) = state.mpv.lock()
                && let Some(mpv) = mpv.as_ref()
                && let Err(error) = mpv.set_property("pause", false)
            {
                alog(&format!("lifecycle auto-continue failed: {error}"));
            }
            converge_screen_on(true);
        }
        if let Some(app) = state.app.upgrade()
            && app.get_player_open()
        {
            if play && !state.start_pause_hold.load(Ordering::SeqCst) {
                app.set_is_paused(false);
            }
            // Re-arm activity rather than only setting visibility: an old
            // inactivity timer may already be due after screen-off.
            app.invoke_osd_mouse_moved();
        }
    }
}

#[cfg(any(target_os = "android", test))]
fn resume_android_recovery(
    recovery: &mut android_recovery::Recovery,
    auto_continue: bool,
    party_rate: Option<f64>,
) -> bool {
    // A party must fetch its host's current timeline before resuming. Native
    // auto-continue otherwise plays stale media while the app reconnects.
    recovery.resume(auto_continue && party_rate.is_none())
}

/// Bundled subtitle font (Roboto Regular; Apache-2.0, license kept next to the
/// file). The vendored Android libmpv is built without fontconfig and with
/// libass's system-font provider disabled, so on Android libass only ever sees
/// fonts embedded in the media — a text subtitle that brings no font of its own
/// has nothing to rasterize and renders blank (the track is still listed and
/// selectable). Unpacking this font and pointing mpv's `sub-fonts-dir`/
/// `sub-font` at it gives libass a default for exactly those subtitles; media
/// with embedded fonts keeps using them. Keep the unmodified subtitle face:
/// UI Roboto omits six emoji mappings that libass cannot fall back for here.
/// The family name must match the face.
#[cfg(target_os = "android")]
const SUBTITLE_FONT: &[u8] = include_bytes!("../../../assets/fonts/Roboto-Subtitles.ttf");
#[cfg(target_os = "android")]
const SUBTITLE_FONT_FAMILY: &str = "Roboto";

/// Write `bytes` to `path` unless a file of the same length is already there.
/// Factored out of [`install_subtitle_font`] so the size check is host-testable.
#[cfg(any(target_os = "android", test))]
fn write_if_size_differs(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    if std::fs::metadata(path).map(|m| m.len()).ok() != Some(bytes.len() as u64) {
        std::fs::write(path, bytes)?;
    }
    Ok(())
}

/// Unpack the bundled subtitle font into the app's private dir and return that
/// dir (the value for mpv's `sub-fonts-dir`). Rewrites on a size change so an
/// app update ships a new font. `None` when the dir can't be written, in which
/// case the caller falls back to the device's system fonts.
#[cfg(target_os = "android")]
fn install_subtitle_font() -> Option<std::path::PathBuf> {
    let dir = nova_config::android_fonts_dir();
    std::fs::create_dir_all(&dir).ok()?;
    write_if_size_differs(&dir.join("Roboto-Regular.ttf"), SUBTITLE_FONT).ok()?;
    Some(dir)
}

/// mbed TLS cannot read Android's platform trust store. Install the pinned
/// Mozilla roots in private cache and refresh by contents when Nova updates.
#[cfg(target_os = "android")]
fn install_android_ca_bundle() -> std::io::Result<std::path::PathBuf> {
    const ROOTS: &[u8] = include_bytes!(env!("NOVA_NATIVE_CA_BUNDLE"));
    let dir = nova_config::app_cache_dir().join("native-media");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("cacert.pem");
    if std::fs::read(&path).ok().as_deref() != Some(ROOTS) {
        std::fs::write(&path, ROOTS)?;
    }
    Ok(path)
}

/// Family name of a font file that ships on the device, used only when
/// [`install_subtitle_font`] can't write the bundled font. `sans-serif` is the
/// last resort: it is a fontconfig alias libass cannot resolve on Android, so
/// it is no worse than the default but unlikely to render.
#[cfg(target_os = "android")]
fn system_subtitle_font_family() -> &'static str {
    const CANDIDATES: &[(&str, &str)] = &[
        ("/system/fonts/Roboto-Regular.ttf", "Roboto"),
        ("/system/fonts/NotoSans-Regular.ttf", "Noto Sans"),
        ("/system/fonts/DroidSans.ttf", "Droid Sans"),
    ];
    CANDIDATES
        .iter()
        .find(|(path, _)| std::path::Path::new(path).exists())
        .map(|(_, family)| *family)
        .unwrap_or("sans-serif")
}

// ---------------------------------------------------------------------------
// mpv render API FFI. Declared here (like the reference prototype) so the
// OpenGL render context can be driven directly from the rendering notifier.
// The symbols come from libmpv, which `libmpv2` links against.
// ---------------------------------------------------------------------------

#[allow(non_camel_case_types)]
pub type mpv_handle = c_void;
#[allow(non_camel_case_types)]
pub type mpv_render_context = c_void;

#[repr(C)]
pub struct mpv_render_param {
    pub type_: c_int,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct mpv_opengl_init_params {
    pub get_proc_address:
        Option<unsafe extern "C" fn(ctx: *mut c_void, name: *const c_char) -> *mut c_void>,
    pub get_proc_address_ctx: *mut c_void,
    pub extra_exts: *const c_char,
}

#[repr(C)]
pub struct mpv_opengl_fbo {
    pub fbo: c_int,
    pub w: c_int,
    pub h: c_int,
    pub internal_format: c_int,
}

pub const MPV_RENDER_PARAM_API_TYPE: c_int = 1;
pub const MPV_RENDER_PARAM_OPENGL_INIT_PARAMS: c_int = 2;
pub const MPV_RENDER_PARAM_OPENGL_FBO: c_int = 3;
pub const MPV_RENDER_PARAM_FLIP_Y: c_int = 4;
pub const MPV_RENDER_PARAM_BLOCK_FOR_TARGET_TIME: c_int = 12;
pub const MPV_RENDER_PARAM_INVALID: c_int = 0;

unsafe extern "C" {
    pub fn mpv_render_context_create(
        res: *mut *mut mpv_render_context,
        mpv: *mut mpv_handle,
        params: *mut mpv_render_param,
    ) -> c_int;
    pub fn mpv_render_context_set_update_callback(
        ctx: *mut mpv_render_context,
        callback: Option<unsafe extern "C" fn(*mut c_void)>,
        callback_ctx: *mut c_void,
    );
    pub fn mpv_render_context_render(
        ctx: *mut mpv_render_context,
        params: *mut mpv_render_param,
    ) -> c_int;
    pub fn mpv_render_context_free(ctx: *mut mpv_render_context);
}

/// Owns an mpv render context bound to the app window's GL context.
struct MpvGlUnderlay {
    render_ctx: *mut mpv_render_context,
    app_weak_ptr: *mut c_void,
    /// GL entry points for the Android/Windows shared-context state guard.
    /// Android also uses the GLES/EGL library fallback for missing symbols.
    #[cfg(any(target_os = "android", target_os = "windows"))]
    gl: glow::Context,
    #[cfg(target_os = "android")]
    egl_context: usize,
}

// The raw pointers are only ever touched from the rendering notifier
// callbacks (and dropped on teardown).
unsafe impl Send for MpvGlUnderlay {}
unsafe impl Sync for MpvGlUnderlay {}

impl MpvGlUnderlay {
    /// Called only from RenderingTeardown, with the original GL context
    /// current. There is deliberately no Drop that invokes the render API:
    /// ordinary Rust destruction does not guarantee a current GL context.
    unsafe fn release_current(self) {
        unsafe {
            if !self.render_ctx.is_null() {
                mpv_render_context_set_update_callback(self.render_ctx, None, std::ptr::null_mut());
                mpv_render_context_free(self.render_ctx);
                if !self.app_weak_ptr.is_null() {
                    let _ = Box::from_raw(self.app_weak_ptr as *mut slint::Weak<AppWindow>);
                }
            }
        }
    }
}

fn bounded_seek(position: f64, delta: f64, duration: f64) -> f64 {
    let next = (position + delta).max(0.0);
    if duration > 0.0 {
        next.min(duration)
    } else {
        next
    }
}

// During a reload, mpv may expose no playhead or the previous file's state.
// The receiver's queued start owns the seek until that load begins reporting.
fn receiver_seek_state(
    live_position: Option<f64>,
    live_duration: Option<f64>,
    fallback: (f64, f64),
    queued_position: Option<f64>,
) -> (f64, f64) {
    (
        queued_position.or(live_position).unwrap_or(fallback.0),
        live_duration
            .filter(|duration| *duration > 0.0)
            .unwrap_or(fallback.1),
    )
}

#[cfg(test)]
mod relative_seek_tests {
    #[test]
    fn bounds_relative_seek_and_preserves_live_stream_seeking() {
        assert_eq!(super::bounded_seek(3.0, -10.0, 100.0), 0.0);
        assert_eq!(super::bounded_seek(95.0, 10.0, 100.0), 100.0);
        assert_eq!(super::bounded_seek(95.0, 10.0, 0.0), 105.0);
    }

    #[test]
    fn relative_seek_preserves_receiver_position_during_reload() {
        let last_observed = (300.0, 600.0);
        let missing = super::receiver_seek_state(None, None, last_observed, None);
        assert_eq!(super::bounded_seek(missing.0, 10.0, missing.1), 310.0);
        let loading = super::receiver_seek_state(Some(0.0), Some(0.0), last_observed, Some(320.0));
        assert_eq!(super::bounded_seek(loading.0, -10.0, loading.1), 310.0);
        let loaded = super::receiver_seek_state(Some(325.0), Some(601.0), last_observed, None);
        assert_eq!(super::bounded_seek(loaded.0, 10.0, loaded.1), 335.0);
    }
}

pub fn format_time(seconds: f64) -> String {
    let total_seconds = seconds.max(0.0) as u64;
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let secs = total_seconds % 60;

    if hours > 0 {
        format!("{:02}:{:02}:{:02}", hours, minutes, secs)
    } else {
        format!("{:02}:{:02}", minutes, secs)
    }
}

/// A visual estimate, not a promise that every packet in this span is cached.
/// Prefer mpv's end timestamp; duration is a fallback for older demuxers.
fn estimated_buffered_until(
    position: f64,
    duration: f64,
    cache_time: Option<f64>,
    cache_duration: Option<f64>,
) -> f32 {
    if !position.is_finite() || position < 0.0 || !duration.is_finite() || duration <= 0.0 {
        return 0.0;
    }
    let end = cache_time
        .filter(|end| end.is_finite() && *end >= 0.0)
        .or_else(|| {
            cache_duration
                .filter(|secs| secs.is_finite() && *secs >= 0.0)
                .map(|secs| position + secs)
        })
        .unwrap_or(0.0);
    if end <= position {
        return 0.0;
    }
    end.min(duration).min(f32::MAX as f64) as f32
}

/// A seek can briefly blank mpv's cache properties without discarding cached
/// packets. Bridge only that transition, never ordinary buffering or readiness.
#[derive(Clone, Copy)]
struct SeekBuffer {
    session: u64,
    start: f64,
    end: f64,
    expires: Instant,
}
impl SeekBuffer {
    fn begin(
        session: u64,
        now: Instant,
        target: f64,
        position: f64,
        duration: f64,
        known_end: f64,
        cache_state: Option<&str>,
    ) -> Option<Self> {
        if !target.is_finite() || target < 0.0 || !duration.is_finite() || duration <= 0.0 {
            return None;
        }
        let fallback = || {
            (position.is_finite()
                && position >= 0.0
                && known_end.is_finite()
                && target >= position
                && target < known_end)
                .then_some((position, known_end))
        };
        // Read only on a seek. The documented ranges account for backward
        // cache too; cache-time alone cannot establish a cached backward span.
        // https://mpv.io/manual/master/#command-interface-demuxer-cache-state
        let range = if let Some(raw) = cache_state {
            if raw.len() > 65536 {
                return None;
            }
            let value: serde_json::Value = serde_json::from_str(raw).ok()?;
            if let Some(ranges) = value
                .get("seekable-ranges")
                .and_then(serde_json::Value::as_array)
            {
                ranges
                    .iter()
                    .take(64)
                    .filter_map(|range| {
                        let start = range.get("start")?.as_f64()?;
                        let end = range.get("end")?.as_f64()?;
                        (start.is_finite()
                            && end.is_finite()
                            && start >= 0.0
                            && start <= target
                            && target < end)
                            .then_some((start, end))
                    })
                    .max_by(|a, b| a.1.total_cmp(&b.1))
            } else {
                fallback()
            }
        } else {
            fallback()
        }?;
        Some(Self {
            session,
            start: range.0,
            end: range.1.min(duration),
            expires: now + Duration::from_secs(2),
        })
    }
    fn visible(self, session: u64, now: Instant, position: f64, duration: f64) -> Option<f32> {
        (self.session == session
            && now < self.expires
            && position.is_finite()
            && position >= self.start
            && position < self.end
            && duration.is_finite()
            && duration > 0.0)
            .then_some(self.end.min(duration).min(f32::MAX as f64) as f32)
    }
}

/// Source-local playback telemetry. The cache span is an estimate in seconds,
/// not a guarantee that all tracks or a particular bitrate can be sustained.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PlaybackReadiness {
    pub buffering: bool,
    pub seekable: bool,
    pub buffered_ahead: f64,
    pub ended: bool,
}

fn readiness_buffered_ahead(
    position: f64,
    duration: f64,
    via_network: Option<bool>,
    cache_time: Option<f64>,
    cache_duration: Option<f64>,
) -> f64 {
    if !position.is_finite() || position < 0.0 {
        return 0.0;
    }
    let remaining =
        (duration.is_finite() && duration > 0.0).then_some((duration - position).max(0.0));
    // mpv commonly bypasses its packet cache for local files. An empty cache
    // there does not mean that a completed download is unready for playback.
    if via_network == Some(false) {
        return remaining.unwrap_or(0.0);
    }
    let ahead = cache_time
        .filter(|end| end.is_finite() && *end >= 0.0)
        .map(|end| (end - position).max(0.0))
        .or_else(|| cache_duration.filter(|secs| secs.is_finite() && *secs >= 0.0))
        .unwrap_or(0.0);
    remaining.map_or(ahead, |remaining| ahead.min(remaining))
}

fn normalized_party_rate(rate: Option<f64>) -> Option<f64> {
    rate.filter(|rate| rate.is_finite()).map(|rate| {
        // The selectable target stays within normal playback bounds; the
        // temporary clock correction also needs headroom at either end.
        rate.clamp(
            nova_config::PLAYBACK_SPEED_MIN as f64 * 0.97,
            nova_config::PLAYBACK_SPEED_MAX as f64 * 1.03,
        )
    })
}

fn apply_load_playback_options(
    mpv: &Mpv,
    paused: bool,
    party_rate: Option<f64>,
) -> libmpv2::Result<()> {
    // Keep the decoded source and final frame available so the timeline can
    // seek back after EOF. Party playback also needs explicit EOF to remain
    // observable until its coordinator reports and closes the source.
    mpv.set_property("keep-open", "yes")?;
    mpv.set_property("pause", paused || party_rate.is_some())?;
    if let Some(rate) = party_rate {
        mpv.set_property("speed", rate)?;
    }
    Ok(())
}

/// Effective presentation rate for the Android display frame-rate hint: the
/// container's nominal rate scaled by the playback rate (at 2× a 24 fps stream
/// presents 48 frames/s). `None` while the container rate is still unknown.
/// No clamping here — the rate arrives already in range, this just scales.
///
/// Android-only like its caller, but also compiled for host tests.
#[cfg(any(target_os = "android", test))]
fn effective_frame_rate(container_fps: Option<f32>, speed: f32) -> Option<f32> {
    container_fps.map(|fps| fps * speed)
}

/// Keep content-rate power savings only after all UI motion has settled.
#[cfg(any(target_os = "android", test))]
fn ui_needs_display_rate(
    osd_visible: bool,
    paused: bool,
    started: bool,
    osd_opacity: f32,
    animations: bool,
) -> bool {
    osd_visible || paused || !started || osd_opacity > 0.001 || animations
}

#[cfg(target_os = "android")]
fn clear_content_frame_rate_for_ui(state: &State) {
    let mut applied = state.applied_frame_rate.lock().unwrap();
    if *applied != Some(None) {
        set_content_frame_rate(None);
        *applied = Some(None);
        alog("frame rate: requested None (ui active)");
    }
}

/// Desktop video handoff is suspended until external progress tracking is
/// reworked. Keep the boundary closed even for automatic fallback callers;
/// Android still uses `external::open_external` and its `ACTION_VIEW` intent.
#[cfg(not(target_os = "android"))]
pub fn open_external(_url: &str) -> Result<(), String> {
    Err("External video playback is currently unavailable on desktop.".into())
}

/// Open `url` in the desktop's default browser, independently of video
/// handoff. Used for links such as an addon's configuration page.
#[cfg(all(not(target_os = "android"), target_os = "windows"))]
pub fn open_browser(url: &str) -> Result<(), String> {
    shell_open_url(url)
}

#[cfg(all(not(target_os = "android"), not(target_os = "windows")))]
pub fn open_browser(url: &str) -> Result<(), String> {
    std::process::Command::new("xdg-open")
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("could not launch xdg-open: {e}"))
}

#[cfg(target_os = "windows")]
fn shell_open_url(url: &str) -> Result<(), String> {
    use std::ffi::c_void;

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn ShellExecuteW(
            hwnd: *mut c_void,
            operation: *const u16,
            file: *const u16,
            parameters: *const u16,
            directory: *const u16,
            show_command: i32,
        ) -> *mut c_void;
    }

    let operation = [b'o' as u16, b'p' as u16, b'e' as u16, b'n' as u16, 0];
    let file: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
        )
    };
    let code = result as isize;
    if code > 32 {
        Ok(())
    } else {
        Err(format!(
            "could not open URL with the Windows shell (code {code})"
        ))
    }
}

/// Android GLES/EGL function lookup, used as the fallback in the mpv
/// `get_proc_address` callback below.
///
/// The callback Slint hands us on Android is glutin's EGL display — a bare
/// `eglGetProcAddress` (`glutin/src/api/egl/display.rs`) with no `dlsym`
/// fallback. EGL only guarantees *extension* entry points from that call, while
/// mpv resolves **everything** — core `gl*`/`egl*` included — through the
/// callback (`render_gl.h`: "some APIs do not always return pointers for all
/// standard functions (even if present); in this case you have to compensate by
/// looking up these functions yourself"). Direct MediaCodec interop needs
/// exactly the entry points that are least reliable that way
/// (`eglCreateImageKHR`, `eglGetCurrentDisplay`, `glEGLImageTargetTexture2DOES`),
/// so fall back to the GLES/EGL libraries themselves.
#[cfg(target_os = "android")]
mod android_gl {
    use std::ffi::{CStr, c_char, c_int, c_void};
    use std::sync::OnceLock;

    // Same linkage as libloading on Android; bionic has carried the dl* family
    // in libc since API 21 (min SDK here is 26) with libdl.so as a stub.
    #[link(name = "dl")]
    unsafe extern "C" {
        fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }

    // bionic's RTLD_NOW.
    const RTLD_NOW: c_int = 2;

    /// Handles of the GLES/EGL libraries the app already renders through,
    /// resolved on first miss (`dlopen` on a live library just bumps a
    /// refcount). Stored as `usize` so the `OnceLock` stays `Sync`.
    fn handles() -> &'static [usize] {
        static HANDLES: OnceLock<Vec<usize>> = OnceLock::new();
        HANDLES.get_or_init(|| {
            [c"libGLESv2.so", c"libEGL.so"]
                .into_iter()
                .map(|name| unsafe { dlopen(name.as_ptr(), RTLD_NOW) } as usize)
                .filter(|&handle| handle != 0)
                .collect()
        })
    }

    pub(super) fn current_context() -> usize {
        let symbol = lookup(c"eglGetCurrentContext");
        if symbol.is_null() {
            return 0;
        }
        // EGL's exported function has this signature on Android.
        let get: unsafe extern "C" fn() -> *mut c_void = unsafe { std::mem::transmute(symbol) };
        unsafe { get() as usize }
    }

    /// `name` resolved straight out of the GLES/EGL libraries (null when
    /// absent).
    pub(super) fn lookup(name: &CStr) -> *mut c_void {
        for &handle in handles() {
            let symbol = unsafe { dlsym(handle as *mut c_void, name.as_ptr()) };
            if !symbol.is_null() {
                return symbol;
            }
        }
        std::ptr::null_mut()
    }
}

// Android restores Skia state; Windows also supplies clean incoming state.
#[cfg(any(
    target_os = "android",
    target_os = "windows",
    all(test, target_os = "linux")
))]
mod gl_state;

/// Trampoline that turns mpv's C `get_proc_address` callback into a call of
/// the Rust closure Slint handed us for its OpenGL context.
unsafe extern "C" fn get_proc_address_trampoline(
    ctx: *mut c_void,
    name: *const c_char,
) -> *mut c_void {
    if name.is_null() {
        return std::ptr::null_mut();
    }
    let cstr = unsafe { CStr::from_ptr(name) };
    if !ctx.is_null() {
        let fn_ref_ptr = ctx as *const &dyn Fn(&CStr) -> *const c_void;
        let get_proc_addr: &dyn Fn(&CStr) -> *const c_void = unsafe { *fn_ref_ptr };
        let symbol = get_proc_addr(cstr);
        if !symbol.is_null() {
            return symbol as *mut c_void;
        }
    }
    #[cfg(target_os = "android")]
    let fallback = android_gl::lookup(cstr);
    #[cfg(not(target_os = "android"))]
    let fallback = std::ptr::null_mut();
    fallback
}

/// mpv pushed a new video frame; ask Slint to repaint so the underlay shows.
unsafe extern "C" fn on_mpv_update(ctx: *mut c_void) {
    if ctx.is_null() {
        return;
    }
    let app_weak = unsafe { &*(ctx as *const slint::Weak<AppWindow>) };
    let _ = app_weak.upgrade_in_event_loop(|app| {
        app.window().request_redraw();
    });
}

/// Shared state behind [`Player`]: the mpv core, its render context and the
/// per-playback flags. All access happens on the UI/event-loop thread except
/// the render context, which the rendering notifier touches.
fn apply_track_options(mpv: &Mpv, prefs: &SessionPreferences, audio: bool) {
    let (saved, global) = if audio {
        (prefs.title.audio.as_ref(), prefs.audio_language.as_str())
    } else {
        (
            prefs.title.subtitles.as_ref(),
            prefs.subtitle_language.as_str(),
        )
    };
    let language = match saved {
        Some(TrackChoice::Track(track)) => track.language.as_deref().unwrap_or(global),
        _ => global,
    };
    let _ = mpv.set_property(
        if audio { "alang" } else { "slang" },
        if language == "off" { "" } else { language },
    );
    let off = matches!(saved, Some(TrackChoice::Off)) || (saved.is_none() && global == "off");
    let _ = mpv.set_property(
        if audio { "aid" } else { "sid" },
        if off { "no" } else { "auto" },
    );
    if !audio {
        let _ = mpv.set_property(
            "subs-fallback",
            if language.is_empty() { "default" } else { "no" },
        );
    }
}

fn apply_preference_options(
    mpv: &Mpv,
    prefs: &SessionPreferences,
    native: &NativeSubtitleDefaults,
) {
    apply_track_options(mpv, prefs, true);
    apply_track_options(mpv, prefs, false);
    let _ = mpv.set_property("speed", prefs.speed() as f64);
    apply_subtitle_style(mpv, prefs.subtitle_style(), native);
}

/// Capture once before applying any saved/title appearance. Disabling the
/// backdrop restores native shadow settings rather than leaving box padding
/// behind as a large subtitle shadow.
struct NativeSubtitleDefaults {
    style: nova_config::playback_preferences::SubtitleStyle,
    border_style: String,
    back_color: String,
    shadow_offset: f64,
}
impl Default for NativeSubtitleDefaults {
    fn default() -> Self {
        Self {
            style: nova_config::playback_preferences::SubtitleStyle::default()
                .with_value(1, 55.0)
                .with_value(4, 3.0),
            border_style: "outline-and-shadow".into(),
            back_color: "#AF000000".into(),
            shadow_offset: 0.0,
        }
    }
}
fn native_subtitle_defaults(mpv: &Mpv) -> NativeSubtitleDefaults {
    NativeSubtitleDefaults {
        style: nova_config::playback_preferences::SubtitleStyle {
            font_size: Some(mpv.get_property::<f64>("sub-font-size").unwrap_or(55.0) as f32),
            outline: Some(mpv.get_property::<f64>("sub-border-size").unwrap_or(3.0) as f32),
            ..Default::default()
        },
        border_style: mpv
            .get_property::<String>("sub-border-style")
            .unwrap_or_else(|_| "outline-and-shadow".into()),
        back_color: mpv
            .get_property::<String>("sub-back-color")
            .unwrap_or_else(|_| "#AF000000".into()),
        shadow_offset: mpv.get_property::<f64>("sub-shadow-offset").unwrap_or(0.0),
    }
}

fn apply_subtitle_style(
    mpv: &Mpv,
    style: nova_config::playback_preferences::SubtitleStyle,
    native: &NativeSubtitleDefaults,
) {
    let style = style.resolved(native.style);
    let _ = mpv.set_property("sub-delay", style.delay as f64);
    let _ = mpv.set_property(
        "sub-font-size",
        style
            .font_size
            .unwrap_or(nova_config::playback_preferences::SubtitleStyle::DEFAULT_FONT_SIZE)
            as f64,
    );
    let _ = mpv.set_property("sub-margin-x", style.margin as i64);
    let _ = mpv.set_property("sub-pos", style.position as f64);
    let _ = mpv.set_property("sub-border-size", style.outline.unwrap_or(3.0) as f64);
    let _ = mpv.set_property(
        "sub-color",
        match style.color {
            1 => "#FFFF00",
            2 => "#00FFFF",
            _ => "#FFFFFF",
        },
    );
    // mpv/libass applies this shared text style to both subtitle slots.
    // BorderStyle=4 preserves the normal glyph outline and puts one padded
    // translucent rectangle behind the cue. No glyph blur, authored ASS
    // rewriting, UI overlay or render-tick work is involved.
    // https://mpv.io/manual/stable/#options-sub-border-style
    if style.background_opacity > 0.0 {
        let alpha = (style.background_opacity * 255.0 / 100.0).round() as u8;
        let _ = mpv.set_property("sub-border-style", "background-box");
        let _ = mpv.set_property("sub-back-color", format!("#{alpha:02X}000000").as_str());
        let _ = mpv.set_property("sub-shadow-offset", 6.0_f64);
    } else {
        let _ = mpv.set_property("sub-border-style", native.border_style.as_str());
        let _ = mpv.set_property("sub-back-color", native.back_color.as_str());
        let _ = mpv.set_property("sub-shadow-offset", native.shadow_offset);
    }
    let _ = mpv.set_property(
        "sub-ass-override",
        if style.override_ass { "force" } else { "yes" },
    );
}

struct PreferenceSession {
    preferences: SessionPreferences,
    tracks: String,
    manual_audio: bool,
    manual_subtitles: bool,
}

/// Torrent piece RAM and mpv's packet queues have separate budgets. Preserve
/// the established demuxer limits: a tiny queue can mark an interleaved track
/// EOF before its first packet arrives. RAM mode only disables disk caching.
#[derive(Clone, Copy)]
struct PacketCacheDefaults {
    disk: bool,
}
fn apply_torrent_packet_cache(
    mpv: &Mpv,
    ram: bool,
    saved: &mut Option<PacketCacheDefaults>,
) -> libmpv2::Result<()> {
    if ram {
        if saved.is_none() {
            *saved = Some(PacketCacheDefaults {
                disk: mpv.get_property("cache-on-disk")?,
            });
        }
        mpv.set_property("cache-on-disk", false)?;
    } else if let Some(defaults) = *saved {
        mpv.set_property("cache-on-disk", defaults.disk)?;
        *saved = None;
    }
    Ok(())
}

/// A received-chunk counter keeps slow first pieces alive without extending
/// startup for unchanged/decreasing counters or a swarm which stopped sending.
#[derive(Default)]
struct RamStartupProgress {
    received: u64,
    advanced_at: Option<Instant>,
}
impl RamStartupProgress {
    fn observe(&mut self, received: u64, now: Instant) {
        if received > self.received {
            self.received = received;
            self.advanced_at = Some(now);
        }
    }
    fn recent(&self, now: Instant) -> bool {
        self.advanced_at
            .is_some_and(|at| now.saturating_duration_since(at) < Duration::from_secs(30))
    }
}

struct State {
    ram_torrent_source: Mutex<Option<String>>,
    packet_cache_defaults: Mutex<Option<PacketCacheDefaults>>,
    ram_buffer_progress: Mutex<RamStartupProgress>,
    sleep_deadline: Mutex<sleep_timer::Deadline>,
    #[cfg(not(target_os = "android"))]
    playback_fullscreen: AtomicBool,
    #[cfg(not(target_os = "android"))]
    catalog_maximized: AtomicBool,
    #[cfg(not(target_os = "android"))]
    preserve_close_window: AtomicBool,
    #[cfg(not(target_os = "android"))]
    transition_window_held: AtomicBool,
    subtitle_position: Mutex<subtitle_position::SubtitlePosition>,
    native_subtitle_defaults: NativeSubtitleDefaults,
    preferences: Mutex<PreferenceSession>,
    secondary_subtitles: Mutex<TrackChoice>,
    secondary_subtitle_position: Mutex<secondary_subtitle_position::Placement>,
    /// Temporary clock correction; never written to title/device preferences.
    party_playback_rate: Mutex<Option<f64>>,
    /// Retain real cache starvation through a transient unavailable property.
    buffering: AtomicBool,
    preference_source: Mutex<String>,
    audio_processing: Mutex<audio_processing::AudioProcessing>,
    mpv: Mutex<Option<Mpv>>,
    /// Invalidates queued captures when a stream is replaced or closed.
    screenshot_session: AtomicU64,
    #[cfg(target_os = "android")]
    gpu_screenshot: Mutex<Option<gpu_screenshot::Request>>,
    /// Set when mpv could not be initialized / the renderer is not OpenGL.
    mpv_error: Mutex<Option<String>>,
    underlay: Mutex<Option<MpvGlUnderlay>>,
    /// Only the ignored native diagnostic may reproduce the old blocking path.
    #[cfg(test)]
    timing_baseline: AtomicBool,
    #[cfg(test)]
    timing_subtitle_us: AtomicU64,
    #[cfg(test)]
    timing_mpv_us: AtomicU64,
    /// URL queued for loadfile; consumed on the next `BeforeRendering` (mpv
    /// must have its render context attached before the file is loaded, or
    /// early frames would be dropped).
    pending: Mutex<Option<String>>,
    /// Request headers paired with the queued URL. Consumed with `pending`.
    pending_headers: Mutex<Vec<(String, String)>>,
    /// External subtitle files to load with the next stream.
    pending_subtitles: Mutex<Vec<String>>,
    source_subtitles: Mutex<source_subtitles::SourceSubtitles>,
    online_subtitles: Mutex<Vec<OnlineSubtitle>>,
    subtitle_sync: Mutex<subtitle_sync::SubtitleSync>,
    #[cfg(target_os = "android")]
    loaded_online_subtitles: Mutex<addon_subtitles::LoadedSubtitles>,
    /// Playback of the current session actually produced frames.
    started: AtomicBool,
    /// The overlay is waiting for an addon URL (or showing its error), not mpv.
    source_waiting: AtomicBool,
    /// A successful external handoff closes the overlay but keeps the source alive.
    external_handoff: AtomicBool,
    /// Sensitive resolved URLs must never leave through OS arguments/intents.
    allow_external: AtomicBool,
    /// A source/timeline confirmation must precede every unpause or seek.
    start_pause_hold: AtomicBool,
    /// The OSD thumb (or a programmatic seek) moved ahead of mpv and a seek is
    /// in flight. The `tick` scrub guard only engages while this is set, so an
    /// initial resume (opened at the saved position via the load-time `start`
    /// option, with the UI still at 0) mirrors mpv instead of being mistaken
    /// for a scrub and frozen.
    seek_pending: AtomicBool,
    /// Count accepted seeks so even tiny/paused seeks notify desktop media widgets.
    #[cfg(target_os = "linux")]
    seek_serial: AtomicU64,
    /// A native setter can succeed without ever moving the demuxer. Bound the
    /// optimistic slider hold, including for paused and unseekable sources.
    seek_deadline: Mutex<Option<Instant>>,
    seek_buffer: Mutex<Option<SeekBuffer>>,
    /// The load timeout / setup error was already reported for this session.
    reported: AtomicBool,
    opened_at: Mutex<Instant>,
    app: slint::Weak<AppWindow>,
    /// Android: URL of the current session, kept so a decoder that never
    /// produced frames can be reloaded with the next decoder in the chain.
    #[cfg(target_os = "android")]
    last_url: Mutex<Option<String>>,
    /// Android decoder and surface reloads need the same source headers.
    #[cfg(target_os = "android")]
    last_headers: Mutex<Vec<(String, String)>>,
    #[cfg(target_os = "android")]
    last_subtitles: Mutex<Vec<String>>,
    /// Android: start position to apply to the next queued `loadfile`. mpv's
    /// global `start` option is only honored for the first load, so a runtime
    /// decoder switch passes the resume point through the loadfile's own
    /// options instead.
    #[cfg(target_os = "android")]
    pending_start: Mutex<Option<f64>>,
    /// Android: session-scoped renderer recovery. Teardown arms it; setup
    /// and activity resume jointly make it eligible for one reload.
    #[cfg(target_os = "android")]
    recovery: Mutex<android_recovery::Recovery>,
    /// Pause state carried across the queued load/render boundary on every platform.
    pending_paused: AtomicBool,
    /// Android: the active hardware decoder was already checked this session.
    #[cfg(target_os = "android")]
    hwdec_checked: AtomicBool,
    /// Android: direct MediaCodec interop is usable on this device (the
    /// JavaVM registration needed by `hwdec_aimagereader` succeeded).
    #[cfg(target_os = "android")]
    direct_available: AtomicBool,
    /// Android: direct MediaCodec was refused or never engaged at least once,
    /// so later sessions skip the 20 s timeout and start at copy-back. Cleared
    /// when the user changes the decoder setting.
    #[cfg(target_os = "android")]
    direct_failed: AtomicBool,
    /// Android: decoder candidates for the current session, in fallback order
    /// (e.g. `["mediacodec", "mediacodec-copy", "no"]`).
    #[cfg(target_os = "android")]
    decoder_chain: Mutex<Vec<&'static str>>,
    /// Android: index into [`State::decoder_chain`] currently requested.
    #[cfg(target_os = "android")]
    decoder_idx: AtomicUsize,
    /// Android: last user-selected decoder preference. A change resets the
    /// sticky `direct_failed` so HW+ can be retried after the user picks it.
    #[cfg(target_os = "android")]
    last_hwdec_pref: Mutex<Option<nova_config::AndroidHwdec>>,
    /// Android: per-stream decoder override set from the player overlay's
    /// runtime picker. `None` means use the persisted Settings → Player
    /// preference; a new `play` (or `close`) clears it so the override never
    /// leaks into the next stream.
    #[cfg(target_os = "android")]
    session_hwdec: Mutex<Option<nova_config::AndroidHwdec>>,
    /// Android: last `hwdec-current` survey value logged for the session, so
    /// the per-tick codec survey only prints when the decoder actually changes
    /// (initialized to a sentinel to force one line per session).
    #[cfg(target_os = "android")]
    last_hwdec_log: Mutex<String>,
    /// Android: container frame rate of the current stream (mpv `container-fps`),
    /// or `None` until known. Drives the display refresh-rate request when the
    /// OSD is fully hidden.
    #[cfg(target_os = "android")]
    content_fps: Mutex<Option<f32>>,
    /// Android: last frame rate requested from the window (`None` = the display
    /// default). Outer `Option` is `None` until the first apply.
    #[cfg(target_os = "android")]
    applied_frame_rate: Mutex<Option<Option<f32>>>,
    /// Android: set when the native window/surface is recreated, so the
    /// per-surface frame-rate request is re-applied on the next tick.
    #[cfg(target_os = "android")]
    frame_rate_reapply: AtomicBool,
}

impl State {
    /// Read only the current accepted source. An external track receives
    /// declared facts only through an exact URL + playback-session match.
    fn track_inventory(&self, mpv: &Mpv) -> libmpv2::Result<String> {
        if self.pending.lock().unwrap().is_some() {
            return Err(libmpv2::Error::Raw(libmpv2::mpv_error::PropertyUnavailable));
        }
        let source = mpv.get_property::<String>("path")?;
        if source != *self.preference_source.lock().unwrap() {
            return Err(libmpv2::Error::Raw(libmpv2::mpv_error::PropertyUnavailable));
        }
        let raw = mpv.get_property::<String>("track-list")?;
        Ok(self.source_subtitles.lock().unwrap().enrich(
            &source,
            self.screenshot_session.load(Ordering::SeqCst),
            raw,
        ))
    }

    /// Used by the rendering callback immediately before submitting loadfile.
    fn take_load_pause(&self) -> bool {
        self.pending_paused.swap(false, Ordering::SeqCst)
            || self.start_pause_hold.load(Ordering::SeqCst)
    }

    fn apply_subtitle_style(
        &self,
        mpv: &Mpv,
        style: nova_config::playback_preferences::SubtitleStyle,
    ) {
        // Every live edit replaces the base used by the temporary OSD lift.
        self.subtitle_position
            .lock()
            .unwrap()
            .set_base(mpv, style.position as f64);
        apply_subtitle_style(mpv, style, &self.native_subtitle_defaults);
        let delay = self
            .subtitle_sync
            .lock()
            .unwrap()
            .delay(mpv, style.delay as f64);
        let _ = mpv.set_property("sub-delay", delay);
    }
}

impl Drop for State {
    fn drop(&mut self) {
        if self.underlay.get_mut().unwrap().is_some() {
            // A backend that misses teardown cannot safely free GL resources
            // here. Keep the core and callback allocation alive as well; mpv
            // requires its render context to be freed before core destruction.
            // Normal shutdown and Android window termination use the notifier.
            #[cfg(target_os = "android")]
            alog("player dropped without render teardown; retaining native mpv resources");
            if let Some(mpv) = self.mpv.get_mut().unwrap().take() {
                std::mem::forget(mpv);
            }
        }
    }
}

/// FFmpeg's JavaVM registration (`libavcodec/jni.h` in FFmpeg 9).
///
/// mpv's **direct** MediaCodec hwdec (`hwdec_aimagereader`) resolves a `JNIEnv`
/// through `mp_jni_get_env()`, which takes the VM from
/// `av_jni_get_java_vm(NULL)` — something the embedding application has to
/// register. Register before creating mpv: older mpv versions asserted on a
/// missing VM, and current versions cannot use direct MediaCodec without it.
/// FFmpeg is now shared separately, so link the public JNI symbol from avcodec.
/// Returns whether registration succeeded; keep direct MediaCodec off otherwise.
#[cfg(target_os = "android")]
fn register_java_vm() -> bool {
    #[link(name = "avcodec")]
    unsafe extern "C" {
        /// `int av_jni_set_java_vm(void *vm, void *log_ctx)`.
        fn av_jni_set_java_vm(vm: *mut c_void, log_ctx: *mut c_void) -> c_int;
    }

    let vm = crate::external::java_vm_ptr();
    if vm.is_null() {
        alog("no JavaVM stashed (android_main has not run); direct mediacodec disabled");
        return false;
    }
    // Safety: `vm` is the process-wide JavaVM pointer stashed from
    // `android_main`; FFmpeg only stores it here.
    let rc = unsafe { av_jni_set_java_vm(vm, std::ptr::null_mut()) };
    if rc != 0 {
        alog(&format!(
            "av_jni_set_java_vm failed ({rc}); direct mediacodec disabled"
        ));
        return false;
    }
    alog("JavaVM registered with FFmpeg (direct mediacodec possible)");
    true
}

/// Per-load behavior; a paused load stays held until the app confirms its timeline.
#[derive(Clone, Copy, Debug)]
pub struct PlaybackLoadPolicy {
    pub allow_external: bool,
    pub paused: bool,
}

/// The in-app player. Cheap to clone (shares [`Arc`]); clone into the UI
/// callbacks, the 250 ms state timer and the `Bridge`.
#[derive(Clone)]
pub struct Player {
    state: Arc<State>,
}

impl Player {
    /// Speculative work must not start while Android playback is backgrounded.
    pub fn can_prefetch_streams(&self) -> bool {
        #[cfg(target_os = "android")]
        {
            !self.state.recovery.lock().unwrap().waiting()
        }
        #[cfg(not(target_os = "android"))]
        {
            true
        }
    }

    /// Create the mpv core and install the rendering notifier on `app`'s
    /// window (the underlay render context is created at the first
    /// `RenderingSetup`, when the GL context is current).
    pub fn setup(app: &AppWindow) -> Self {
        Self::setup_inner(app, None)
    }

    /// Observe rendering without registering a second notifier (Slint only
    /// permits one). The observer must stay cheap and preserve graphics state.
    /// `AfterRendering` is before presentation, not proof of screen visibility.
    pub fn setup_with_render_observer(
        app: &AppWindow,
        observer: impl FnMut(RenderingState) + 'static,
    ) -> Self {
        Self::setup_inner(app, Some(Box::new(observer)))
    }

    fn setup_inner(app: &AppWindow, mut observer: Option<Box<dyn FnMut(RenderingState)>>) -> Self {
        // Android: register the JavaVM with libavutil before mpv can touch a
        // decoder — direct MediaCodec aborts without it (see
        // [`register_java_vm`]), so which decoder to request is decided here.
        #[cfg(target_os = "android")]
        let direct_hwdec = register_java_vm();

        let mpv = Mpv::new();
        let mpv_error = match &mpv {
            Ok(mpv) => {
                let _ = mpv.set_property("terminal", "no");
                let _ = mpv.set_property("msg-level", "all=no");
                let _ = mpv.set_property("vo", "libmpv");
                let _ = configure_render_timing(mpv);
                // Android: audio out through OpenSL ES (the vendored libmpv
                // links it) and no config/script discovery — the app owns mpv's
                // configuration, and there is no user-editable mpv.conf to
                // honour inside the app's private directory.
                #[cfg(target_os = "android")]
                {
                    let _ = mpv.set_property("ao", "opensles");
                    let _ = mpv.set_property("config", "no");
                    let _ = mpv.set_property("load-scripts", "no");
                    let _ = mpv.set_property("tls-verify", true);
                    match install_android_ca_bundle() {
                        Ok(path) => {
                            let _ =
                                mpv.set_property("tls-ca-file", path.to_string_lossy().as_ref());
                        }
                        Err(error) => alog(&format!("could not install TLS roots: {error}")),
                    }
                    // libass here has no fontconfig, so give it a default font:
                    // text subtitles without an embedded font render blank
                    // otherwise. See [`SUBTITLE_FONT`].
                    match install_subtitle_font() {
                        Some(dir) => {
                            let _ =
                                mpv.set_property("sub-fonts-dir", dir.to_string_lossy().as_ref());
                            let _ = mpv.set_property("sub-font", SUBTITLE_FONT_FAMILY);
                            alog("subtitle font: bundled Roboto installed as default");
                        }
                        None => {
                            let _ = mpv.set_property("sub-fonts-dir", "/system/fonts");
                            let _ = mpv.set_property("sub-font", system_subtitle_font_family());
                            alog("subtitle font: bundle unwritable; using /system/fonts");
                        }
                    }
                    // PGS/bitmap subtitle decoding is explicitly enabled and
                    // checked by the native build recipe; see SOURCES.
                }
                #[cfg(not(target_os = "android"))]
                let _ = configure_desktop_decoder(mpv);
                None
            }
            Err(e) => Some(format!("failed to initialize mpv: {e}")),
        };

        // Capture before any per-title/global style can change these options.
        // Renderer recovery reuses this core, so its baseline stays immutable.
        let native_subtitle_defaults = mpv
            .as_ref()
            .ok()
            .map(native_subtitle_defaults)
            .unwrap_or_default();
        let state = Arc::new(State {
            ram_torrent_source: Mutex::new(None),
            packet_cache_defaults: Mutex::new(None),
            ram_buffer_progress: Mutex::new(Default::default()),
            sleep_deadline: Mutex::new(Default::default()),
            native_subtitle_defaults,
            subtitle_position: Mutex::new(subtitle_position::SubtitlePosition::default()),
            preference_source: Mutex::new(String::new()),
            preferences: Mutex::new(PreferenceSession {
                preferences: SessionPreferences::from_settings(
                    &nova_config::active_cache_settings(),
                    TitlePreferences::default(),
                ),
                tracks: String::new(),
                manual_audio: false,
                manual_subtitles: false,
            }),
            secondary_subtitles: Mutex::new(TrackChoice::Off),
            secondary_subtitle_position: Mutex::new(Default::default()),
            party_playback_rate: Mutex::new(None),
            buffering: AtomicBool::new(false),
            audio_processing: Mutex::new(Default::default()),
            mpv: Mutex::new(mpv.ok()),
            screenshot_session: AtomicU64::new(0),
            #[cfg(target_os = "android")]
            gpu_screenshot: Mutex::new(None),
            mpv_error: Mutex::new(mpv_error),
            underlay: Mutex::new(None),
            #[cfg(test)]
            timing_baseline: AtomicBool::new(false),
            #[cfg(test)]
            timing_subtitle_us: AtomicU64::new(0),
            #[cfg(test)]
            timing_mpv_us: AtomicU64::new(0),
            pending: Mutex::new(None),
            pending_headers: Mutex::new(Vec::new()),
            pending_subtitles: Mutex::new(Vec::new()),
            source_subtitles: Mutex::new(Default::default()),
            online_subtitles: Mutex::new(Vec::new()),
            subtitle_sync: Mutex::new(subtitle_sync::SubtitleSync::default()),
            #[cfg(target_os = "android")]
            loaded_online_subtitles: Mutex::new(addon_subtitles::LoadedSubtitles::default()),
            started: AtomicBool::new(false),
            source_waiting: AtomicBool::new(false),
            #[cfg(not(target_os = "android"))]
            playback_fullscreen: AtomicBool::new(false),
            #[cfg(not(target_os = "android"))]
            catalog_maximized: AtomicBool::new(false),
            #[cfg(not(target_os = "android"))]
            preserve_close_window: AtomicBool::new(false),
            #[cfg(not(target_os = "android"))]
            transition_window_held: AtomicBool::new(false),
            external_handoff: AtomicBool::new(false),
            allow_external: AtomicBool::new(true),
            start_pause_hold: AtomicBool::new(false),
            seek_pending: AtomicBool::new(false),
            #[cfg(target_os = "linux")]
            seek_serial: AtomicU64::new(0),
            seek_deadline: Mutex::new(None),
            seek_buffer: Mutex::new(None),
            reported: AtomicBool::new(false),
            opened_at: Mutex::new(Instant::now()),
            app: app.as_weak(),
            #[cfg(target_os = "android")]
            last_url: Mutex::new(None),
            #[cfg(target_os = "android")]
            last_headers: Mutex::new(Vec::new()),
            #[cfg(target_os = "android")]
            last_subtitles: Mutex::new(Vec::new()),
            #[cfg(target_os = "android")]
            pending_start: Mutex::new(None),
            #[cfg(target_os = "android")]
            recovery: Mutex::new(android_recovery::Recovery::new(
                ANDROID_FOREGROUND.load(Ordering::SeqCst),
            )),
            pending_paused: AtomicBool::new(false),
            #[cfg(target_os = "android")]
            hwdec_checked: AtomicBool::new(false),
            #[cfg(target_os = "android")]
            direct_available: AtomicBool::new(direct_hwdec),
            #[cfg(target_os = "android")]
            direct_failed: AtomicBool::new(false),
            #[cfg(target_os = "android")]
            decoder_chain: Mutex::new(Vec::new()),
            #[cfg(target_os = "android")]
            decoder_idx: AtomicUsize::new(0),
            #[cfg(target_os = "android")]
            last_hwdec_pref: Mutex::new(None),
            #[cfg(target_os = "android")]
            session_hwdec: Mutex::new(None),
            #[cfg(target_os = "android")]
            last_hwdec_log: Mutex::new(String::from("<unset>")),
            #[cfg(target_os = "android")]
            content_fps: Mutex::new(None),
            #[cfg(target_os = "android")]
            applied_frame_rate: Mutex::new(None),
            #[cfg(target_os = "android")]
            frame_rate_reapply: AtomicBool::new(false),
        });

        #[cfg(target_os = "android")]
        {
            *ANDROID_PLAYER.lock().unwrap() = Some(Arc::downgrade(&state));
        }
        sleep_timer::start_watchdog(&state);
        let notifier_state = Arc::clone(&state);
        if let Err(e) =
            app.window()
                .set_rendering_notifier(move |state, graphics_api| {
                    if let Some(observer) = observer.as_mut() {
                        observer(state.clone());
                    }
                    match state {
                    RenderingState::RenderingSetup => {
                        let mpv = notifier_state.mpv.lock().unwrap();
                        let Some(mpv) = mpv.as_ref() else {
                            return; // mpv init failed; play() reports the error
                        };
                        if notifier_state.underlay.lock().unwrap().is_some() {
                            // Never free or reuse an old mpv context under a
                            // replacement GL context. The local Android backend
                            // patch must deliver teardown before destroying it.
                            #[cfg(target_os = "android")]
                            {
                                let msg = "render setup refused: previous mpv context missed teardown";
                                alog(msg);
                                *notifier_state.mpv_error.lock().unwrap() = Some(msg.into());
                            }
                            return;
                        }

                        let GraphicsAPI::NativeOpenGL { get_proc_address } = graphics_api else {
                            let msg = "cannot play in-app: the mpv player needs an OpenGL renderer";
                            *notifier_state.mpv_error.lock().unwrap() = Some(msg.to_string());
                            #[cfg(target_os = "android")]
                            alog(msg);
                            return;
                        };

                        #[cfg(target_os = "android")]
                        let egl_context = android_gl::current_context();
                        #[cfg(target_os = "android")]
                        {
                            if egl_context == 0 {
                                let msg = "render setup failed: no current EGL context";
                                alog(msg);
                                *notifier_state.mpv_error.lock().unwrap() = Some(msg.into());
                                return;
                            }
                            alog(&format!("render setup begin: egl={egl_context:#x}"));
                        }
                        unsafe {
                            let mut render_ctx = std::ptr::null_mut();
                            let get_proc_fn: &dyn Fn(&CStr) -> *const c_void = *get_proc_address;
                            let get_proc_address_ctx = &get_proc_fn as *const _ as *mut c_void;

                            let mut gl_init = mpv_opengl_init_params {
                                get_proc_address: Some(get_proc_address_trampoline),
                                get_proc_address_ctx,
                                extra_exts: std::ptr::null(),
                            };

                            let api_type = b"opengl\0";
                            let mut params = [
                                mpv_render_param {
                                    type_: MPV_RENDER_PARAM_API_TYPE,
                                    data: api_type.as_ptr() as *mut c_void,
                                },
                                mpv_render_param {
                                    type_: MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
                                    data: &mut gl_init as *mut _ as *mut c_void,
                                },
                                mpv_render_param {
                                    type_: MPV_RENDER_PARAM_INVALID,
                                    data: std::ptr::null_mut(),
                                },
                            ];

                            let raw_ctx = mpv.ctx.as_ptr() as *mut mpv_handle;
                            #[cfg(target_os = "windows")]
                            let gl = glow::Context::from_loader_function_cstr(get_proc_fn);
                            let status = {
                                // Creation also changes GL state, including on
                                // failure. Restore it before Slint draws again.
                                #[cfg(target_os = "windows")]
                                let _guard = gl_state::Guard::new(&gl);
                                mpv_render_context_create(
                                    &mut render_ctx,
                                    raw_ctx,
                                    params.as_mut_ptr(),
                                )
                            };
                            if status < 0 {
                                let msg = "cannot play in-app: mpv render context creation failed";
                                *notifier_state.mpv_error.lock().unwrap() = Some(msg.to_string());
                                #[cfg(target_os = "android")]
                                alog(msg);
                                return;
                            }

                            let app_weak_box = Box::new(notifier_state.app.clone());
                            let app_weak_ptr = Box::into_raw(app_weak_box) as *mut c_void;
                            mpv_render_context_set_update_callback(
                                render_ctx,
                                Some(on_mpv_update),
                                app_weak_ptr,
                            );
                            #[cfg(target_os = "android")]
                            alog("mpv render context created (OpenGL/GLES underlay ready)");
                            // Android: resolve the GL entry points once for the
                            // state guard. The notifier's `get_proc_address`
                            // misses extension entry points on some drivers, so
                            // fall back to the GLES/EGL libraries.
                            #[cfg(target_os = "android")]
                            let gl = {
                                let get_proc: &dyn Fn(&CStr) -> *const c_void = *get_proc_address;
                                glow::Context::from_loader_function_cstr(|name| {
                                    let symbol = get_proc(name);
                                    if !symbol.is_null() {
                                        return symbol;
                                    }
                                    android_gl::lookup(name)
                                })
                            };
                            *notifier_state.underlay.lock().unwrap() = Some(MpvGlUnderlay {
                                render_ctx,
                                app_weak_ptr,
                                #[cfg(any(target_os = "android", target_os = "windows"))]
                                gl,
                                #[cfg(target_os = "android")]
                                egl_context,
                            });

                            // A new surface/context (e.g. the native-window
                            // recreation an orientation change triggers) can
                            // arrive with a frame already queued for it. Ask
                            // for one more full repaint so the whole overlay is
                            // re-serialized onto the new context instead of
                            // waiting for the next video frame.
                            if let Some(app) = notifier_state.app.upgrade() {
                                app.window().request_redraw();
                            }
                            // The frame-rate request is a property of the
                            // surface, so it is lost when the surface is
                            // recreated; re-apply it on the next tick.
                            #[cfg(target_os = "android")]
                            notifier_state
                                .frame_rate_reapply
                                .store(true, Ordering::SeqCst);
                            #[cfg(target_os = "android")]
                            {
                                let mut recovery = notifier_state.recovery.lock().unwrap();
                                recovery.setup();
                                alog(&format!("render setup ready: {recovery:?}"));
                            }
                        }
                    }
                    RenderingState::BeforeRendering => {
                        if notifier_state.enforce_sleep_deadline() { return; }
                        let Some(app) = notifier_state.app.upgrade() else {
                            return;
                        };
                        // A fade, popup or gesture animation may start between
                        // the 250ms telemetry ticks. Clear the video-only display
                        // hint before painting its first animated frame.
                        #[cfg(target_os = "android")]
                        if app.get_player_open() && app.window().has_active_animations() {
                            clear_content_frame_rate_for_ui(&notifier_state);
                        }
                        let underlay = notifier_state.underlay.lock().unwrap();
                        let Some(underlay) = underlay.as_ref() else {
                            return;
                        };

                        #[cfg(target_os = "android")]
                        if underlay.egl_context == 0
                            || underlay.egl_context != android_gl::current_context()
                        {
                            let msg = "render refused: mpv belongs to another EGL context";
                            alog(msg);
                            *notifier_state.mpv_error.lock().unwrap() = Some(msg.into());
                            return;
                        }

                        // Start playback of a freshly picked stream.
                        // Drain before entering the load body: an if-let temporary would keep
                        // pending locked while taking mpv; inventory readers take mpv first.
                        let pending_url = notifier_state.pending.lock().unwrap().take();
                        if let Some(url) = pending_url {
                            let headers = notifier_state
                                .pending_headers
                                .lock()
                                .unwrap()
                                .drain(..)
                                .collect::<Vec<_>>();
                            let subtitles = notifier_state
                                .pending_subtitles
                                .lock()
                                .unwrap()
                                .drain(..)
                                .collect::<Vec<_>>();
                            #[cfg(target_os = "android")]
                            let start = notifier_state.pending_start.lock().unwrap().take();
                            if let Ok(mpv) = notifier_state.mpv.lock()
                                && let Some(mpv) = mpv.as_ref()
                            {
                                let paused = notifier_state.take_load_pause();
                                // Set pause once, before loadfile: restoring a
                                // paused frame must not briefly start playback.
                                let result = (|| -> libmpv2::Result<()> {
                                    {
                                        let mut session = notifier_state.preferences.lock().unwrap();
                                        session.tracks.clear();
                                        session.manual_audio = false;
                                        session.manual_subtitles = false;
                                        notifier_state.subtitle_position.lock().unwrap().set_base(mpv, session.preferences.subtitle_style().position as f64);
                                        apply_preference_options(mpv, &session.preferences, &notifier_state.native_subtitle_defaults);
                                    }
                                    notifier_state.audio_processing.lock().unwrap().reset();
                                    apply_torrent_packet_cache(mpv,
                                        notifier_state.ram_torrent_source.lock().unwrap().as_deref() == Some(url.as_str()),
                                        &mut notifier_state.packet_cache_defaults.lock().unwrap())?;
                                    set_http_header_fields(mpv, &headers)?;
                                    set_string_list(mpv, c"sub-files", subtitles)?;
                                    apply_load_playback_options(mpv, paused, *notifier_state.party_playback_rate.lock().unwrap())?;
                                    // Runtime reloads use a per-file start;
                                    // mpv only honors global start on first load.
                                    #[cfg(target_os = "android")]
                                    if let Some(pos) = start {
                                        return mpv_load::load_file_at(mpv, &url, pos);
                                    }
                                    mpv.command("loadfile", &[url.as_str()])
                                })();
                                match result {
                                    Ok(()) => {
                                        *notifier_state.opened_at.lock().unwrap() = Instant::now();
                                        #[cfg(target_os = "android")]
                                        {
                                            let mut recovery = notifier_state.recovery.lock().unwrap();
                                            recovery.submitted();
                                            alog(&format!("loadfile submitted: start={start:?} paused={paused} {recovery:?}"));
                                        }
                                    }
                                    Err(error) => {
                                        #[cfg(not(target_os = "android"))]
                                        eprintln!("nova-player: loadfile failed: {error}");
                                        #[cfg(target_os = "android")]
                                        {
                                            notifier_state.recovery.lock().unwrap().load_failed();
                                            alog(&format!("loadfile/recovery failed: {error}"));
                                        }
                                        // Don't log the source or its headers:
                                        // they may contain private access tokens.
                                        // A rejected file/option is per-load, not
                                        // a broken mpv core. Keep later streams
                                        // in-app instead of forcing external playback.
                                        notifier_state.reported.store(true, Ordering::SeqCst);
                                        let _ = mpv.command("stop", &[]);
                                        notifier_state.source_waiting.store(true, Ordering::SeqCst);
                                        notifier_state.pending_paused.store(false, Ordering::SeqCst);
                                        notifier_state.start_pause_hold.store(false, Ordering::SeqCst);
                                        app.set_player_status_is_error(true);
                                        app.set_player_status(nova_ui::backend_text::tr(
                                            "Cannot play this stream (mpv could not start playback).",
                                        ).into());
                                    }
                                }
                            }
                        }

                        // Draw the video only while the player overlay is up (the
                        // catalog view paints over it anyway otherwise).
                        if !app.get_player_open() || notifier_state.source_waiting.load(Ordering::SeqCst) {
                            return;
                        }
                        // `Window::size()` is already in physical pixels, so
                        // hand mpv the real framebuffer size *without* the
                        // scale factor: multiplying again would give an
                        // oversized target and crop the video (the overflow
                        // grows with the window).
                        let size = app.window().size();
                        let w = size.width.max(1) as i32;
                        let h = size.height.max(1) as i32;

                        // Apply live layout/fade clearance before rendering, so
                        // paused subtitles also move with the Slint controls.
                        #[cfg(test)]
                        let subtitle_start = Instant::now();
                        if let Ok(mpv) = notifier_state.mpv.lock()
                            && let Some(mpv) = mpv.as_ref()
                        {
                            let _ = notifier_state.subtitle_position.lock().unwrap().apply(mpv);
                        }
                        #[cfg(test)]
                        notifier_state.timing_subtitle_us.store(subtitle_start.elapsed().as_micros() as u64, Ordering::Relaxed);

                        // The default framebuffer (FBO 0) is presented with a
                        // bottom-left origin on both desktop GL and Android's
                        // GLES/EGL window surface, i.e. vertically flipped
                        // relative to the decoded image. Passing flip_y=1
                        // cancels that, so it is needed on Android too (it was
                        // previously assumed unnecessary there, which left the
                        // video upside down).
                        let mut flip = 1_i32;
                        // Normal UI redraws also repaint the latest video frame.
                        // Never hold Slint's UI/GL thread for an mpv video deadline.
                        #[cfg(not(test))]
                        let mut block_for_target = 0_i32;
                        #[cfg(test)]
                        let mut block_for_target = i32::from(notifier_state.timing_baseline.load(Ordering::Relaxed));

                        #[cfg(test)]
                        let render_start = Instant::now();
                        unsafe {
                            let mut fbo = mpv_opengl_fbo {
                                fbo: 0,
                                w,
                                h,
                                internal_format: 0x8058,
                            };

                            let mut params = [
                                mpv_render_param {
                                    type_: MPV_RENDER_PARAM_OPENGL_FBO,
                                    data: &mut fbo as *mut _ as *mut c_void,
                                },
                                mpv_render_param {
                                    type_: MPV_RENDER_PARAM_FLIP_Y,
                                    data: &mut flip as *mut _ as *mut c_void,
                                },
                                mpv_render_param {
                                    type_: MPV_RENDER_PARAM_BLOCK_FOR_TARGET_TIME,
                                    data: &mut block_for_target as *mut _ as *mut c_void,
                                },
                                mpv_render_param {
                                    type_: MPV_RENDER_PARAM_INVALID,
                                    data: std::ptr::null_mut(),
                                },
                            ];

                            // Slint caches the GL state it set. Preserve it
                            // around mpv, and on Windows also neutralize the
                            // incoming blend/clip/pixel-transfer state.
                            #[cfg(any(target_os = "android", target_os = "windows"))]
                            let _guard = gl_state::Guard::new(&underlay.gl);

                            // Capture through the same renderer, before Slint paints its
                            // controls. HW+ frames cannot be downloaded by mpv's software
                            // screenshot fallback. Never change the decoder for a capture.
                            #[cfg(target_os = "android")]
                            if let Some(request) = notifier_state.gpu_screenshot.lock().unwrap().take() {
                                request.complete(
                                    notifier_state.screenshot_session.load(Ordering::SeqCst),
                                    notifier_state.started.load(Ordering::SeqCst),
                                    &underlay.gl,
                                    underlay.render_ctx,
                                );
                            }
                            // Also restores mpv's window dimensions after a capture.
                            mpv_render_context_render(underlay.render_ctx, params.as_mut_ptr());
                        }
                        #[cfg(test)]
                        notifier_state.timing_mpv_us.store(render_start.elapsed().as_micros() as u64, Ordering::Relaxed);
                    }
                    RenderingState::RenderingTeardown => {
                        // A queued capture belongs to this surface. Wake the worker
                        // with Unavailable instead of retaining it across recovery.
                        #[cfg(target_os = "android")]
                        notifier_state.gpu_screenshot.lock().unwrap().take();
                        #[cfg(target_os = "android")]
                        {
                            // Capture before free disables the VO and may make
                            // playback properties temporarily unavailable.
                            let mpv = notifier_state.mpv.lock().unwrap();
                            let mut recovery = notifier_state.recovery.lock().unwrap();
                            if let Some(mpv) = mpv.as_ref() {
                                recovery.observe(
                                    if notifier_state.seek_pending.load(Ordering::SeqCst) {
                                        None
                                    } else {
                                        mpv.get_property("time-pos").ok()
                                    },
                                    mpv.get_property("pause").ok(),
                                );
                            }
                            recovery.teardown();
                            alog(&format!("render teardown begin: {recovery:?}"));
                        }
                        let mut underlay = notifier_state.underlay.lock().unwrap();
                        #[cfg(target_os = "android")]
                        if let Some(old) = underlay.as_ref()
                            && (old.egl_context == 0
                                || old.egl_context != android_gl::current_context())
                        {
                            // A failed make-current must not turn into UB.
                            let msg = "render teardown refused: original EGL context is not current";
                            alog(msg);
                            *notifier_state.mpv_error.lock().unwrap() = Some(msg.into());
                            return;
                        }
                        if let Some(old) = underlay.take() {
                            // Slint suspend makes the old surface current before
                            // invoking this notifier, while ANativeWindow lives.
                            #[cfg(target_os = "android")]
                            alog(&format!("render teardown release: egl={:#x}", old.egl_context));
                            unsafe { old.release_current() };
                            #[cfg(target_os = "android")]
                            alog("render teardown complete: mpv context released");
                        }
                    }
                    _ => {}
                    }
                })
        {
            *state.mpv_error.lock().unwrap() =
                Some(format!("cannot install rendering notifier: {e}"));
        }

        Self { state }
    }

    /// Extend startup only while this RAM source is making measured progress.
    /// No-progress swarms and a decoder which never opens still reach the
    /// native error path; active prebuffering does not trigger decoder retries.
    pub fn note_ram_torrent_progress(&self, bytes: u64) {
        if self.state.ram_torrent_source.lock().unwrap().is_none() {
            return;
        }
        self.state
            .ram_buffer_progress
            .lock()
            .unwrap()
            .observe(bytes, Instant::now());
    }

    /// Only the loaded RAM source can supply adaptive-buffer hints. A pending
    /// replacement never reuses the previous source's duration or cache state.
    pub fn ram_torrent_telemetry(&self) -> Option<(String, f64, bool, f64)> {
        if self.state.pending.lock().unwrap().is_some()
            || !self.state.started.load(Ordering::SeqCst)
        {
            return None;
        }
        let source = self.state.ram_torrent_source.lock().unwrap().clone()?;
        let mpv = self.state.mpv.lock().ok()?;
        let mpv = mpv.as_ref()?;
        if mpv.get_property::<String>("path").ok().as_deref() != Some(source.as_str()) {
            return None;
        }
        Some((
            source,
            mpv.get_property("duration").ok()?,
            mpv.get_property("paused-for-cache").unwrap_or(false),
            mpv.get_property("speed").unwrap_or(1.0),
        ))
    }

    /// Bind the cache policy to this exact source, including Android reloads.
    pub fn set_ram_torrent_source(&self, url: &str, enabled: bool) {
        *self.state.ram_torrent_source.lock().unwrap() = enabled.then(|| url.to_owned());
        *self.state.ram_buffer_progress.lock().unwrap() = Default::default();
    }

    /// Open `url` in the player overlay (replacing any current playback),
    /// starting at `start_pos_secs` (0 = from the beginning: pass the resume
    /// position so playback opens in a single backend session instead of
    /// play-from-0 plus a later seek — each restart is slow on debrid
    /// backends). Returns `Err` when mpv isn't usable, so the caller can
    /// fall back to an external player.
    pub fn play(&self, url: &str, start_pos_secs: f64) -> Result<(), String> {
        self.play_with_headers(url, start_pos_secs, &[])
    }

    /// Queue playback with request headers required by the stream source.
    pub fn play_with_headers(
        &self,
        url: &str,
        start_pos_secs: f64,
        headers: &[(String, String)],
    ) -> Result<(), String> {
        self.play_with_options(url, start_pos_secs, headers, &[])
    }

    /// Queue source headers and subtitle files before loading a stream.
    pub fn play_with_options(
        &self,
        url: &str,
        start_pos_secs: f64,
        headers: &[(String, String)],
        subtitles: &[String],
    ) -> Result<(), String> {
        self.play_with_preferences(
            url,
            start_pos_secs,
            headers,
            subtitles,
            SessionPreferences::from_settings(
                &nova_config::active_cache_settings(),
                TitlePreferences::default(),
            ),
        )
    }

    /// Queue a new session with semantic defaults and title overrides.
    pub fn play_with_preferences(
        &self,
        url: &str,
        start_pos_secs: f64,
        headers: &[(String, String)],
        subtitles: &[String],
        preferences: SessionPreferences,
    ) -> Result<(), String> {
        self.play_with_preferences_and_policy(
            url,
            start_pos_secs,
            headers,
            subtitles,
            preferences,
            true,
        )
    }

    /// Queue playback while retaining a source's external-handoff policy through
    /// Android decoder recovery. Ordinary streams keep their existing fallback.
    pub fn play_with_preferences_and_policy(
        &self,
        url: &str,
        start_pos_secs: f64,
        headers: &[(String, String)],
        subtitles: &[String],
        preferences: SessionPreferences,
        allow_external: bool,
    ) -> Result<(), String> {
        self.play_with_preferences_and_load_policy(
            url,
            start_pos_secs,
            headers,
            subtitles,
            preferences,
            PlaybackLoadPolicy {
                allow_external,
                paused: false,
            },
        )
    }

    /// Queue a source held paused before loadfile/first frame when confirmation is required.
    pub fn play_with_preferences_and_load_policy(
        &self,
        url: &str,
        start_pos_secs: f64,
        headers: &[(String, String)],
        subtitles: &[String],
        preferences: SessionPreferences,
        policy: PlaybackLoadPolicy,
    ) -> Result<(), String> {
        let subtitles = subtitles
            .iter()
            .take(20)
            .filter(|url| url.len() <= 4096)
            .cloned()
            .map(SourceSubtitle::from)
            .collect::<Vec<_>>();
        self.play_with_source_subtitles_and_load_policy(
            url,
            start_pos_secs,
            headers,
            &subtitles,
            preferences,
            policy,
        )
    }

    /// Typed source captions retain declared facts while an origin-aware app
    /// worker fetches them. mpv only receives local caption files.
    pub fn play_with_source_subtitles_and_load_policy(
        &self,
        url: &str,
        start_pos_secs: f64,
        headers: &[(String, String)],
        subtitles: &[SourceSubtitle],
        preferences: SessionPreferences,
        policy: PlaybackLoadPolicy,
    ) -> Result<(), String> {
        self.state
            .start_pause_hold
            .store(policy.paused, Ordering::SeqCst);
        let ram_torrent = self.state.ram_torrent_source.lock().unwrap().as_deref() == Some(url);
        let allow_external = policy.allow_external && !policy.paused && !ram_torrent;
        #[cfg(target_os = "android")]
        external::update_picture_in_picture(false, true, 16, 9);
        {
            let mpv = self.state.mpv.lock().unwrap();
            self.state.subtitle_sync.lock().unwrap().clear(mpv.as_ref());
            self.state.screenshot_session.fetch_add(1, Ordering::SeqCst);
            self.state.started.store(false, Ordering::SeqCst);
        }
        self.state
            .allow_external
            .store(allow_external, Ordering::SeqCst);
        if let Some(app) = self.state.app.upgrade() {
            app.set_effective_subtitle_style(slint::ModelRc::new(slint::VecModel::from(
                self.resolve_subtitle_style(preferences.subtitle_style())
                    .values()
                    .to_vec(),
            )));
            app.set_effective_playback_speed(
                self.state
                    .party_playback_rate
                    .lock()
                    .unwrap()
                    .map_or(preferences.speed(), |rate| rate as f32),
            );
            app.set_screenshot_busy(false);
            app.set_screenshot_notice(slint::SharedString::default());
            app.set_player_chapters(Default::default());
            app.set_secondary_sub_tracks(Default::default());
            app.set_secondary_sub_track_label(Default::default());
        }
        *self.state.preferences.lock().unwrap() = PreferenceSession {
            preferences,
            tracks: String::new(),
            manual_audio: false,
            manual_subtitles: false,
        };
        *self.state.secondary_subtitles.lock().unwrap() = TrackChoice::Off;
        *self.state.preference_source.lock().unwrap() = url.to_owned();
        self.state.source_subtitles.lock().unwrap().clear();
        self.state.online_subtitles.lock().unwrap().clear();
        #[cfg(target_os = "android")]
        {
            self.state.loaded_online_subtitles.lock().unwrap().clear();
        }
        self.state.external_handoff.store(false, Ordering::SeqCst);
        if let Some(err) = self.state.mpv_error.lock().unwrap().as_ref() {
            return Err(err.clone());
        }
        let Some(app) = self.state.app.upgrade() else {
            return Err("player window gone".into());
        };

        self.state.source_waiting.store(false, Ordering::SeqCst);
        *self.state.pending.lock().unwrap() = Some(url.to_string());
        *self.state.pending_headers.lock().unwrap() = headers.to_vec();
        let subtitles = source_subtitles::bounded(subtitles);
        self.state.source_subtitles.lock().unwrap().begin(
            url,
            self.state.screenshot_session.load(Ordering::SeqCst),
            subtitles.clone(),
        );
        self.state
            .source_subtitles
            .lock()
            .unwrap()
            .set_headers(headers);
        // http-header-fields is global to mpv, including subtitle requests and
        // redirects. Only local captions may enter this native list.
        self.state.pending_subtitles.lock().unwrap().clear();
        #[cfg(target_os = "android")]
        let start_paused = policy.paused || !ANDROID_FOREGROUND.load(Ordering::SeqCst);
        #[cfg(not(target_os = "android"))]
        let start_paused = policy.paused;
        self.state
            .pending_paused
            .store(start_paused, Ordering::SeqCst);
        #[cfg(target_os = "android")]
        {
            // Keep the URL for a decoder fallback reload; then arm the chain
            // from the user's Settings → Player preference. A new stream drops
            // any runtime override the previous stream picked.
            self.state.recovery.lock().unwrap().begin(start_pos_secs);
            *self.state.pending_start.lock().unwrap() = Some(start_pos_secs.max(0.0));
            *self.state.last_url.lock().unwrap() = Some(url.to_string());
            *self.state.last_headers.lock().unwrap() = headers.to_vec();
            self.state.last_subtitles.lock().unwrap().clear();
            *self.state.last_hwdec_log.lock().unwrap() = String::from("<unset>");
            *self.state.session_hwdec.lock().unwrap() = None;
            self.state.hwdec_checked.store(false, Ordering::SeqCst);
            self.arm_decoder_chain();
        }
        self.state.started.store(false, Ordering::SeqCst);
        self.state.buffering.store(false, Ordering::SeqCst);
        self.state.seek_pending.store(false, Ordering::SeqCst);
        *self.state.seek_deadline.lock().unwrap() = None;
        *self.state.seek_buffer.lock().unwrap() = None;
        self.state.reported.store(false, Ordering::SeqCst);
        *self.state.opened_at.lock().unwrap() = Instant::now();

        // A new foreground stream starts playing regardless of how the previous
        // one ended. A source resolved after Android Home stays paused.
        // mpv's `pause` property survives `stop`/EOF, so set it
        // here (and again before loadfile in the render callback) — otherwise
        // the second playback would silently start paused. `start` is set on
        // every play (even 0.0): it persists across files, so a previous
        // resume must not leak into the next playback.
        if let Ok(mpv) = self.state.mpv.lock()
            && let Some(mpv) = mpv.as_ref()
        {
            // Desktop session picks live only in mpv. Every new stream starts
            // from the saved device preference, never the previous stream's pick.
            #[cfg(any(target_os = "windows", target_os = "linux"))]
            let _ = configure_desktop_decoder(mpv);
            let _ = mpv.set_property("start", start_pos_secs.max(0.0));
            // Rate is per device (Settings → Player / the player's own
            // settings panel); `speed` also survives stop/EOF, so set it
            // from the saved value on every play instead of letting the
            // previous session's rate leak into this one.
            let _ = mpv.set_property("speed", self.effective_playback_speed() as f64);
            let _ = apply_load_playback_options(
                mpv,
                start_paused,
                *self.state.party_playback_rate.lock().unwrap(),
            );
        }

        // Android: immersive playback — hide the status and navigation bars
        // initially. The fullscreen action changes the retry policy; `close`
        // requests the bars back.
        #[cfg(target_os = "android")]
        {
            external::set_playback_active(
                true,
                nova_config::active_cache_settings().android_auto_rotate,
            );
            let _ = set_system_bars_hidden(true);
            app.set_is_fullscreen(true);
        }
        // Loading counts as engaged: keep the screen awake from open.
        #[cfg(target_os = "linux")]
        converge_idle_inhibit(true);
        #[cfg(target_os = "android")]
        converge_screen_on(!start_paused);

        #[cfg(not(target_os = "android"))]
        if !app.get_player_open() {
            self.restore_playback_fullscreen(&app);
        }
        #[cfg(not(target_os = "android"))]
        self.state
            .transition_window_held
            .store(false, Ordering::SeqCst);
        app.set_player_open(true);
        app.set_playback_started(false);
        app.set_position(0.0);
        app.set_duration(0.0);
        app.set_buffered_until(0.0);
        app.set_player_badges(Default::default());
        app.set_is_paused(true);
        app.set_time_text(slint::SharedString::from("00:00 / 00:00"));
        app.set_player_status_is_error(false);
        app.set_player_status(slint::SharedString::from("Loading stream…"));
        // Android: show the decoder that was just armed right away, so the
        // badge is populated before the first frame and never reads "unknown"
        // after an orientation change. `tick` corrects it to the decoder mpv
        // actually engages (following any fallback). Windows/Linux start with their
        // saved preference below; other desktops keep the label empty.
        #[cfg(target_os = "android")]
        {
            let (label, idx) = match self.state.decoder_chain.lock().unwrap().first().copied() {
                Some(HWDEC_PREFERRED) => ("HW+", 0),
                Some(HWDEC_FALLBACK) => ("HW", 1),
                _ => ("SW", 2),
            };
            app.set_decode_label(slint::SharedString::from(label));
            app.set_decoder_selected(idx);
        }
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        {
            let preference = nova_config::active_cache_settings()
                .desktop_hwdec
                .supported();
            app.set_decode_label(preference.label().into());
            app.set_decoder_selected(preference.index());
        }
        #[cfg(not(any(target_os = "android", target_os = "windows", target_os = "linux")))]
        app.set_decode_label(slint::SharedString::default());
        app.set_osd_visible(true);
        // Android has no volume UI: pin playback to 100% (unmuted) so a stale
        // value from an earlier session can never leave it silent. Desktop
        // keeps mirroring the live mpv values (persist across streams).
        #[cfg(target_os = "android")]
        {
            if let Ok(mpv) = self.state.mpv.lock()
                && let Some(mpv) = mpv.as_ref()
            {
                let _ = mpv.set_property("volume", 100.0_f64);
                let _ = mpv.set_property("mute", false);
            }
            app.set_volume(100.0);
            app.set_muted(false);
        }
        #[cfg(not(target_os = "android"))]
        if let Ok(mpv) = self.state.mpv.lock()
            && let Some(mpv) = mpv.as_ref()
        {
            let vol: f64 = mpv.get_property("volume").unwrap_or(100.0);
            let muted: bool = mpv.get_property("mute").unwrap_or(false);
            app.set_volume(vol as f32);
            app.set_muted(muted);
        }
        app.window().request_redraw();
        Ok(())
    }

    /// Toggle play/pause (overlay Play button or click on the video).
    pub fn toggle(&self) {
        self.set_paused(!self.is_paused());
    }

    /// Query the receiver rather than a delayed UI mirror when toggling remotely.
    pub fn is_paused(&self) -> bool {
        if self.state.start_pause_hold.load(Ordering::SeqCst) {
            return true;
        }
        self.state
            .mpv
            .lock()
            .ok()
            .and_then(|mpv| mpv.as_ref().and_then(|mpv| mpv.get_property("pause").ok()))
            .unwrap_or(true)
    }

    /// Recovery timing needs user intent before the first frame: the loading
    /// overlay's paused=true placeholder and an idle old mpv file are unrelated.
    pub fn playback_pause_requested(&self) -> bool {
        if self.state.start_pause_hold.load(Ordering::SeqCst) {
            return true;
        }
        #[cfg(target_os = "android")]
        if !ANDROID_FOREGROUND.load(Ordering::SeqCst) {
            return true;
        }
        if self.state.source_waiting.load(Ordering::SeqCst) || self.playback_failed() {
            return false;
        }
        if self.state.pending.lock().unwrap().is_some() {
            return self.state.pending_paused.load(Ordering::SeqCst);
        }
        self.state
            .mpv
            .lock()
            .ok()
            .and_then(|mpv| mpv.as_ref().and_then(|mpv| mpv.get_property("pause").ok()))
            .unwrap_or(false)
    }

    pub fn awaiting_start_confirmation(&self) -> bool {
        self.state.start_pause_hold.load(Ordering::SeqCst)
    }

    /// Release only after the app accepted the replacement's timeline decision.
    pub fn release_start_pause(&self) {
        self.state.start_pause_hold.store(false, Ordering::SeqCst);
    }

    /// Apply an explicit pause state using the same platform side effects as Play.
    pub fn set_paused(&self, new_paused: bool) {
        if !new_paused && self.state.start_pause_hold.load(Ordering::SeqCst) {
            return;
        }
        if self.state.source_waiting.load(Ordering::SeqCst) {
            return;
        }
        let Ok(mpv_guard) = self.state.mpv.lock() else {
            return;
        };
        let Some(mpv) = mpv_guard.as_ref() else {
            return;
        };
        let _ = mpv.set_property("pause", new_paused);
        if self.state.pending.lock().unwrap().is_some() {
            self.state
                .pending_paused
                .store(new_paused, Ordering::SeqCst);
        }
        if let Some(app) = self.state.app.upgrade() {
            app.set_is_paused(new_paused);
        }
        // Screens stay awake only while actually playing (mpv/VLC behavior).
        #[cfg(target_os = "linux")]
        converge_idle_inhibit(!new_paused);
        #[cfg(target_os = "android")]
        converge_screen_on(!new_paused);
        #[cfg(target_os = "android")]
        {
            drop(mpv_guard);
            if let Some(app) = self.state.app.upgrade() {
                android_pip::publish(&self.state, &app, false);
            }
        }
    }

    /// Requests for the accepted source only. The transport must bind video
    /// headers to the video's origin, including across caption redirects.
    pub fn source_subtitle_requests(&self) -> Vec<SourceSubtitleRequest> {
        self.state.source_subtitles.lock().unwrap().requests()
    }

    /// Accept a downloaded caption only into the exact originating session.
    /// Local files stay owned by the player through Android renderer recovery.
    pub fn attach_source_subtitle(
        &self,
        session: u64,
        url: &str,
        file: SourceSubtitleFile,
    ) -> bool {
        if self.state.screenshot_session.load(Ordering::SeqCst) != session {
            return false;
        }
        let mut subtitles = self.state.source_subtitles.lock().unwrap();
        if !subtitles.attach(session, url, file) {
            return false;
        }
        #[cfg(target_os = "android")]
        {
            *self.state.last_subtitles.lock().unwrap() = subtitles.local_paths();
        }
        true
    }

    fn load_source_subtitles(&self) {
        if self.state.pending.lock().unwrap().is_some() {
            return;
        }
        let source = self.state.preference_source.lock().unwrap().clone();
        let engine = self.state.mpv.lock().unwrap();
        let Some(mpv) = engine.as_ref() else {
            return;
        };
        if mpv.get_property::<String>("path").ok().as_deref() != Some(source.as_str()) {
            return;
        }
        if mpv.get_property::<f64>("time-pos").is_err() {
            return;
        }
        let manual = self.state.preferences.lock().unwrap().manual_subtitles;
        self.state.source_subtitles.lock().unwrap().load_pending(
            mpv,
            &source,
            self.state.screenshot_session.load(Ordering::SeqCst),
            manual,
        );
    }

    /// Seek to `pos` seconds (OSD slider).
    pub fn seek(&self, pos: f32) {
        if !pos.is_finite() || self.state.start_pause_hold.load(Ordering::SeqCst) {
            return;
        }
        let app = self.state.app.upgrade();
        let pending = self.state.pending.lock().unwrap().is_some();
        let source = self.state.preference_source.lock().unwrap().clone();
        let session = self.state.screenshot_session.load(Ordering::SeqCst);
        let now = Instant::now();
        let target = pos.max(0.0) as f64;
        // Android can seek a queued decoder/surface reload before native
        // properties return. Its per-load start owns that request, rather than
        // an indefinitely pending native time-pos setter.
        #[cfg(target_os = "android")]
        if pending {
            self.state.recovery.lock().unwrap().seek(target);
            *self.state.pending_start.lock().unwrap() = Some(target);
            if let Some(app) = app {
                app.set_position(target as f32);
            }
            self.state.seek_pending.store(false, Ordering::SeqCst);
            *self.state.seek_deadline.lock().unwrap() = None;
            return;
        }
        let grace = app.as_ref().and_then(|app| {
            if pending || !self.state.started.load(Ordering::SeqCst) {
                return None;
            }
            let engine = self.state.mpv.lock().ok()?;
            let mpv = engine.as_ref()?;
            if mpv.get_property::<String>("path").ok().as_deref() != Some(source.as_str()) {
                return None;
            }
            let cache = mpv.get_property::<String>("demuxer-cache-state").ok();
            let previous = *self.state.seek_buffer.lock().unwrap();
            if cache.is_none()
                && let Some(previous) = previous
            {
                // Repeated keys can reuse a confirmed backward range, but
                // cannot prolong missing-telemetry grace indefinitely.
                return previous
                    .visible(session, now, target, app.get_duration() as f64)
                    .map(|_| previous);
            }
            SeekBuffer::begin(
                session,
                now,
                target,
                app.get_position() as f64,
                app.get_duration() as f64,
                app.get_buffered_until() as f64,
                cache.as_deref(),
            )
        });
        // Reject unsupported seeks before publishing an optimistic target.
        // mpv can return success for time-pos on a pipe without moving it.
        let accepted = self.state.mpv.lock().ok().is_some_and(|engine| {
            engine.as_ref().is_some_and(|mpv| {
                !pending
                    && self.state.started.load(Ordering::SeqCst)
                    && mpv.get_property::<String>("path").ok().as_deref() == Some(source.as_str())
                    && mpv.get_property::<bool>("seekable").unwrap_or(false)
                    && mpv.set_property("time-pos", target).is_ok()
            })
        });
        if !accepted {
            self.state.seek_pending.store(false, Ordering::SeqCst);
            *self.state.seek_deadline.lock().unwrap() = None;
            *self.state.seek_buffer.lock().unwrap() = None;
            if let Some(app) = app {
                if let Ok(engine) = self.state.mpv.lock()
                    && let Some(mpv) = engine.as_ref()
                    && mpv.get_property::<String>("path").ok().as_deref() == Some(source.as_str())
                    && let Ok(actual) = mpv.get_property::<f64>("time-pos")
                {
                    app.set_position(actual.max(0.0) as f32);
                }
                app.set_buffered_until(0.0);
            }
            return;
        }
        *self.state.seek_buffer.lock().unwrap() = grace;
        if let Some(app) = app {
            app.set_buffered_until(
                grace
                    .and_then(|grace| {
                        grace.visible(session, now, target, app.get_duration() as f64)
                    })
                    .unwrap_or(0.0),
            );
            app.set_position(pos.max(0.0));
        }
        #[cfg(target_os = "android")]
        {
            self.state.recovery.lock().unwrap().seek(pos as f64);
            if self.state.pending.lock().unwrap().is_some() {
                *self.state.pending_start.lock().unwrap() = Some(pos.max(0.0) as f64);
            }
        }
        // Hold the committed target briefly while mpv catches up. Native
        // acceptance does not guarantee the requested position is reachable.
        *self.state.seek_deadline.lock().unwrap() = Some(now + Duration::from_secs(5));
        self.state.seek_pending.store(true, Ordering::SeqCst);
        #[cfg(target_os = "linux")]
        self.state.seek_serial.fetch_add(1, Ordering::Relaxed);
    }

    /// Relative commands resolve against the receiver's latest playhead.
    pub fn seek_relative(&self, delta: f64) {
        let app = self.state.app.upgrade();
        let fallback = app
            .as_ref()
            .map(|app| (app.get_position() as f64, app.get_duration() as f64))
            .unwrap_or_default();
        #[cfg(target_os = "android")]
        let queued_position = {
            let pending = self.state.pending.lock().unwrap().is_some();
            let queued = pending
                .then(|| *self.state.pending_start.lock().unwrap())
                .flatten();
            // The render thread consumes `pending` before fresh properties are
            // available. The existing UI mirror survives surface/decoder reloads.
            queued.or_else(|| {
                (!self.state.started.load(Ordering::SeqCst)
                    && app.as_ref().is_some_and(|app| app.get_playback_started()))
                .then_some(fallback.0)
            })
        };
        #[cfg(not(target_os = "android"))]
        let queued_position = None;
        let (position, duration) = self
            .state
            .mpv
            .lock()
            .ok()
            .and_then(|mpv| {
                mpv.as_ref().map(|mpv| {
                    (
                        mpv.get_property::<f64>("time-pos").ok(),
                        mpv.get_property::<f64>("duration").ok(),
                    )
                })
            })
            .unwrap_or_default();
        let (position, duration) =
            receiver_seek_state(position, duration, fallback, queued_position);
        self.seek(bounded_seek(position, delta, duration).min(f32::MAX as f64) as f32);
    }

    /// Read live volume before applying a relative adjustment.
    pub fn step_volume(&self, delta: f64) {
        let volume = self
            .state
            .mpv
            .lock()
            .ok()
            .and_then(|mpv| {
                mpv.as_ref()
                    .and_then(|mpv| mpv.get_property::<f64>("volume").ok())
            })
            .unwrap_or_else(|| {
                self.state
                    .app
                    .upgrade()
                    .map_or(100.0, |app| app.get_volume() as f64)
            });
        self.set_volume((volume + delta).clamp(0.0, 100.0) as f32);
    }

    /// Set volume (0–100).
    pub fn set_volume(&self, vol: f32) {
        let Ok(mpv) = self.state.mpv.lock() else {
            return;
        };
        if let Some(mpv) = mpv.as_ref() {
            let _ = mpv.set_property("volume", vol.clamp(0.0, 100.0) as f64);
            let _ = mpv.set_property("mute", false);
        }
        if let Some(app) = self.state.app.upgrade() {
            app.set_volume(vol.clamp(0.0, 100.0));
            app.set_muted(false);
        }
    }

    /// Apply a live rate, including transient gesture previews. The semantic
    /// base rate lives in the session and is restored after previews/reloads.
    pub fn set_speed(&self, speed: f32) {
        // A local gesture must not steal the party clock's temporary rate.
        if self.state.party_playback_rate.lock().unwrap().is_some() {
            return;
        }
        let Ok(mpv) = self.state.mpv.lock() else {
            return;
        };
        if let Some(mpv) = mpv.as_ref() {
            let _ = mpv.set_property("speed", nova_config::clamp_playback_speed(speed) as f64);
        }
    }

    /// Apply a temporary absolute party clock rate, including ±3% correction
    /// around the selectable target bounds. Clearing it
    /// restores the latest title/global speed, including edits made meanwhile.
    /// A queued source receives the correction when its load is submitted.
    pub fn set_party_playback_rate(&self, rate: Option<f64>) {
        // Only an explicit None leaves party ownership. A malformed correction
        // must not restore personal speed or enable ordinary local controls.
        if rate.is_some_and(|rate| !rate.is_finite()) {
            return;
        }
        let rate = normalized_party_rate(rate);
        *self.state.party_playback_rate.lock().unwrap() = rate;
        let effective = self.effective_playback_speed();
        if self.state.pending.lock().unwrap().is_none()
            && self.state.started.load(Ordering::SeqCst)
            && !self.state.source_waiting.load(Ordering::SeqCst)
        {
            let source = self.state.preference_source.lock().unwrap().clone();
            if let Ok(mpv) = self.state.mpv.lock()
                && let Some(mpv) = mpv.as_ref()
                && !source.is_empty()
                && mpv.get_property::<String>("path").ok().as_deref() == Some(source.as_str())
                && mpv.get_property::<bool>("idle-active").ok() == Some(false)
            {
                let _ = mpv.set_property("speed", effective as f64);
                // Leaving a party restores the rate, while ordinary playback
                // still retains the source for seeking after EOF.
                let _ = mpv.set_property("keep-open", "yes");
            }
        }
        if let Some(app) = self.state.app.upgrade() {
            app.set_effective_playback_speed(effective);
        }
    }

    /// Read only telemetry owned by the currently loaded source. User pause
    /// and core-idle are deliberately not interpreted as cache starvation.
    pub fn playback_readiness(&self) -> PlaybackReadiness {
        if self.state.pending.lock().unwrap().is_some()
            || !self.state.started.load(Ordering::SeqCst)
            || self.state.source_waiting.load(Ordering::SeqCst)
        {
            return PlaybackReadiness::default();
        }
        let source = self.state.preference_source.lock().unwrap().clone();
        let Ok(mpv) = self.state.mpv.lock() else {
            return PlaybackReadiness::default();
        };
        let Some(mpv) = mpv.as_ref() else {
            return PlaybackReadiness::default();
        };
        if source.is_empty()
            || mpv.get_property::<String>("path").ok().as_deref() != Some(source.as_str())
            || mpv.get_property::<bool>("idle-active").ok() != Some(false)
        {
            return PlaybackReadiness::default();
        }
        if let Ok(buffering) = mpv.get_property::<bool>("paused-for-cache") {
            self.state.buffering.store(buffering, Ordering::SeqCst);
        }
        PlaybackReadiness {
            buffering: self.state.buffering.load(Ordering::SeqCst),
            seekable: mpv.get_property("seekable").unwrap_or(false),
            ended: mpv.get_property("eof-reached").unwrap_or(false),
            buffered_ahead: readiness_buffered_ahead(
                mpv.get_property("time-pos").unwrap_or(f64::NAN),
                mpv.get_property("duration").unwrap_or(0.0),
                mpv.get_property("demuxer-via-network").ok(),
                mpv.get_property("demuxer-cache-time").ok(),
                mpv.get_property("demuxer-cache-duration").ok(),
            ),
        }
    }

    /// Toggle mute on/off.
    pub fn toggle_mute(&self) {
        let Ok(mpv) = self.state.mpv.lock() else {
            return;
        };
        if let Some(mpv) = mpv.as_ref() {
            let current: bool = mpv.get_property("mute").unwrap_or(false);
            let _ = mpv.set_property("mute", !current);
            if let Some(app) = self.state.app.upgrade() {
                app.set_muted(!current);
            }
        }
    }

    pub fn set_muted(&self, muted: bool) {
        if let Ok(mpv) = self.state.mpv.lock()
            && let Some(mpv) = mpv.as_ref()
        {
            let _ = mpv.set_property("mute", muted);
        }
        if let Some(app) = self.state.app.upgrade() {
            app.set_muted(muted);
        }
    }

    /// Toggle window fullscreen.
    pub fn toggle_fullscreen(&self) {
        if let Some(app) = self.state.app.upgrade() {
            #[cfg(target_os = "android")]
            self.set_fullscreen(!app.get_is_fullscreen());
            #[cfg(not(target_os = "android"))]
            self.set_fullscreen(!app.window().is_fullscreen());
        }
    }

    pub fn set_fullscreen(&self, fullscreen: bool) {
        if let Some(app) = self.state.app.upgrade() {
            app.window().set_fullscreen(fullscreen);
            app.set_is_fullscreen(fullscreen);
            // The Android immersion retry loop must follow this explicit
            // choice instead of hiding the bars again on the next tick.
            #[cfg(target_os = "android")]
            let _ = set_system_bars_hidden(fullscreen);
            #[cfg(not(target_os = "android"))]
            self.state
                .playback_fullscreen
                .store(fullscreen, Ordering::SeqCst);
        }
    }

    /// Device-local preference, independent of the catalog's maximized state.
    #[cfg(not(target_os = "android"))]
    pub fn set_playback_fullscreen_preference(&self, fullscreen: bool) {
        self.state
            .playback_fullscreen
            .store(fullscreen, Ordering::SeqCst);
    }

    #[cfg(not(target_os = "android"))]
    pub fn playback_fullscreen_preference(&self) -> bool {
        if let Some(app) = self.state.app.upgrade()
            && app.get_player_open()
        {
            return app.window().is_fullscreen();
        }
        self.state.playback_fullscreen.load(Ordering::SeqCst)
    }

    #[cfg(not(target_os = "android"))]
    fn restore_playback_fullscreen(&self, app: &AppWindow) {
        if self
            .state
            .transition_window_held
            .swap(false, Ordering::SeqCst)
        {
            // The picker still owns the playback window. Preserve its original
            // catalog geometry, and honor an explicit WM fullscreen exit made
            // while the next source was being selected.
            self.state
                .playback_fullscreen
                .store(app.window().is_fullscreen(), Ordering::SeqCst);
        } else {
            self.state
                .catalog_maximized
                .store(app.window().is_maximized(), Ordering::SeqCst);
        }
        let fullscreen = self.state.playback_fullscreen.load(Ordering::SeqCst);
        app.window().set_fullscreen(fullscreen);
        app.set_is_fullscreen(fullscreen);
    }

    /// Apply orientation policy on settings edits and active-profile changes.
    #[cfg(target_os = "android")]
    pub fn set_android_auto_rotate(&self, enabled: bool) {
        if let Some(app) = self.state.app.upgrade() {
            external::set_playback_active(app.get_player_open(), enabled);
        }
    }

    /// Select a stream-local id, returning metadata only after a valid pick.
    pub fn pick_track_preference(&self, audio: bool, track_id: i32) -> Option<TrackChoice> {
        if self.state.pending.lock().unwrap().is_some() {
            return None;
        }
        let choice = {
            let mpv = self.state.mpv.lock().ok()?;
            let mpv = mpv.as_ref()?;
            if mpv.get_property::<String>("path").ok().as_deref()
                != Some(self.state.preference_source.lock().unwrap().as_str())
            {
                return None;
            }
            let choice = if track_id < 0 {
                TrackChoice::Off
            } else {
                let raw = self.state.track_inventory(mpv).ok()?;
                let track = track_preferences::parse_tracks(&raw)
                    .into_iter()
                    .find(|t| t.audio == audio && t.id == track_id)?;
                TrackChoice::Track(track.descriptor)
            };
            if !audio {
                // mpv cannot select the same track in primary and secondary
                // slots. Release an invisible reference before a viewer pick.
                self.state
                    .subtitle_sync
                    .lock()
                    .unwrap()
                    .release_reference(mpv);
            }
            mpv.set_property(
                if audio { "aid" } else { "sid" },
                if track_id < 0 {
                    "no".to_owned()
                } else {
                    track_id.to_string()
                },
            )
            .ok()?;
            let mut session = self.state.preferences.lock().ok()?;
            if !audio {
                self.state
                    .subtitle_sync
                    .lock()
                    .unwrap()
                    .primary_selection_changed(
                        mpv,
                        self.resolve_subtitle_style(session.preferences.subtitle_style())
                            .delay as f64,
                    );
            }
            if audio {
                session.preferences.title.audio = Some(choice.clone());
                session.manual_audio = true;
            } else {
                session.preferences.title.subtitles = Some(choice.clone());
                session.manual_subtitles = true;
            }
            choice
        };
        if !audio {
            self.reapply_secondary_subtitle();
        }
        self.refresh_tracks();
        Some(choice)
    }

    pub fn pick_audio_track(&self, track_id: i32) {
        let _ = self.pick_track_preference(true, track_id);
    }
    pub fn pick_sub_track(&self, track_id: i32) {
        let _ = self.pick_track_preference(false, track_id);
    }

    /// Reset just one field; all other session choices remain untouched.
    pub fn restore_playback_default(&self, control: i32, settings: &nova_config::CacheSettings) {
        {
            let mpv = self.state.mpv.lock().unwrap();
            let mut session = self.state.preferences.lock().unwrap();
            match control {
                0 => {
                    session.preferences.title.audio = None;
                    session.manual_audio = false;
                    session.preferences.audio_language = settings.default_audio_language.clone();
                }
                1 => {
                    session.preferences.title.subtitles = None;
                    session.manual_subtitles = false;
                    session.preferences.subtitle_language =
                        settings.default_subtitle_language.clone();
                }
                2 => {
                    session.preferences.title.speed = None;
                    session.preferences.default_speed = settings.playback_speed;
                }
                3 => {
                    session.preferences.title.subtitle_style = None;
                    session.preferences.default_subtitle_style =
                        settings.subtitle_style.normalized();
                    if let Some(mpv) = mpv.as_ref() {
                        self.state
                            .subtitle_sync
                            .lock()
                            .unwrap()
                            .reset_timing(mpv, false);
                    }
                }
                _ => return,
            }
            session.tracks.clear();
            if let Some(mpv) = mpv.as_ref() {
                if control == 3 {
                    self.state
                        .apply_subtitle_style(mpv, session.preferences.subtitle_style());
                } else if control == 2 {
                    if self.state.party_playback_rate.lock().unwrap().is_none() {
                        let _ = mpv.set_property("speed", session.preferences.speed() as f64);
                    }
                } else {
                    if control == 1 {
                        self.state
                            .subtitle_sync
                            .lock()
                            .unwrap()
                            .release_reference(mpv);
                    }
                    apply_track_options(mpv, &session.preferences, control == 0);
                }
            }
        }
        self.resolve_preferred_tracks();
        self.refresh_tracks();
    }

    /// The selected mpv track ID belongs only to the currently loaded stream.
    pub fn selected_track_id(&self, audio: bool) -> i32 {
        self.state
            .mpv
            .lock()
            .ok()
            .and_then(|mpv| {
                mpv.as_ref().and_then(|mpv| {
                    mpv.get_property::<i64>(if audio { "aid" } else { "sid" })
                        .ok()
                })
            })
            .and_then(|id| i32::try_from(id).ok())
            .unwrap_or(-1)
    }

    /// FFmpeg stream index of the selected audio track, scoped to the loaded source.
    /// Receiver remuxing must not confuse mpv's user track ID with a demuxer stream index.
    pub fn selected_audio_stream_index(&self) -> Option<usize> {
        let mpv = self.state.mpv.lock().ok()?;
        let mpv = mpv.as_ref()?;
        let selected = mpv.get_property::<i64>("aid").ok()?;
        let raw = self.state.track_inventory(mpv).ok()?;
        let tracks: serde_json::Value = serde_json::from_str(&raw).ok()?;
        tracks
            .as_array()?
            .iter()
            .find(|track| {
                track.get("type").and_then(|v| v.as_str()) == Some("audio")
                    && track.get("id").and_then(|v| v.as_i64()) == Some(selected)
                    && !track
                        .get("external")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
            })?
            .get("ff-index")?
            .as_u64()
            .and_then(|index| usize::try_from(index).ok())
    }

    /// Selected external subtitle source for a private relay, or `false` for embedded/bitmap tracks.
    pub fn selected_subtitle_source(&self) -> Option<(bool, String)> {
        let mpv = self.state.mpv.lock().ok()?;
        let mpv = mpv.as_ref()?;
        let selected = mpv.get_property::<i64>("sid").ok()?;
        let raw = self.state.track_inventory(mpv).ok()?;
        let tracks: serde_json::Value = serde_json::from_str(&raw).ok()?;
        let track = tracks.as_array()?.iter().find(|track| {
            track.get("type").and_then(|v| v.as_str()) == Some("sub")
                && track.get("id").and_then(|v| v.as_i64()) == Some(selected)
        })?;
        let external = track
            .get("external")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Some((
            external,
            track
                .get("external-filename")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_owned(),
        ))
    }

    /// Device preferences apply to active audio without a loadfile/seek.
    pub fn update_audio_processing(&self, settings: &nova_config::CacheSettings) {
        let mpv = self.state.mpv.lock().unwrap();
        let mut processing = self.state.audio_processing.lock().unwrap();
        processing.configure(settings);
        if let Some(mpv) = mpv.as_ref() {
            processing.poll(mpv);
        }
        if let Some(app) = self.state.app.upgrade() {
            app.set_audio_processing_status(processing.status.clone().into());
        }
    }

    pub fn effective_playback_speed(&self) -> f32 {
        let base = self.state.preferences.lock().unwrap().preferences.speed();
        self.state
            .party_playback_rate
            .lock()
            .unwrap()
            .map_or(base, |rate| rate as f32)
    }

    /// App-level timing policy reads the cached typed title choice, without
    /// touching mpv or re-reading the preference store on each playback tick.
    pub fn intro_skip_mode(
        &self,
        global: nova_config::IntroSkipMode,
    ) -> nova_config::IntroSkipMode {
        self.state
            .preferences
            .lock()
            .unwrap()
            .preferences
            .intro_skip_mode(global)
    }

    pub fn set_title_intro_skip_mode(&self, mode: Option<nova_config::IntroSkipMode>) {
        self.state
            .preferences
            .lock()
            .unwrap()
            .preferences
            .title
            .intro_skip = mode;
    }

    pub fn effective_subtitle_style(&self) -> nova_config::playback_preferences::SubtitleStyle {
        let mpv = self.state.mpv.lock().unwrap();
        let mut style = self.resolve_subtitle_style(
            self.state
                .preferences
                .lock()
                .unwrap()
                .preferences
                .subtitle_style(),
        );
        if let Some(mpv) = mpv.as_ref() {
            style.delay = self
                .state
                .subtitle_sync
                .lock()
                .unwrap()
                .delay(mpv, style.delay as f64) as f32;
        }
        style
    }

    pub fn resolve_subtitle_style(
        &self,
        style: nova_config::playback_preferences::SubtitleStyle,
    ) -> nova_config::playback_preferences::SubtitleStyle {
        style.resolved(self.state.native_subtitle_defaults.style)
    }

    pub fn set_title_subtitle_style_value(
        &self,
        index: i32,
        value: f32,
    ) -> nova_config::playback_preferences::SubtitleStylePatch {
        let mpv = self.state.mpv.lock().unwrap();
        let mut session = self.state.preferences.lock().unwrap();
        let patch = session
            .preferences
            .title
            .subtitle_style
            .unwrap_or_default()
            .with_value(index, value);
        session.preferences.title.subtitle_style = Some(patch);
        if let Some(mpv) = mpv.as_ref() {
            if index == 0 {
                self.state
                    .subtitle_sync
                    .lock()
                    .unwrap()
                    .reset_timing(mpv, true);
            }
            let style = session.preferences.subtitle_style();
            self.state.apply_subtitle_style(mpv, style);
        }
        patch
    }

    pub fn set_title_speed(&self, speed: f32) {
        self.state
            .preferences
            .lock()
            .unwrap()
            .preferences
            .title
            .speed = Some(speed);
        self.set_speed(speed);
    }

    /// Local global edits affect only fields still inheriting in this session.
    pub fn update_playback_defaults(&self, settings: &nova_config::CacheSettings) {
        #[cfg(target_os = "android")]
        self.set_android_auto_rotate(settings.android_auto_rotate);
        #[cfg(target_os = "android")]
        if !settings.android_pip_enabled {
            // Apply the device preference at capture time, before the next
            // timer tick or Home gesture. Android has no public exit-PiP API;
            // leave an existing window video-only and paused until expanded.
            external::update_picture_in_picture(false, true, 16, 9);
            if self
                .state
                .app
                .upgrade()
                .is_some_and(|app| app.get_android_pip_active())
            {
                self.set_paused(true);
            }
        }
        let inherited = {
            let session = self.state.preferences.lock().unwrap();
            [
                session.preferences.title.audio.is_none()
                    && session.preferences.audio_language != settings.default_audio_language,
                session.preferences.title.subtitles.is_none()
                    && session.preferences.subtitle_language != settings.default_subtitle_language,
                session.preferences.title.speed.is_none()
                    && session.preferences.default_speed != settings.playback_speed,
            ]
        };
        for (control, inherits) in inherited.into_iter().enumerate() {
            if inherits {
                self.restore_playback_default(control as i32, settings);
            }
        }
        let mpv = self.state.mpv.lock().unwrap();
        let mut session = self.state.preferences.lock().unwrap();
        if session.preferences.default_subtitle_style != settings.subtitle_style.normalized() {
            let old_delay = session.preferences.subtitle_style().delay;
            session.preferences.default_subtitle_style = settings.subtitle_style.normalized();
            if let Some(mpv) = mpv.as_ref() {
                let style = session.preferences.subtitle_style();
                if old_delay != style.delay {
                    self.state
                        .subtitle_sync
                        .lock()
                        .unwrap()
                        .reset_timing(mpv, true);
                }
                self.state.apply_subtitle_style(mpv, style);
            }
        }
    }

    fn resolve_preferred_tracks(&self) {
        if self.resolve_preferred_tracks_inner() {
            self.refresh_tracks();
        }
    }

    fn resolve_preferred_tracks_inner(&self) -> bool {
        // A queued load still exposes the previous file's tracks.
        if self.state.pending.lock().unwrap().is_some() {
            return false;
        }
        let mpv = self.state.mpv.lock().unwrap();
        let Some(mpv) = mpv.as_ref() else {
            return false;
        };
        if mpv.get_property::<String>("path").ok().as_deref()
            != Some(self.state.preference_source.lock().unwrap().as_str())
        {
            return false;
        }
        #[cfg(target_os = "android")]
        self.restore_online_subtitle(mpv);
        let Ok(raw) = self.state.track_inventory(mpv) else {
            return false;
        };
        let mut session = self.state.preferences.lock().unwrap();
        if raw == session.tracks {
            return false;
        }
        session.tracks = raw.clone();
        self.state
            .subtitle_sync
            .lock()
            .unwrap()
            .update_inventory(&raw);
        let tracks = track_preferences::parse_tracks(&raw);
        if tracks.is_empty() {
            return false;
        }
        for audio in [true, false] {
            if if audio {
                session.manual_audio
            } else {
                session.manual_subtitles
            } {
                continue;
            }
            let (saved, global) = if audio {
                (
                    session.preferences.title.audio.as_ref(),
                    session.preferences.audio_language.as_str(),
                )
            } else {
                (
                    session.preferences.title.subtitles.as_ref(),
                    session.preferences.subtitle_language.as_str(),
                )
            };
            if let Some(id) = select_track(&tracks, audio, saved, global) {
                if !audio
                    && mpv.get_property::<String>("sid").ok().as_deref()
                        != Some(id.to_string().as_str())
                {
                    self.state
                        .subtitle_sync
                        .lock()
                        .unwrap()
                        .release_reference(mpv);
                }
                let _ = mpv.set_property(
                    if audio { "aid" } else { "sid" },
                    if id < 0 {
                        "no".to_owned()
                    } else {
                        id.to_string()
                    },
                );
            } else if saved.is_some() || !global.is_empty() {
                // Clear a failed load-time language hint before Automatic;
                // otherwise a saved kind that is missing could pick commentary.
                let _ = mpv.set_property(if audio { "alang" } else { "slang" }, "");
                if !audio {
                    self.state
                        .subtitle_sync
                        .lock()
                        .unwrap()
                        .release_reference(mpv);
                    let _ = mpv.set_property("subs-fallback", "default");
                }
                let _ = mpv.set_property(if audio { "aid" } else { "sid" }, "auto");
            }
        }
        self.state
            .subtitle_sync
            .lock()
            .unwrap()
            .primary_selection_changed(
                mpv,
                self.resolve_subtitle_style(session.preferences.subtitle_style())
                    .delay as f64,
            );
        self.resolve_secondary_subtitle(mpv, &tracks);
        true
    }

    /// Current-selection summary for a track list: the selected track's
    /// `lang — title`. Empty when there are no tracks or none is selected.
    fn track_selection_label(rows: &[TrackRow]) -> slint::SharedString {
        if rows.is_empty() {
            return slint::SharedString::default();
        }
        for t in rows {
            if t.selected {
                let lang = t.lang.as_str();
                let title = t.title.as_str();
                let label = if lang.is_empty() {
                    title.to_string()
                } else if title.is_empty() {
                    lang.to_string()
                } else {
                    format!("{lang} — {title}")
                };
                return slint::SharedString::from(label);
            }
        }
        slint::SharedString::default()
    }

    /// Re-read mpv's `track-list` and populate the audio/subtitle track
    /// models. Called when one of the track popups opens (and after a pick).
    pub fn refresh_tracks(&self) {
        let Some(app) = self.state.app.upgrade() else {
            return;
        };
        let Ok(mpv) = self.state.mpv.lock() else {
            return;
        };
        let Some(mpv) = mpv.as_ref() else {
            return;
        };
        if self.state.pending.lock().unwrap().is_some()
            || mpv.get_property::<String>("path").ok().as_deref()
                != Some(self.state.preference_source.lock().unwrap().as_str())
        {
            return;
        }
        // MPV_FORMAT_STRING on a node property returns its JSON form.
        let raw: String = match self.state.track_inventory(mpv) {
            Ok(s) => s,
            Err(_) => return, // no file loaded; keep whatever is shown
        };
        self.state
            .subtitle_sync
            .lock()
            .unwrap()
            .update_inventory(&raw);
        let parsed = match serde_json::from_str::<serde_json::Value>(&raw) {
            Ok(v) => v,
            Err(_) => return,
        };

        // Selection source of truth. Some libmpv builds (Android) don't keep
        // `track-list/N/selected` in sync with a just-applied `sid`/`aid`, so
        // read the selection properties directly and fall back to the
        // track-list flag when they don't name a numeric track (`no`/`auto`).
        let track_id_prop = |name: &str| -> Option<i32> {
            mpv.get_property::<String>(name).ok().and_then(|raw| {
                if raw == "no" {
                    Some(-1)
                } else {
                    raw.parse::<i32>().ok()
                }
            })
        };
        let sid = track_id_prop("sid");
        let aid = track_id_prop("aid");

        let mut audio_rows: Vec<TrackRow> = Vec::new();
        let mut sub_rows: Vec<TrackRow> = Vec::new();
        if let serde_json::Value::Array(list) = parsed {
            for t in list {
                let type_ = t.get("type").and_then(|v| v.as_str()).unwrap_or("");
                let id = t.get("id").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                let mut lang = t
                    .get("lang")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let mut title = t
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let codec = t
                    .get("codec")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if t.get("nova-source-subtitle").and_then(|v| v.as_bool()) == Some(true) {
                    let included = nova_ui::backend_text::tr("Included with stream");
                    title = if title.is_empty() {
                        included.into()
                    } else {
                        format!("{title} · {included}")
                    };
                }
                let listed_selected = t.get("selected").and_then(|v| v.as_bool()).unwrap_or(false);
                let selected = match type_ {
                    "audio" => aid.map_or(listed_selected, |a| a == id),
                    "sub" => sid.map_or(listed_selected, |s| s == id),
                    _ => listed_selected,
                };
                // The popup renders `lang — title`; avoid a leading dash by
                // shifting a lone title into the label slot.
                if lang.is_empty() && title.is_empty() {
                    lang = format!("Track {id}");
                } else if lang.is_empty() {
                    std::mem::swap(&mut lang, &mut title);
                }
                let row = TrackRow {
                    index: id,
                    lang: lang.into(),
                    title: title.into(),
                    codec: codec.into(),
                    selected,
                };
                match type_ {
                    "audio" => audio_rows.push(row),
                    "sub" => sub_rows.push(row),
                    _ => {}
                }
            }
        }

        for (index, subtitle) in self
            .state
            .online_subtitles
            .lock()
            .unwrap()
            .iter()
            .take(200)
            .enumerate()
        {
            if !sub_rows.iter().any(|row| {
                row.title.as_str() == subtitle.title && row.lang.as_str() == subtitle.language
            }) {
                sub_rows.push(TrackRow {
                    index: -2 - index as i32,
                    lang: subtitle.language.clone().into(),
                    title: subtitle.title.clone().into(),
                    codec: Default::default(),
                    selected: false,
                });
            }
        }
        let sub_label = Self::track_selection_label(&sub_rows);
        let audio_label = Self::track_selection_label(&audio_rows);
        app.set_audio_tracks(Rc::new(VecModel::from(audio_rows)).into());
        let secondary = if matches!(
            &*self.state.secondary_subtitles.lock().unwrap(),
            TrackChoice::Off
        ) {
            -1 // Automatic alignment may own a deliberately invisible reference.
        } else {
            track_id_prop("secondary-sid").unwrap_or(-1)
        };
        let secondary_rows = sub_rows
            .iter()
            .filter(|row| row.index != -1 && Some(row.index) != sid)
            .cloned()
            .map(|mut row| {
                row.selected = row.index == secondary;
                row
            })
            .collect::<Vec<_>>();
        app.set_secondary_sub_track_label(Self::track_selection_label(&secondary_rows));
        app.set_secondary_sub_tracks(Rc::new(VecModel::from(secondary_rows)).into());
        app.set_sub_tracks(Rc::new(VecModel::from(sub_rows)).into());
        app.set_sub_track_label(sub_label);
        app.set_audio_track_label(audio_label);
    }

    /// Consume the distinction between finishing playback and handing its
    /// still-live source to an external player (Android decoder fallback).
    pub fn take_external_handoff(&self) -> bool {
        self.state.external_handoff.swap(false, Ordering::SeqCst)
    }

    /// Open the normal loading overlay before an addon has supplied a URL.
    /// No loadfile is queued and the decoder's timeout starts only in `play`.
    pub fn wait_for_source(&self, status: &str) -> bool {
        let Some(app) = self.state.app.upgrade() else {
            return false;
        };
        #[cfg(not(target_os = "android"))]
        let fullscreen = app.window().is_fullscreen();
        #[cfg(target_os = "android")]
        let fullscreen = app.get_is_fullscreen() || app.window().is_fullscreen();
        self.with_preserved_fullscreen_on_close(|| self.close());
        self.state.source_waiting.store(true, Ordering::SeqCst);
        if fullscreen {
            app.window().set_fullscreen(true);
            app.set_is_fullscreen(true);
        }
        #[cfg(not(target_os = "android"))]
        if !fullscreen {
            self.restore_playback_fullscreen(&app);
        }
        #[cfg(target_os = "android")]
        {
            external::set_playback_active(
                true,
                nova_config::active_cache_settings().android_auto_rotate,
            );
            let _ = set_system_bars_hidden(true);
            app.set_is_fullscreen(true);
            converge_screen_on(ANDROID_FOREGROUND.load(Ordering::SeqCst));
        }
        #[cfg(target_os = "linux")]
        converge_idle_inhibit(true);
        app.set_player_status_is_error(false);
        app.set_player_status(nova_ui::backend_text::tr(status).into());
        app.set_osd_visible(true);
        app.set_player_open(true);
        app.window().request_redraw();
        true
    }

    /// Abandon a failed held load without releasing any media into playback.
    fn abandon_failed_start_pause(&self) {
        if !self.state.start_pause_hold.load(Ordering::SeqCst) {
            return;
        }
        *self.state.pending.lock().unwrap() = None;
        if let Ok(guard) = self.state.mpv.lock()
            && let Some(mpv) = guard.as_ref()
        {
            let _ = mpv.command("stop", &[]);
        }
        self.state.source_waiting.store(true, Ordering::SeqCst);
        self.state.pending_paused.store(false, Ordering::SeqCst);
        self.state.start_pause_hold.store(false, Ordering::SeqCst);
    }

    /// Resolver and decoder startup failures remain actionable until Close.
    pub fn playback_failed(&self) -> bool {
        self.state.reported.load(Ordering::SeqCst) && !self.state.started.load(Ordering::SeqCst)
    }

    /// Keep a resolver error readable until Close without probing an old mpv file.
    pub fn source_failed(&self, status: &str) {
        if !self.state.source_waiting.load(Ordering::SeqCst) {
            return;
        }
        self.abandon_failed_start_pause();
        self.state.reported.store(true, Ordering::SeqCst);
        if let Some(app) = self.state.app.upgrade() {
            app.set_player_status_is_error(true);
            app.set_player_status(nova_ui::backend_text::tr(status).into());
            app.set_osd_visible(true);
            app.window().request_redraw();
        }
        #[cfg(target_os = "linux")]
        converge_idle_inhibit(false);
        #[cfg(target_os = "android")]
        converge_screen_on(false);
    }

    /// UI thread: reserve the OSD's current logical height independently of
    /// pixel density. Actual subtitle updates stay beside mpv rendering.
    pub fn set_subtitle_clearance(&self, fraction: f32, opacity: f32) {
        let (fraction, opacity) = if subtitle_position::OSD_LIFT_ENABLED {
            (fraction, opacity)
        } else {
            (0.0, 0.0)
        };
        self.state
            .subtitle_position
            .lock()
            .unwrap()
            .set_clearance(fraction, opacity);
        // The final fade callback can arrive after BeforeRendering. A paused
        // video needs one more frame to restore its exact base position.
        if let Some(app) = self.state.app.upgrade() {
            app.window().request_redraw();
        }
    }

    /// Run the ordinary authorized close callback during a source/episode
    /// handoff without briefly restoring the desktop catalog window. Cleanup
    /// and callback authority stay unchanged; even a panic releases this scope.
    pub fn with_preserved_fullscreen_on_close(&self, action: impl FnOnce()) {
        #[cfg(not(target_os = "android"))]
        {
            struct Restore<'a>(&'a AtomicBool, bool);
            impl Drop for Restore<'_> {
                fn drop(&mut self) {
                    self.0.store(self.1, Ordering::SeqCst);
                }
            }
            let flag = &self.state.preserve_close_window;
            let _restore = Restore(flag, flag.swap(true, Ordering::SeqCst));
            action();
        }
        #[cfg(target_os = "android")]
        action();
    }

    /// Back/Stop from a handoff picker returns the retained desktop window
    /// to the catalog. Nested normal-close cleanup inside the scope is ignored.
    pub fn release_window_transition(&self) {
        #[cfg(not(target_os = "android"))]
        if !self.state.preserve_close_window.load(Ordering::SeqCst)
            && self.state.transition_window_held.load(Ordering::SeqCst)
            && self
                .state
                .app
                .upgrade()
                .is_some_and(|app| !app.get_player_open())
        {
            self.close();
        }
    }

    /// Stop playback and return to the catalog.
    pub fn close(&self) {
        #[cfg(target_os = "android")]
        external::update_picture_in_picture(false, true, 16, 9);
        // Capture before leaving fullscreen. Closing an already-closed player
        // must not replace the saved choice with the catalog's window state.
        #[cfg(not(target_os = "android"))]
        let preserve_window = self.state.preserve_close_window.load(Ordering::SeqCst);
        #[cfg(target_os = "android")]
        let preserve_window = false;
        #[cfg(not(target_os = "android"))]
        let held_window = if preserve_window {
            self.state.transition_window_held.load(Ordering::SeqCst)
        } else {
            self.state
                .transition_window_held
                .swap(false, Ordering::SeqCst)
        };
        #[cfg(not(target_os = "android"))]
        let catalog_maximized = self
            .state
            .app
            .upgrade()
            .filter(|app| app.get_player_open() || held_window)
            .map(|app| {
                self.state
                    .playback_fullscreen
                    .store(app.window().is_fullscreen(), Ordering::SeqCst);
                if preserve_window {
                    self.state
                        .transition_window_held
                        .store(true, Ordering::SeqCst);
                }
                self.state.catalog_maximized.load(Ordering::SeqCst)
            });
        self.state.source_waiting.store(false, Ordering::SeqCst);
        self.state.start_pause_hold.store(false, Ordering::SeqCst);
        self.state.pending_paused.store(false, Ordering::SeqCst);
        #[cfg(target_os = "android")]
        self.state.loaded_online_subtitles.lock().unwrap().clear();
        {
            let mpv = self.state.mpv.lock().unwrap();
            self.state.subtitle_sync.lock().unwrap().clear(mpv.as_ref());
            self.state.screenshot_session.fetch_add(1, Ordering::SeqCst);
            self.state.started.store(false, Ordering::SeqCst);
        }
        self.state.external_handoff.store(false, Ordering::SeqCst);
        if let Ok(mpv) = self.state.mpv.lock()
            && let Some(mpv) = mpv.as_ref()
        {
            self.state.subtitle_position.lock().unwrap().restore(mpv);
            let _ = mpv.command("stop", &[]);
        }
        *self.state.pending.lock().unwrap() = None;
        self.state.preference_source.lock().unwrap().clear();
        self.state.source_subtitles.lock().unwrap().clear();
        self.state.pending_headers.lock().unwrap().clear();
        self.state.pending_subtitles.lock().unwrap().clear();
        #[cfg(target_os = "android")]
        {
            self.state.recovery.lock().unwrap().close();
            *self.state.pending_start.lock().unwrap() = None;
            self.state.pending_paused.store(false, Ordering::SeqCst);
            *self.state.last_url.lock().unwrap() = None;
            self.state.last_headers.lock().unwrap().clear();
            self.state.last_subtitles.lock().unwrap().clear();
            *self.state.session_hwdec.lock().unwrap() = None;
        }
        self.state.started.store(false, Ordering::SeqCst);
        self.state.buffering.store(false, Ordering::SeqCst);
        self.state.seek_pending.store(false, Ordering::SeqCst);
        *self.state.seek_deadline.lock().unwrap() = None;
        *self.state.seek_buffer.lock().unwrap() = None;
        self.state.reported.store(false, Ordering::SeqCst);

        let processing_status = {
            let mut processing = self.state.audio_processing.lock().unwrap();
            processing.reset();
            processing.status.clone()
        };
        if let Some(app) = self.state.app.upgrade() {
            app.set_audio_processing_status(processing_status.into());
            app.set_player_open(false);
            app.set_player_chapters(Default::default());
            app.set_screenshot_busy(false);
            app.set_screenshot_notice(slint::SharedString::default());
            app.set_player_title(slint::SharedString::default());
            app.set_player_show_title(slint::SharedString::default());
            app.set_player_episode_title(slint::SharedString::default());
            app.set_player_media_summary(Default::default());
            app.set_player_badges(Default::default());
            app.set_playback_started(false);
            app.set_position(0.0);
            app.set_duration(0.0);
            app.set_buffered_until(0.0);
            app.set_is_paused(true);
            app.set_time_text(slint::SharedString::from("00:00 / 00:00"));
            app.set_player_status_is_error(false);
            app.set_player_status(slint::SharedString::default());
            app.set_decode_label(slint::SharedString::default());
            app.set_decoder_selected(-1);
            app.set_sub_track_label(slint::SharedString::default());
            app.set_audio_track_label(slint::SharedString::default());
            // Playback fullscreen belongs to this overlay on every platform.
            // Returning to the catalog also restores Android safe-area insets.
            if !preserve_window && (app.get_is_fullscreen() || app.window().is_fullscreen()) {
                app.window().set_fullscreen(false);
                app.set_is_fullscreen(false);
            }
            #[cfg(not(target_os = "android"))]
            if !preserve_window && let Some(maximized) = catalog_maximized {
                app.window().set_maximized(maximized);
            }
            app.window().request_redraw();
        }

        // Android: request the bars back; `tick` keeps retrying until the
        // system confirms it (a `show` can fail or race the overlay teardown).
        #[cfg(target_os = "android")]
        let _ = set_system_bars_hidden(false);

        // Android: clear the content frame-rate request so the catalog (and the
        // rest of the system) goes back to the display's normal maximum.
        #[cfg(target_os = "android")]
        {
            set_content_frame_rate(None);
            *self.state.content_fps.lock().unwrap() = None;
            *self.state.applied_frame_rate.lock().unwrap() = Some(None);
        }

        // Screens may dim again: playback is over on every platform.
        #[cfg(target_os = "linux")]
        converge_idle_inhibit(false);
        #[cfg(target_os = "android")]
        {
            converge_screen_on(false);
            // Decoder teardown may block. Publish rotation only after it so a
            // following wait_for_source can coalesce the close/open requests.
            external::set_playback_active(
                false,
                nova_config::active_cache_settings().android_auto_rotate,
            );
        }
    }

    /// Mirror mpv state into the overlay (called every 250 ms while the
    /// overlay is up); also detects end-of-stream and load failures.
    ///
    /// mpv is read once, in a scoped block. Everything below can end playback —
    /// [`Self::close`], the Android hand-off, the copy-back retry — and each of
    /// those locks the same mpv mutex, so no guard may be held across them:
    /// `std::sync::Mutex` is not reentrant, and re-locking it on the UI thread
    /// would hang the app.
    pub fn tick(&self) {
        if self.state.enforce_sleep_deadline() {
            return;
        }
        let Some(app) = self.state.app.upgrade() else {
            return;
        };

        // Android: converge the system bars towards the last requested state
        // *before* the early-return, so a `show` that failed while the overlay
        // was torn down keeps retrying after `player_open` goes false — that is
        // what stops the bars from staying hidden once the player closes. While
        // the overlay is up this re-asserts a hidden state at a slow cadence
        // (see `reassert_system_bars`), not on every tick.
        #[cfg(target_os = "android")]
        let _ = reassert_system_bars();

        if !app.get_player_open() || self.state.source_waiting.load(Ordering::SeqCst) {
            return;
        }

        self.load_source_subtitles();
        self.resolve_preferred_tracks();

        self.poll_subtitle_sync();
        self.poll_secondary_subtitle_position();

        // Sample actual starvation on the regular native-player cadence even
        // when party telemetry is sent less often or a UI seek skips mirroring.
        let _ = self.playback_readiness();

        /// One tick's worth of mpv state.
        struct Snapshot {
            pos: f64,
            #[cfg(target_os = "android")]
            live_pos: Option<f64>,
            #[cfg(target_os = "android")]
            live_paused: Option<bool>,
            dur: f64,
            buffered_until: f32,
            media_badges: Option<Vec<slint::SharedString>>,
            chapters: Option<Vec<chapters::Chapter>>,
            paused: bool,
            vol: f64,
            muted: bool,
            eof: bool,
            /// The UI position (slider/seek) is more than 2 s from mpv's
            /// `time-pos`. Checked after the Android resume-reload so a
            /// rebuilt render context blanking `time-pos` cannot strand it.
            diverged: bool,
            /// Actual decoder for platforms with a runtime picker.
            #[cfg(any(target_os = "android", target_os = "windows", target_os = "linux"))]
            hwdec_current: String,
            /// Android: the container's nominal frame rate (mpv `container-fps`).
            #[cfg(target_os = "android")]
            container_fps: f64,
        }

        // Android: `hwdec-current` is only meaningful once the decoder is open,
        // so the one-shot engagement check is armed here and only consumed on
        // the first tick that shows progress. The value itself is read every
        // tick, so the overlay badge tracks a decoder that changes mid-session
        // (e.g. the direct→copy-back downgrade below, or a runtime fallback).
        #[cfg(target_os = "android")]
        let probe_hwdec = !self.state.hwdec_checked.load(Ordering::SeqCst);

        // Rendering consumes the pending load before locking mpv. Snapshot
        // ownership first so cache polling cannot invert that lock order.
        let pending_load = self.state.pending.lock().unwrap().is_some();
        let buffer_source = self.state.preference_source.lock().unwrap().clone();
        let snapshot = {
            let Ok(mpv) = self.state.mpv.lock() else {
                return;
            };
            let Some(mpv) = mpv.as_ref() else {
                return;
            };

            if !pending_load {
                let mut processing = self.state.audio_processing.lock().unwrap();
                processing.poll(mpv);
                app.set_audio_processing_status(processing.status.clone().into());
            }
            let live_pos: Option<f64> = mpv.get_property("time-pos").ok();
            let live_paused: Option<bool> = mpv.get_property("pause").ok();
            let pos = live_pos.unwrap_or(0.0);
            let dur: f64 = mpv.get_property("duration").unwrap_or(0.0);
            let paused = live_paused.unwrap_or(false);
            let seek_pending = self.state.seek_pending.load(Ordering::SeqCst);
            let diverged = seek_pending
                && self
                    .state
                    .seek_deadline
                    .lock()
                    .unwrap()
                    .is_some_and(|deadline| Instant::now() < deadline)
                && mpv.get_property::<bool>("seekable").unwrap_or(false)
                && (app.get_position() as f64 - pos).abs() > 2.0;
            // A queued replacement must not inherit the previous file's
            // cache. The same source ownership check guards track preferences.
            let buffered_until = if !pending_load
                && mpv.get_property::<String>("path").ok().as_deref()
                    == Some(buffer_source.as_str())
            {
                let fresh = estimated_buffered_until(
                    pos,
                    dur,
                    mpv.get_property("demuxer-cache-time").ok(),
                    mpv.get_property("demuxer-cache-duration").ok(),
                );
                let mut grace = self.state.seek_buffer.lock().unwrap();
                if fresh > 0.0 && !diverged {
                    *grace = None;
                    fresh
                } else {
                    let display_position = if diverged {
                        app.get_position() as f64
                    } else {
                        pos
                    };
                    let display_duration = if dur.is_finite() && dur > 0.0 {
                        dur
                    } else {
                        app.get_duration() as f64
                    };
                    let retained = (*grace).and_then(|grace| {
                        grace.visible(
                            self.state.screenshot_session.load(Ordering::SeqCst),
                            Instant::now(),
                            display_position,
                            display_duration,
                        )
                    });
                    if retained.is_none() {
                        *grace = None;
                    }
                    retained.unwrap_or(fresh)
                }
            } else {
                *self.state.seek_buffer.lock().unwrap() = None;
                0.0
            };

            // After a released OSD scrub commits a seek, until mpv actually
            // applies it, the slider value diverges from mpv's time-pos.
            // Don't overwrite the committed target with the stale position
            // or the slider would fall back to its previous state. The held
            // preview is UI-local and does not arm this guard. Resume as soon as
            // mpv's time-pos catches up with the slider. Only a seek the UI
            // actually issued arms this (`seek_pending`), so an initial resume
            // opened at the saved position (UI still at 0) mirrors mpv instead
            // of being frozen here. Handled *after* the Android resume-reload
            // below, because freeing/recreating the mpv render context
            // (surface recreation) briefly blanks `time-pos`, which would
            // otherwise trip this guard and strand the reload.
            if seek_pending && !diverged {
                // mpv caught up with the seek: disengage the guard.
                self.state.seek_pending.store(false, Ordering::SeqCst);
                *self.state.seek_deadline.lock().unwrap() = None;
            }

            Snapshot {
                pos,
                #[cfg(target_os = "android")]
                live_pos,
                #[cfg(target_os = "android")]
                live_paused,
                dur,
                buffered_until,
                // Hidden controls need no repeated metadata reads. A pending
                // replacement must never display the previous file's badges.
                media_badges: (!pending_load
                    && app.get_osd_visible()
                    && mpv.get_property::<String>("path").ok().as_deref()
                        == Some(buffer_source.as_str()))
                .then(|| media_badges::read(mpv)),
                chapters: (!pending_load
                    && app.get_osd_visible()
                    && mpv.get_property::<String>("path").ok().as_deref()
                        == Some(buffer_source.as_str()))
                .then(|| chapters::read(mpv, dur)),
                paused,
                // Mirror volume/mute (cheap reads; keeps UI in sync if changed
                // via keyboard shortcuts or external controls).
                vol: mpv.get_property("volume").unwrap_or(100.0),
                muted: mpv.get_property("mute").unwrap_or(false),
                eof: mpv.get_property("eof-reached").unwrap_or(false),
                diverged,
                #[cfg(any(target_os = "android", target_os = "windows", target_os = "linux"))]
                hwdec_current: mpv.get_property("hwdec-current").unwrap_or_default(),
                #[cfg(target_os = "android")]
                container_fps: mpv.get_property("container-fps").unwrap_or(0.0),
            }
        };

        if let Some(chapters) = snapshot.chapters {
            let rows: Vec<nova_ui::ChapterRow> = chapters
                .into_iter()
                .map(|chapter| nova_ui::ChapterRow {
                    position: chapter.position,
                    title: chapter.title.into(),
                })
                .collect();
            let current = app.get_player_chapters();
            if current.row_count() != rows.len()
                || rows
                    .iter()
                    .enumerate()
                    .any(|(i, row)| current.row_data(i).as_ref() != Some(row))
            {
                app.set_player_chapters(Rc::new(VecModel::from(rows)).into());
            }
        }

        // The request belongs to the session, not its URL or a transient
        // started/time-pos flag. Resume and renderer setup can arrive in either
        // order; neither consumes it until both foreground and renderer ready.
        #[cfg(target_os = "android")]
        {
            let mut recovery = self.state.recovery.lock().unwrap();
            recovery.observe(
                if snapshot.diverged {
                    None
                } else {
                    snapshot.live_pos
                },
                snapshot.live_paused,
            );
            let reload = recovery.take_reload();
            let waiting = recovery.waiting();
            drop(recovery);
            if let Some(reload) = reload {
                alog(&format!(
                    "recovery scheduled: session={} at={:.3}s paused={}",
                    reload.session, reload.position, reload.paused
                ));
                if self.reload_current_url(Some(reload.position)) {
                    self.state
                        .pending_paused
                        .store(reload.paused, Ordering::SeqCst);
                } else {
                    self.state.recovery.lock().unwrap().load_failed();
                    alog("recovery scheduling failed: no active source");
                }
                return; // Do not mirror the old VO or run decoder fallback.
            }
            if waiting {
                return; // Preserve UI state and don't time out while backgrounded.
            }
        }

        // An optimistic seek holds only the displayed position. Pause, EOF,
        // decoder and startup-error convergence must continue while it lands.
        // Linux: converge screen inhibition on live state (self-heals EOF
        // and any pause change that bypassed toggle()). Transition-only
        // internally, so steady ticks cost just a mutex lock.
        #[cfg(target_os = "linux")]
        converge_idle_inhibit(!snapshot.paused && !snapshot.eof);
        // Android: same convergence through the window flag (self-heals EOF
        // and out-of-band pauses; transitions marshal one JNI call).
        #[cfg(target_os = "android")]
        converge_screen_on(!snapshot.paused && !snapshot.eof);

        // Android: survey the active decoder on every tick while the overlay is
        // up and print it to logcat (`adb logcat -s nova-player`) whenever it
        // changes — lets us correlate a playback glitch with a decoder switch.
        #[cfg(target_os = "android")]
        self.survey_hwdec(snapshot.hwdec_current.as_str());

        // Android: mirror the live decoder into the overlay on every tick — even
        // before frames progress or while paused — so the settings modal's
        // decoder submenu (and the value it shows) always reflects what mpv is
        // actually running. `survey_hwdec` above only logs the changes.
        #[cfg(target_os = "android")]
        if !snapshot.hwdec_current.is_empty() {
            app.set_decode_label(slint::SharedString::from(decode_label(
                snapshot.hwdec_current.as_str(),
            )));
            app.set_decoder_selected(decode_index(snapshot.hwdec_current.as_str()));
        }

        #[cfg(any(target_os = "windows", target_os = "linux"))]
        if let Some(active) = desktop_decoder::active(&snapshot.hwdec_current) {
            app.set_decode_label(active.label().into());
            app.set_decoder_selected(active.index());
        }

        // Android: drive the display refresh rate from the OSD state and the
        // content's frame rate.
        #[cfg(target_os = "android")]
        self.sync_frame_rate(&app, snapshot.container_fps);

        if let Some(badges) = snapshot.media_badges {
            let current = app.get_player_badges();
            if current.row_count() != badges.len()
                || badges
                    .iter()
                    .enumerate()
                    .any(|(i, badge)| current.row_data(i).as_ref() != Some(badge))
            {
                app.set_player_badges(Rc::new(VecModel::from(badges)).into());
            }
        }
        let was_paused = app.get_is_paused();
        if snapshot.dur > 0.0 || snapshot.pos > 0.0 {
            // The startup wake chooses a playing/paused timeout. Publish the
            // live pause state first, rather than retaining loading's `true`.
            app.set_is_paused(snapshot.paused);
            // Frames are progressing: reveal the video (transparent window
            // background) and drop the loading status.
            if !self.state.started.swap(true, Ordering::SeqCst) {
                app.set_player_status_is_error(false);
                app.set_player_status(slint::SharedString::default());
                // Arm the OSD auto-hide countdown as playback begins, so
                // the controls fade even if the mouse never moves.
                app.invoke_osd_mouse_moved();
            }
            app.set_playback_started(true);
            if !snapshot.diverged {
                app.set_position(snapshot.pos as f32);
            }
            app.set_duration(snapshot.dur.max(1.0) as f32);
            app.set_buffered_until(snapshot.buffered_until);
            // OSD visibility is owned solely by the auto-hide countdown in
            // `run.rs` (Android keeps paused controls visible; desktop can
            // fade them after 5 s): do not override that policy here.
            app.set_time_text(slint::SharedString::from(format!(
                "{} / {}",
                format_time(if snapshot.diverged {
                    app.get_position() as f64
                } else {
                    snapshot.pos
                }),
                format_time(snapshot.dur)
            )));

            // Android: confirm the decoder we asked for actually engaged. If
            // mpv fell back on its own (e.g. direct MediaCodec refused), settle
            // the chain on what really runs so a later timeout does not retry
            // it. The label itself is kept fresh every tick above; the empty
            // check here avoids settling during the brief window where
            // `hwdec-current` is unset (a runtime switch, or the render-context
            // teardown/setup an orientation change triggers).
            #[cfg(target_os = "android")]
            if !snapshot.hwdec_current.is_empty() && probe_hwdec {
                self.state.hwdec_checked.store(true, Ordering::SeqCst);
                let current = snapshot.hwdec_current.as_str();
                self.settle_decoder(current);
                alog(&format!(
                    "hwdec: engaged {current:?} ({})",
                    decode_label(current)
                ));
            }
        } else if !self.state.reported.load(Ordering::SeqCst)
            && self.state.opened_at.lock().unwrap().elapsed() > Duration::from_secs(20)
            && !(self.state.ram_torrent_source.lock().unwrap().as_deref()
                == Some(buffer_source.as_str())
                && self
                    .state
                    .ram_buffer_progress
                    .lock()
                    .unwrap()
                    .recent(Instant::now()))
        {
            // Android: the requested decoder produced no frames at all — step
            // down the fallback chain and reload the stream with the next one
            // before reporting a failure.
            #[cfg(target_os = "android")]
            if self.advance_decoder(true) {
                return;
            }

            // mpv never produced a frame: report instead of "Loading…" forever
            // (the overlay stays up so the message is readable and Close
            // works) — or, on Android, hand the stream to a system player
            // instead of leaving an unplayable overlay behind.
            if !self.state.reported.swap(true, Ordering::SeqCst) {
                #[cfg(target_os = "android")]
                if self.hand_off_to_external_player() {
                    return;
                }
                self.abandon_failed_start_pause();
                app.set_is_paused(true);
                app.set_player_status_is_error(true);
                app.set_player_status(slint::SharedString::from(
                    "Cannot play this stream (mpv could not start playback).",
                ));
            }
        }

        app.set_volume(snapshot.vol as f32);
        app.set_muted(snapshot.muted);

        // Retain the final frame and timeline at EOF. A terminal pause avoids
        // rewriting identical progress every save interval and lets a seek
        // back inspect a frame before the user explicitly presses Play.
        // Parties own their terminal reporting and pause/rate policy.
        if self.state.started.load(Ordering::SeqCst)
            && snapshot.eof
            && self.state.party_playback_rate.lock().unwrap().is_none()
        {
            if !snapshot.paused {
                self.set_paused(true);
            }
            app.set_is_paused(true);
            if !snapshot.paused || !was_paused {
                // Wake once on the terminal transition, after publishing the
                // pause so the OSD chooses its paused timeout.
                app.invoke_osd_mouse_moved();
            }
        }
        #[cfg(target_os = "android")]
        android_pip::publish(&self.state, &app, snapshot.eof);
    }

    /// Android: last resort for a stream the in-app player cannot start — hand
    /// it to a system video player (`ACTION_VIEW`, the path this app used before
    /// it had in-app playback) and leave the overlay. Returns `false` when no
    /// external player took it, in which case the caller keeps its on-screen
    /// error. Must not be called while the mpv lock is held ([`Self::close`]
    /// takes it).
    #[cfg(target_os = "android")]
    fn hand_off_to_external_player(&self) -> bool {
        if !self.state.allow_external.load(Ordering::SeqCst)
            // Existing playback can enter a party after its source policy was
            // captured. The live coordinator still requires the native player.
            || self.state.party_playback_rate.lock().unwrap().is_some()
            || !self.state.last_headers.lock().unwrap().is_empty()
            || !self.state.last_subtitles.lock().unwrap().is_empty()
            || !self.state.source_subtitles.lock().unwrap().is_empty()
        {
            return false;
        }
        let Some(url) = self.state.last_url.lock().unwrap().clone() else {
            return false;
        };
        match open_external(&url) {
            Ok(()) => {
                alog("no in-app playback: handed the stream to an external player");
                self.close();
                self.state.external_handoff.store(true, Ordering::SeqCst);
                true
            }
            Err(e) => {
                alog(&format!("external-player fallback failed: {e}"));
                false
            }
        }
    }

    /// Android: build this session's decoder fallback chain and request its
    /// first entry. The preference is the runtime override set from the
    /// player overlay's picker when present, else the user's Settings →
    /// Player preference. The chain is `HW+ → HW → SW`, `HW → SW`, or just
    /// `SW`; when direct MediaCodec is unavailable (VM not registered) or
    /// previously failed, `HW+` is skipped. Changing the preference clears
    /// the sticky failure so HW+ is retried.
    #[cfg(target_os = "android")]
    fn arm_decoder_chain(&self) {
        let pref = self
            .state
            .session_hwdec
            .lock()
            .unwrap()
            .unwrap_or_else(|| nova_config::active_cache_settings().android_hwdec);
        {
            let mut last = self.state.last_hwdec_pref.lock().unwrap();
            if *last != Some(pref) {
                *last = Some(pref);
                self.state.direct_failed.store(false, Ordering::SeqCst);
            }
        }
        let direct_ok = self.state.direct_available.load(Ordering::SeqCst)
            && !self.state.direct_failed.load(Ordering::SeqCst);
        let chain: Vec<&'static str> = match pref {
            nova_config::AndroidHwdec::HwPlus if direct_ok => {
                vec![HWDEC_PREFERRED, HWDEC_FALLBACK, HWDEC_SOFTWARE]
            }
            nova_config::AndroidHwdec::HwPlus | nova_config::AndroidHwdec::Hw => {
                vec![HWDEC_FALLBACK, HWDEC_SOFTWARE]
            }
            nova_config::AndroidHwdec::Sw => vec![HWDEC_SOFTWARE],
        };
        let first = chain[0];
        if let Ok(mpv) = self.state.mpv.lock() {
            if let Some(mpv) = mpv.as_ref() {
                let _ = mpv.set_property("hwdec", first);
            }
        }
        alog(&format!("hwdec: preference={pref:?}, chain={chain:?}"));
        *self.state.decoder_chain.lock().unwrap() = chain;
        self.state.decoder_idx.store(0, Ordering::SeqCst);
    }

    /// Android: settle the chain on the decoder mpv actually selected once
    /// frames start. If direct MediaCodec was requested but did not engage,
    /// record the sticky failure and point at the entry that is really running,
    /// so a later stall does not retry a decoder that already refused.
    #[cfg(target_os = "android")]
    fn settle_decoder(&self, current: &str) {
        let chain = self.state.decoder_chain.lock().unwrap().clone();
        let attempted_direct = chain.first() == Some(&HWDEC_PREFERRED);
        let idx = if current == HWDEC_PREFERRED {
            chain
                .iter()
                .position(|m| *m == HWDEC_PREFERRED)
                .unwrap_or(0)
        } else if current.contains("mediacodec") {
            if attempted_direct {
                self.state.direct_failed.store(true, Ordering::SeqCst);
            }
            chain.iter().position(|m| *m == HWDEC_FALLBACK).unwrap_or(0)
        } else {
            if attempted_direct {
                self.state.direct_failed.store(true, Ordering::SeqCst);
            }
            chain
                .iter()
                .position(|m| *m == HWDEC_SOFTWARE)
                .unwrap_or_else(|| chain.len().saturating_sub(1))
        };
        self.state.decoder_idx.store(idx, Ordering::SeqCst);
    }

    /// Android: step to the next decoder in the chain. With `reload` the
    /// current URL is re-armed so mpv re-opens the stream (needed when the
    /// decoder produced no frames); without it the switch applies in place.
    /// Returns `false` when the chain is exhausted. Must not be called while
    /// the mpv lock is held ([`Self::close`] takes it).
    #[cfg(target_os = "android")]
    fn advance_decoder(&self, reload: bool) -> bool {
        let (next_idx, mode) = {
            let chain = self.state.decoder_chain.lock().unwrap();
            let cur = self.state.decoder_idx.load(Ordering::SeqCst);
            if chain.get(cur) == Some(&HWDEC_PREFERRED) {
                self.state.direct_failed.store(true, Ordering::SeqCst);
            }
            let next_idx = cur + 1;
            let Some(mode) = chain.get(next_idx).copied() else {
                return false;
            };
            (next_idx, mode)
        };
        self.state.decoder_idx.store(next_idx, Ordering::SeqCst);
        // Capture the live position before `hwdec` is touched (it can briefly
        // unset `time-pos`), so a stall-triggered fallback resumes in place.
        let resume = self.current_position_secs();
        if let Ok(mpv) = self.state.mpv.lock() {
            if let Some(mpv) = mpv.as_ref() {
                let _ = mpv.set_property("hwdec", mode);
            }
        }
        alog(&format!("hwdec: falling back to {mode}"));
        if reload && !self.reload_current_url(Some(resume)) {
            return false;
        }
        true
    }

    /// Android: best guess at the live playback position, captured *before* a
    /// decoder change. Reading mpv's `time-pos` after `arm_decoder_chain` can
    /// yield 0 (the property is briefly unset while the decoder re-opens), so
    /// fall back to the overlay's last ticked position.
    #[cfg(target_os = "android")]
    fn current_position_secs(&self) -> f64 {
        let mut mpv_pos: Option<f64> = None;
        if let Ok(mpv) = self.state.mpv.lock() {
            if let Some(mpv) = mpv.as_ref() {
                mpv_pos = mpv.get_property("time-pos").ok();
            }
        }
        if let Some(pos) = mpv_pos.filter(|p| *p > 0.0) {
            return pos;
        }
        self.state
            .app
            .upgrade()
            .map(|app| app.get_position() as f64)
            .unwrap_or(0.0)
    }

    /// Android: apply a decoder picked in the player overlay's runtime menu to
    /// the current stream. Sets a per-stream override, clears the sticky direct
    /// failure (an explicit pick is a fresh try), re-arms the chain, then
    /// reloads at the current position. The persisted Settings → Player
    /// preference is deliberately left untouched.
    #[cfg(target_os = "android")]
    pub fn pick_decoder(&self, index: i32) {
        let pref = nova_config::AndroidHwdec::from_index(index);
        // Snapshot where we are *before* touching `hwdec`: changing it can
        // momentarily unset `time-pos`, and reading 0 here restarted the
        // stream from the beginning.
        let resume = self.current_position_secs();
        *self.state.session_hwdec.lock().unwrap() = Some(pref);
        // Asking for HW+ again must retry it even if it refused earlier in
        // this run; `arm_decoder_chain` then builds the matching chain.
        self.state.direct_failed.store(false, Ordering::SeqCst);
        self.arm_decoder_chain();
        // Immediate feedback: reflect the pick in the settings modal before the
        // reload produces frames. `tick` overwrites both with the decoder mpv
        // really engages, so any fallback still moves the checkmark.
        if let Some(app) = self.state.app.upgrade() {
            app.set_decode_label(slint::SharedString::from(pref.label()));
            app.set_decoder_selected(pref.index());
        }
        alog(&format!("hwdec: runtime pick -> {pref:?}"));
        // A decoder change needs a fresh open; resume at the captured spot.
        let _ = self.reload_current_url(Some(resume));
    }

    /// Android: read mpv's live `hwdec-current` and mirror it into the overlay
    /// (label + settings checkmark) immediately. [`Self::tick`] already does this
    /// every 250 ms while the player is up; this is for the settings modal
    /// opening, so it shows the decoder that is really running without waiting
    /// for the next tick.
    #[cfg(target_os = "android")]
    pub fn refresh_decoder(&self) {
        let current: String = {
            let Ok(mpv) = self.state.mpv.lock() else {
                return;
            };
            let Some(mpv) = mpv.as_ref() else {
                return;
            };
            mpv.get_property("hwdec-current").unwrap_or_default()
        };
        if current.is_empty() {
            return;
        }
        if let Some(app) = self.state.app.upgrade() {
            app.set_decode_label(slint::SharedString::from(decode_label(current.as_str())));
            app.set_decoder_selected(decode_index(current.as_str()));
        }
    }

    /// Desktop: mpv reinitializes the decoder in place, retaining the current
    /// file, position, pause state, headers and tracks. Only mpv is changed;
    /// `play` reapplies the saved preference for the next stream.
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    pub fn pick_decoder(&self, index: i32) {
        let preference = nova_config::DesktopHwdec::from_index(index).supported();
        let applied = self
            .state
            .mpv
            .lock()
            .ok()
            .and_then(|guard| {
                guard
                    .as_ref()
                    .map(|mpv| desktop_decoder::configure(mpv, preference))
            })
            .is_some_and(|result| result.is_ok());
        if applied && let Some(app) = self.state.app.upgrade() {
            app.set_decode_label(preference.label().into());
            app.set_decoder_selected(preference.index());
        }
    }

    #[cfg(any(target_os = "windows", target_os = "linux"))]
    pub fn refresh_decoder(&self) {
        let active = self.state.mpv.lock().ok().and_then(|guard| {
            let current: String = guard.as_ref()?.get_property("hwdec-current").ok()?;
            desktop_decoder::active(&current)
        });
        if let Some(active) = active
            && let Some(app) = self.state.app.upgrade()
        {
            app.set_decode_label(active.label().into());
            app.set_decoder_selected(active.index());
        }
    }

    /// Android: request a display refresh rate from the current OSD state and
    /// the stream's container frame rate.
    ///
    /// While the OSD is on screen — or fading in/out, loading, paused, or under
    /// a popup — the request is cleared (`None`) so the panel runs at its normal
    /// maximum. Only once the OSD's animated opacity has reached zero do we ask
    /// for the stream's effective rate (e.g. 23.976 → the closest display mode),
    /// which is where battery and judder are won for 24 fps content.
    ///
    /// The effective rate follows the playback rate: at 2× a 24 fps stream
    /// presents 48 frames/s, so the hint is `container-fps × speed` — matching
    /// the nominal container rate while sped up would hold the display at the
    /// wrong mode and judder. The tick re-evaluates this every 250 ms, so a
    /// rate change takes effect without an extra hook.
    #[cfg(target_os = "android")]
    fn sync_frame_rate(&self, app: &AppWindow, container_fps: f64) {
        if container_fps.is_finite() && container_fps > 0.0 {
            *self.state.content_fps.lock().unwrap() = Some(container_fps as f32);
        }

        // `osd_opacity` is the scrim's live animated value; the flags cover the
        // window before the first frame of the fade has painted (and any path
        // where the callback has not run yet).
        let osd_engaged = ui_needs_display_rate(
            app.get_osd_visible(),
            app.get_is_paused(),
            app.get_playback_started(),
            app.get_osd_opacity(),
            app.window().has_active_animations(),
        );

        let desired = if osd_engaged {
            None
        } else {
            effective_frame_rate(
                *self.state.content_fps.lock().unwrap(),
                self.state
                    .mpv
                    .lock()
                    .unwrap()
                    .as_ref()
                    .and_then(|mpv| mpv.get_property::<f64>("speed").ok())
                    .unwrap_or(1.0) as f32,
            )
        };

        let mut applied = self.state.applied_frame_rate.lock().unwrap();
        let reapply = self.state.frame_rate_reapply.swap(false, Ordering::SeqCst);
        if reapply || *applied != Some(desired) {
            set_content_frame_rate(desired);
            *applied = Some(desired);
            alog(&format!("frame rate: requested {desired:?}"));
        }
    }

    /// Android: the OSD was just woken (touch, key, or a popup opening) or is
    /// animating. Drop the content frame-rate request immediately so the fade and
    /// controls run at the display's normal maximum, rather than waiting up to a
    /// tick. No-op when the request is already cleared.
    #[cfg(target_os = "android")]
    pub fn notify_osd_activity(&self) {
        clear_content_frame_rate_for_ui(&self.state);
    }

    /// Android: re-arm the current URL through `pending` so the
    /// `BeforeRendering` handler reloads it. When `resume_at` is set it is
    /// passed as a per-load `start` option (see the `loadfile` call), so a
    /// manual decoder switch resumes where the user was instead of restarting
    /// at 0. Returns `false` when there is no URL to reload.
    #[cfg(target_os = "android")]
    fn reload_current_url(&self, resume_at: Option<f64>) -> bool {
        let Some(url) = self.state.last_url.lock().unwrap().clone() else {
            return false;
        };
        self.arm_online_subtitle_restore();
        *self.state.pending_headers.lock().unwrap() =
            self.state.last_headers.lock().unwrap().clone();
        *self.state.pending_subtitles.lock().unwrap() =
            self.state.last_subtitles.lock().unwrap().clone();
        *self.state.pending_start.lock().unwrap() = resume_at.map(|p| p.max(0.0));
        // Re-arm the load: the `BeforeRendering` handler picks `pending` up
        // and reloads at the queued start.
        *self.state.pending.lock().unwrap() = Some(url);
        self.state.started.store(false, Ordering::SeqCst);
        self.state.buffering.store(false, Ordering::SeqCst);
        self.state.hwdec_checked.store(false, Ordering::SeqCst);
        *self.state.seek_buffer.lock().unwrap() = None;
        *self.state.last_hwdec_log.lock().unwrap() = String::from("<unset>");
        *self.state.opened_at.lock().unwrap() = Instant::now();
        if let Some(app) = self.state.app.upgrade() {
            app.set_buffered_until(0.0);
            app.window().request_redraw();
        }
        true
    }

    /// Android: log the active decoder to logcat whenever it changes. Called on
    /// every [`Self::tick`] while the overlay is up, so the `nova-player` log
    /// shows the full decoder timeline for a session (`hwdec-current` is empty
    /// until the video decoder is actually open).
    #[cfg(target_os = "android")]
    fn survey_hwdec(&self, current: &str) {
        let Ok(mut last) = self.state.last_hwdec_log.lock() else {
            return;
        };
        if last.as_str() == current {
            return;
        }
        let label = if current.is_empty() {
            "--"
        } else {
            decode_label(current)
        };
        alog(&format!("hwdec survey: {current:?} ({label})"));
        *last = current.to_string();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ram_startup_tracks_partial_piece_chunks_and_expires_without_new_data() {
        let now = Instant::now();
        let mut progress = RamStartupProgress::default();
        progress.observe(0, now);
        assert!(!progress.recent(now));
        // A first 16 MiB piece may take much longer than the native startup
        // deadline to verify; each received chunk is still useful progress.
        progress.observe(16384, now);
        assert!(progress.recent(now + Duration::from_secs(29)));
        progress.observe(16384, now + Duration::from_secs(29));
        progress.observe(0, now + Duration::from_secs(29));
        assert!(
            !progress.recent(now + Duration::from_secs(31)),
            "unchanged/stale stats cannot extend startup"
        );
        progress.observe(32768, now + Duration::from_secs(32));
        assert!(progress.recent(now + Duration::from_secs(32)));
        assert!(
            !progress.recent(now + Duration::from_secs(62)),
            "no-progress startup must eventually fail"
        );
    }

    #[test]
    fn ram_torrent_cache_disables_disk_and_preserves_demuxer_limits() {
        let mpv = Mpv::with_initializer(|init| {
            init.set_property("config", false)?;
            init.set_property("vo", "null")?;
            init.set_property("ao", "null")
        })
        .unwrap();
        // Deliberately nondefault values catch both a fixed tiny replacement
        // and a later restore which overwrites the established packet policy.
        mpv.set_property("cache-on-disk", true).unwrap();
        mpv.set_property("demuxer-max-bytes", 64 * 1024 * 1024i64)
            .unwrap();
        mpv.set_property("demuxer-max-back-bytes", 12 * 1024 * 1024i64)
            .unwrap();
        mpv.set_property("demuxer-readahead-secs", 7.0).unwrap();
        mpv.set_property("cache-secs", 45.0).unwrap();
        let packet_options = || {
            (
                mpv.get_property::<i64>("demuxer-max-bytes").unwrap(),
                mpv.get_property::<i64>("demuxer-max-back-bytes").unwrap(),
                mpv.get_property::<f64>("demuxer-readahead-secs").unwrap(),
                mpv.get_property::<f64>("cache-secs").unwrap(),
            )
        };
        let before = packet_options();
        let mut saved = None;
        apply_torrent_packet_cache(&mpv, true, &mut saved).unwrap();
        assert!(!mpv.get_property::<bool>("cache-on-disk").unwrap());
        assert_eq!(packet_options(), before);
        apply_torrent_packet_cache(&mpv, true, &mut saved).unwrap();
        assert!(!mpv.get_property::<bool>("cache-on-disk").unwrap());
        assert_eq!(packet_options(), before);
        apply_torrent_packet_cache(&mpv, false, &mut saved).unwrap();
        assert!(saved.is_none());
        assert!(mpv.get_property::<bool>("cache-on-disk").unwrap());
        assert_eq!(packet_options(), before);
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn desktop_external_video_handoff_is_disabled() {
        // No URL or saved handler may reach a desktop process launcher.
        for url in ["https://example.invalid/movie.mkv", "/tmp/movie.mkv"] {
            assert!(open_external(url).is_err());
        }
    }

    #[test]
    fn android_party_resume_waits_for_host_with_or_without_renderer_loss() {
        for renderer_lost in [false, true] {
            let mut recovery = android_recovery::Recovery::new(true);
            recovery.setup();
            recovery.begin(12.0);
            recovery.submitted();
            recovery.observe(Some(42.0), Some(false));
            recovery.pause();
            if renderer_lost {
                recovery.teardown();
                recovery.setup();
            }

            assert!(!resume_android_recovery(&mut recovery, true, Some(1.0)));
            if renderer_lost {
                assert!(recovery.take_reload().unwrap().paused);
            } else {
                assert!(recovery.take_reload().is_none());
            }

            // The coordinator can resume once its host report arrives. Leaving
            // the party restores the ordinary preference on the next resume.
            recovery.observe(Some(43.0), Some(false));
            recovery.pause();
            assert!(resume_android_recovery(&mut recovery, true, None));
        }
    }

    #[test]
    fn retained_pip_unlock_recovers_without_activity_resume_and_preserves_pause_policy() {
        for auto_continue in [false, true] {
            for manually_paused in [false, true] {
                for renderer_lost in [false, true] {
                    for visible_before_surface in [false, true] {
                        for party_rate in [None, Some(1.0)] {
                            let mut recovery = android_recovery::Recovery::new(true);
                            recovery.setup();
                            recovery.begin(42.0);
                            recovery.submitted();
                            recovery.observe(Some(42.0), Some(manually_paused));
                            recovery.pause(); // screen-off / Stop while pinned
                            if renderer_lost {
                                recovery.teardown();
                            }
                            assert!(recovery.waiting());
                            if !visible_before_surface {
                                recovery.setup();
                            }
                            // The visible-PiP callback uses this same policy
                            // even though Android never sends Activity Resume.
                            let play =
                                resume_android_recovery(&mut recovery, auto_continue, party_rate);
                            assert_eq!(
                                play,
                                auto_continue && !manually_paused && party_rate.is_none()
                            );
                            if visible_before_surface {
                                recovery.setup();
                            }
                            let reload = recovery.take_reload();
                            assert_eq!(reload.is_some(), renderer_lost);
                            if let Some(reload) = reload {
                                assert_eq!(reload.position, 42.0);
                                assert_eq!(reload.paused, !play);
                                recovery.submitted();
                            }
                            assert!(!recovery.waiting());
                            // Stay paused still permits explicit Play. A later
                            // surface replacement must retain that new state.
                            recovery.observe(Some(43.0), Some(false));
                            recovery.teardown();
                            recovery.setup();
                            assert!(!recovery.take_reload().unwrap().paused);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn readiness_estimate_handles_local_files_unknown_duration_and_eof() {
        assert_eq!(
            readiness_buffered_ahead(30.0, 100.0, Some(false), None, None),
            70.0
        );
        assert_eq!(
            readiness_buffered_ahead(98.0, 100.0, Some(true), Some(110.0), None),
            2.0
        );
        assert_eq!(
            readiness_buffered_ahead(30.0, 0.0, Some(true), None, Some(4.5)),
            4.5
        );
        assert_eq!(readiness_buffered_ahead(30.0, 100.0, None, None, None), 0.0);
        assert_eq!(
            readiness_buffered_ahead(30.0, 100.0, Some(true), Some(20.0), Some(50.0)),
            0.0
        );
        assert_eq!(
            readiness_buffered_ahead(30.0, 100.0, Some(true), Some(f64::NAN), Some(4.5)),
            4.5
        );
        assert_eq!(
            readiness_buffered_ahead(f64::NAN, 100.0, Some(false), None, None),
            0.0
        );
    }

    #[test]
    fn party_clock_rate_is_bounded_and_rejects_non_finite_values() {
        assert_eq!(normalized_party_rate(Some(2.0)), Some(2.0));
        assert_eq!(normalized_party_rate(Some(0.5)), Some(0.5));
        assert_eq!(normalized_party_rate(Some(3.0)), Some(2.06));
        assert_eq!(normalized_party_rate(Some(0.1)), Some(0.485));
        assert_eq!(normalized_party_rate(Some(1.545)), Some(1.545));
        assert_eq!(normalized_party_rate(Some(1.455)), Some(1.455));
        assert_eq!(normalized_party_rate(Some(1.03)), Some(1.03));
        assert_eq!(normalized_party_rate(Some(f64::NAN)), None);
        assert_eq!(normalized_party_rate(Some(f64::INFINITY)), None);
        assert_eq!(normalized_party_rate(None), None);
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn confirmation_load_stays_paused_across_queue_and_controls_until_released() {
        const CHILD: &str = "NOVA_CONFIRMATION_HOLD_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(CHILD, "1")
                .args(["--exact", "tests::confirmation_load_stays_paused_across_queue_and_controls_until_released", "--nocapture"])
                .output().unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        i_slint_backend_testing::init_integration_test_with_mock_time();
        let app = AppWindow::new().unwrap();
        let player = Player::setup(&app);
        let directory =
            std::env::temp_dir().join(format!("nova-confirmation-hold-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(directory.clone());
        let source = directory.join("video.y4m");
        let mut video = b"YUV4MPEG2 W32 H18 F30:1 Ip A1:1 C420jpeg\n".to_vec();
        for _ in 0..300 {
            video.extend(b"FRAME\n");
            video.extend([128u8; 32 * 18 * 3 / 2]);
        }
        std::fs::write(&source, video).unwrap();
        let mpv = Mpv::with_initializer(|init| {
            init.set_property("config", false)?;
            init.set_property("vo", "null")?;
            init.set_property("ao", "null")
        })
        .unwrap();
        *player.state.mpv.lock().unwrap() = Some(mpv);
        *player.state.mpv_error.lock().unwrap() = None;
        player
            .play_with_preferences_and_load_policy(
                source.to_str().unwrap(),
                0.0,
                &[],
                &[],
                SessionPreferences::from_settings(
                    &nova_config::CacheSettings::default(),
                    TitlePreferences::default(),
                ),
                PlaybackLoadPolicy {
                    allow_external: false,
                    paused: true,
                },
            )
            .unwrap();
        assert!(player.awaiting_start_confirmation());
        // Consume the exact pause state used at the renderer/loadfile boundary.
        let paused = player.state.take_load_pause();
        assert!(
            paused,
            "desktop must not replace the requested pause with false"
        );
        let queued = player.state.pending.lock().unwrap().take().unwrap();
        {
            let guard = player.state.mpv.lock().unwrap();
            let mpv = guard.as_ref().unwrap();
            apply_load_playback_options(mpv, paused, None).unwrap();
            mpv.command("loadfile", &[queued.as_str()]).unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let ready = player
                .state
                .mpv
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .get_property::<f64>("duration")
                .unwrap_or(0.0)
                > 0.0;
            if ready {
                break;
            }
            assert!(Instant::now() < deadline, "held source did not load");
            std::thread::sleep(Duration::from_millis(10));
        }
        for _ in 0..20 {
            player.set_paused(false);
            player.toggle();
            player.seek(3.0);
            player.seek_relative(4.0);
            player.tick();
            let guard = player.state.mpv.lock().unwrap();
            let mpv = guard.as_ref().unwrap();
            assert!(mpv.get_property::<bool>("pause").unwrap());
            assert!(
                mpv.get_property::<f64>("time-pos").unwrap_or(0.0).abs() < 0.05,
                "confirmation controls must not move or start the native playhead"
            );
            drop(guard);
            std::thread::sleep(Duration::from_millis(10));
        }
        player.release_start_pause();
        player.seek(2.0);
        player.set_paused(false);
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let position = player
                .state
                .mpv
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .get_property::<f64>("time-pos")
                .unwrap_or(0.0);
            if position > 2.05 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "explicit confirmation did not resume playback"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        // Terminal startup failure abandons the hold and deferred load, while
        // keeping the failed source stopped. A second alternative may load.
        let missing = directory.join("missing.y4m");
        player
            .play_with_preferences_and_load_policy(
                missing.to_str().unwrap(),
                0.0,
                &[],
                &[],
                SessionPreferences::from_settings(
                    &nova_config::CacheSettings::default(),
                    TitlePreferences::default(),
                ),
                PlaybackLoadPolicy {
                    allow_external: false,
                    paused: true,
                },
            )
            .unwrap();
        let queued = player.state.pending.lock().unwrap().take().unwrap();
        {
            let guard = player.state.mpv.lock().unwrap();
            let mpv = guard.as_ref().unwrap();
            apply_load_playback_options(mpv, player.state.take_load_pause(), None).unwrap();
            mpv.command("loadfile", &[queued.as_str()]).unwrap();
        }
        std::thread::sleep(Duration::from_millis(100));
        *player.state.opened_at.lock().unwrap() = Instant::now() - Duration::from_secs(21);
        player.tick();
        assert!(player.playback_failed());
        assert!(!player.awaiting_start_confirmation());
        assert!(player.state.pending.lock().unwrap().is_none());
        assert!(player.state.source_waiting.load(Ordering::SeqCst));
        player.set_paused(false); // Error controls cannot play an abandoned file.
        player
            .play_with_preferences_and_load_policy(
                source.to_str().unwrap(),
                0.0,
                &[],
                &[],
                SessionPreferences::from_settings(
                    &nova_config::CacheSettings::default(),
                    TitlePreferences::default(),
                ),
                PlaybackLoadPolicy {
                    allow_external: false,
                    paused: true,
                },
            )
            .unwrap();
        assert!(player.awaiting_start_confirmation());
        assert!(player.state.take_load_pause());
        assert!(player.state.pending.lock().unwrap().is_some());
        player.close();
        assert!(!player.awaiting_start_confirmation());
    }

    #[test]
    fn load_options_hold_eof_and_pause_party_start() {
        let mpv = Mpv::with_initializer(|init| {
            init.set_property("config", false)?;
            init.set_property("vo", "null")?;
            init.set_property("ao", "null")
        })
        .unwrap();
        apply_load_playback_options(&mpv, false, Some(1.0)).unwrap();
        assert!(mpv.get_property::<bool>("pause").unwrap());
        assert_eq!(mpv.get_property::<String>("keep-open").unwrap(), "yes");
        apply_load_playback_options(&mpv, false, None).unwrap();
        assert!(!mpv.get_property::<bool>("pause").unwrap());
        assert_eq!(mpv.get_property::<String>("keep-open").unwrap(), "yes");
        apply_load_playback_options(&mpv, true, None).unwrap();
        assert!(
            mpv.get_property::<bool>("pause").unwrap(),
            "background recovery remains paused outside a party"
        );
    }

    #[test]
    fn cached_seeks_bridge_only_transient_same_session_ranges() {
        let now = Instant::now();
        let ranges =
            r#"{"seekable-ranges":[{"start":20.0,"end":80.0},{"start":120.0,"end":140.0}]}"#;
        let forward = SeekBuffer::begin(7, now, 60.0, 50.0, 200.0, 80.0, Some(ranges)).unwrap();
        let backward = SeekBuffer::begin(7, now, 30.0, 50.0, 200.0, 80.0, Some(ranges)).unwrap();
        assert_eq!(forward.visible(7, now, 60.0, 200.0), Some(80.0));
        assert_eq!(
            backward.visible(7, now + Duration::from_millis(750), 30.0, 200.0),
            Some(80.0)
        );
        assert!(
            forward.visible(8, now, 60.0, 200.0).is_none(),
            "replacement source owns a different session"
        );
        assert!(
            forward
                .visible(7, now + Duration::from_secs(2), 60.0, 200.0)
                .is_none(),
            "missing telemetry cannot preserve stale ranges forever"
        );
        assert!(forward.visible(7, now, 90.0, 200.0).is_none());
        assert!(
            SeekBuffer::begin(7, now, 100.0, 50.0, 200.0, 140.0, Some(ranges)).is_none(),
            "cache holes must not be painted as buffered"
        );
        assert!(SeekBuffer::begin(7, now, 90.0, 50.0, 200.0, 80.0, Some(ranges)).is_none());
        assert!(
            SeekBuffer::begin(
                7,
                now,
                60.0,
                50.0,
                200.0,
                80.0,
                Some(r#"{"seekable-ranges":[]}"#)
            )
            .is_none()
        );
        assert!(
            SeekBuffer::begin(7, now, 30.0, 50.0, 200.0, 80.0, None).is_none(),
            "a forward estimate cannot establish backward cache"
        );
        assert!(SeekBuffer::begin(7, now, 60.0, 50.0, 200.0, 80.0, None).is_some());
        assert!(SeekBuffer::begin(7, now, f64::NAN, 50.0, 200.0, 80.0, None).is_none());
        assert!(SeekBuffer::begin(7, now, 60.0, 50.0, 200.0, 80.0, Some("invalid")).is_none());
    }

    #[test]
    fn buffer_estimate_prefers_end_time_clamps_and_handles_missing_data() {
        assert_eq!(
            estimated_buffered_until(30.0, 100.0, Some(55.0), Some(80.0)),
            55.0
        );
        assert_eq!(
            estimated_buffered_until(30.0, 100.0, None, Some(20.0)),
            50.0
        );
        assert_eq!(
            estimated_buffered_until(90.0, 100.0, Some(150.0), None),
            100.0
        );
        assert_eq!(
            estimated_buffered_until(30.0, 100.0, Some(20.0), Some(50.0)),
            0.0
        );
        assert_eq!(estimated_buffered_until(30.0, 100.0, None, None), 0.0);
        assert_eq!(estimated_buffered_until(30.0, 0.0, Some(55.0), None), 0.0);
        assert_eq!(
            estimated_buffered_until(30.0, 100.0, Some(f64::NAN), Some(20.0)),
            50.0
        );
        assert_eq!(
            estimated_buffered_until(30.0, 100.0, Some(f64::INFINITY), Some(-1.0)),
            0.0
        );
        assert_eq!(
            estimated_buffered_until(f64::NAN, 100.0, Some(55.0), None),
            0.0
        );
        assert_eq!(estimated_buffered_until(-1.0, 100.0, Some(55.0), None), 0.0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn paused_unseekable_and_expired_seeks_keep_native_state_converging() {
        const CHILD: &str = "NOVA_PLAYER_SEEK_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(CHILD, "1")
                .args([
                    "--exact",
                    "tests::paused_unseekable_and_expired_seeks_keep_native_state_converging",
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
        use std::io::Write;
        use std::os::fd::AsRawFd;
        use std::os::unix::net::UnixStream;
        i_slint_backend_testing::init_integration_test_with_mock_time();
        let app = AppWindow::new().unwrap();
        let player = Player::setup(&app);
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
        let (reader, mut writer) = UnixStream::pair().unwrap();
        let fixture = wav.clone();
        let producer = std::thread::spawn(move || {
            writer.write_all(&fixture).unwrap();
        });
        let pipe = format!("fd://{}", reader.as_raw_fd());
        let mpv = Mpv::with_initializer(|init| {
            init.set_property("config", false)?;
            init.set_property("vo", "null")?;
            init.set_property("ao", "null")?;
            init.set_property("cache", false)?;
            init.set_property("pause", true)?;
            init.set_property("keep-open", true)
        })
        .unwrap();
        mpv.command("loadfile", &[&pipe]).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while mpv.get_property::<f64>("time-pos").is_err() {
            assert!(Instant::now() < deadline, "pipe fixture did not load");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!mpv.get_property::<bool>("seekable").unwrap());
        // The native acceptance code alone cannot establish success.
        assert!(mpv.set_property("time-pos", 8.0_f64).is_ok());
        *player.state.mpv.lock().unwrap() = Some(mpv);
        *player.state.preference_source.lock().unwrap() = pipe;
        app.set_player_open(true);
        player.tick();
        assert!(app.get_playback_started());
        app.set_position(8.0); // The slider may publish its target before callback.
        player.seek(8.0);
        assert!(!player.state.seek_pending.load(Ordering::SeqCst));
        assert!(app.get_position() < 2.0);
        player.tick();
        assert!(app.get_is_paused());
        producer.join().unwrap();

        // Exercise a successful native source with a pending target that never
        // arrives, without waiting five wall-clock seconds in a unit test.
        let path = std::env::temp_dir().join(format!("nova-seek-{}.wav", std::process::id()));
        std::fs::write(&path, wav).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _cleanup = Cleanup(path.clone());
        let source = path.to_string_lossy().into_owned();
        {
            let engine = player.state.mpv.lock().unwrap();
            let mpv = engine.as_ref().unwrap();
            mpv.command("loadfile", &[&source]).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while mpv.get_property::<String>("path").ok().as_deref() != Some(source.as_str())
                || !mpv.get_property::<bool>("seekable").unwrap_or(false)
                || mpv.get_property::<f64>("time-pos").is_err()
            {
                assert!(Instant::now() < deadline, "local fixture did not load");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        *player.state.preference_source.lock().unwrap() = source;
        app.set_position(8.0);
        app.set_is_paused(false);
        player.state.seek_pending.store(true, Ordering::SeqCst);
        *player.state.seek_deadline.lock().unwrap() = Some(Instant::now() + Duration::from_secs(5));
        player.tick();
        assert!(app.get_is_paused(), "pending seeks must still mirror pause");
        assert_eq!(
            app.get_position(),
            8.0,
            "accepted targets have a short grace period"
        );
        *player.state.seek_deadline.lock().unwrap() = Some(Instant::now() - Duration::from_secs(1));
        player.tick();
        assert!(!player.state.seek_pending.load(Ordering::SeqCst));
        assert!(
            app.get_position() < 2.0,
            "expired targets reconcile to actual position"
        );
        player.seek(4.0);
        assert!(player.state.seek_pending.load(Ordering::SeqCst));
        let deadline = Instant::now() + Duration::from_secs(5);
        while player.state.seek_pending.load(Ordering::SeqCst) {
            player.tick();
            assert!(
                Instant::now() < deadline,
                "ordinary seek failed to reconcile"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!((app.get_position() - 4.0).abs() <= 2.0);
        player.close();
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn close_restores_catalog_fullscreen_and_invalidates_capture_session() {
        // Slint's event-loop proxy is process-global; the sleep-timer fixture
        // also installs a platform. Keep this UI lifecycle test independent.
        const CHILD: &str = "NOVA_PLAYER_CLOSE_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(CHILD, "1")
                .args([
                    "--exact",
                    "tests::close_restores_catalog_fullscreen_and_invalidates_capture_session",
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
        i_slint_backend_testing::init_integration_test_with_mock_time();
        let app = AppWindow::new().unwrap();
        let player = Player::setup(&app);
        assert_eq!(player.playback_readiness(), PlaybackReadiness::default());
        let session = player.screenshot_session();
        app.window().set_maximized(true);
        player.set_playback_fullscreen_preference(true);
        player.restore_playback_fullscreen(&app);
        app.set_player_open(true);
        app.window().set_fullscreen(true);
        app.set_is_fullscreen(true);
        app.set_screenshot_busy(true);
        app.set_screenshot_notice("Screenshot saved".into());
        app.set_player_show_title("Show".into());
        app.set_player_episode_title("Episode".into());
        app.set_buffered_until(100.0);
        player.state.started.store(true, Ordering::SeqCst);
        player.set_subtitle_clearance(0.25, 1.0);
        {
            let guard = player.state.mpv.lock().unwrap();
            let mpv = guard.as_ref().unwrap();
            mpv.set_property("sub-pos", 95.0_f64).unwrap();
            player
                .state
                .subtitle_position
                .lock()
                .unwrap()
                .apply(mpv)
                .unwrap();
            assert_eq!(mpv.get_property::<f64>("sub-pos").unwrap(), 95.0);
        }
        player.close();
        assert_eq!(
            player
                .state
                .mpv
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .get_property::<f64>("sub-pos")
                .unwrap(),
            95.0
        );
        assert!(!app.get_player_open());
        assert!(!app.get_is_fullscreen());
        assert!(!app.window().is_fullscreen());
        assert!(player.playback_fullscreen_preference());
        assert!(
            app.window().is_maximized(),
            "fullscreen exit restores catalog maximization"
        );
        player.close();
        assert!(
            player.playback_fullscreen_preference(),
            "catalog cleanup preserves the choice"
        );
        assert!(!app.get_screenshot_busy());
        assert!(app.get_screenshot_notice().is_empty());
        assert!(app.get_player_show_title().is_empty());
        assert!(app.get_player_episode_title().is_empty());
        assert_eq!(app.get_buffered_until(), 0.0);
        assert_ne!(session, player.screenshot_session());
        assert_eq!(
            player.capture_screenshot(session),
            Err(ScreenshotError::StaleSession)
        );
        assert!(!player.state.started.load(Ordering::SeqCst));
        // Window-manager fullscreen can change without the mirrored OSD flag.
        app.window().set_fullscreen(true);
        app.set_is_fullscreen(false);
        player.close();
        assert!(!app.window().is_fullscreen());
        assert!(!app.get_is_fullscreen());

        // Exercise replacement in the same UI test: Slint permits only one
        // testing platform per process, even when test threads are separate.
        // The headless backend cannot install an OpenGL notifier. This test exercises
        // session queueing, not rendering; no source is loaded without that notifier.
        *player.state.mpv_error.lock().unwrap() = None;
        *player.state.pending.lock().unwrap() = Some("https://example.invalid/old".into());
        app.set_buffered_until(100.0);
        app.window().set_fullscreen(true);
        assert!(player.wait_for_source("Resolving stream…"));
        assert!(app.get_player_open());
        assert!(app.window().is_fullscreen());
        assert!(player.state.pending.lock().unwrap().is_none());
        assert!(player.state.source_waiting.load(Ordering::SeqCst));
        assert_eq!(player.playback_readiness(), PlaybackReadiness::default());
        assert_eq!(app.get_position(), 0.0);
        assert_eq!(app.get_duration(), 0.0);
        assert_eq!(app.get_buffered_until(), 0.0);
        *player.state.opened_at.lock().unwrap() = Instant::now() - Duration::from_secs(35);
        player.tick();
        assert_eq!(app.get_player_status(), "Resolving stream…");
        assert!(!player.playback_failed());
        app.set_show_stream_loading_text(false);
        assert!(!app.get_player_status_is_error());
        player.source_failed("Could not resolve this stream.");
        assert!(
            app.get_player_status_is_error(),
            "errors must survive hidden routine loading text"
        );
        assert!(player.playback_failed());
        player.tick();
        assert_eq!(app.get_player_status(), "Could not resolve this stream.");
        assert!(!player.take_external_handoff());
        player.close();
        assert!(!player.playback_failed(), "Close clears the failed session");
        assert!(!app.get_player_status_is_error());
        let before = player.screenshot_session();
        app.set_screenshot_busy(true);
        app.set_screenshot_notice("Previous capture failed".into());
        player.state.started.store(true, Ordering::SeqCst);
        app.set_buffered_until(100.0);
        player
            .play_with_preferences(
                "https://example.invalid/episode",
                0.0,
                &[],
                &[],
                SessionPreferences {
                    audio_language: "en".into(),
                    subtitle_language: "en".into(),
                    default_speed: 1.0,
                    default_subtitle_style: Default::default(),
                    title: TitlePreferences::default(),
                },
            )
            .unwrap();
        assert!(
            !player.playback_failed(),
            "Replacement clears the old failure"
        );
        assert!(!player.state.source_waiting.load(Ordering::SeqCst));
        assert_ne!(before, player.screenshot_session());
        assert!(!app.get_screenshot_busy());
        assert!(app.get_screenshot_notice().is_empty());
        assert!(!player.state.started.load(Ordering::SeqCst));
        assert_eq!(app.get_buffered_until(), 0.0);
        assert_eq!(player.playback_readiness(), PlaybackReadiness::default());
        app.set_buffered_until(100.0);
        player.seek(50.0);
        assert_eq!(app.get_buffered_until(), 0.0);
        assert_eq!(
            player.capture_screenshot(before),
            Err(ScreenshotError::StaleSession)
        );
        player.close();
        assert!(player.wait_for_source("Resolving stream…"));
        assert!(
            app.window().is_fullscreen(),
            "reopening restores fullscreen"
        );
        player.toggle_fullscreen();
        assert!(!app.window().is_fullscreen());
        player.close();
        assert!(!player.playback_fullscreen_preference());
        assert!(player.wait_for_source("Resolving stream…"));
        assert!(
            !app.window().is_fullscreen(),
            "a windowed choice is also remembered"
        );
        player.close();

        // Exercise the exact synchronous callback boundary used by both
        // manual and automatic next-episode handoffs. Assert inside the close
        // callback so a false->true fullscreen flash cannot pass unnoticed.
        app.window().set_maximized(true);
        player.set_playback_fullscreen_preference(true);
        player.restore_playback_fullscreen(&app);
        app.set_player_open(true);
        let next_player = player.clone();
        let next_app = app.as_weak();
        app.on_close_player(move || {
            next_player.close();
            let app = next_app.upgrade().unwrap();
            assert!(
                app.window().is_fullscreen(),
                "next-episode close must never leave fullscreen"
            );
        });
        player.with_preserved_fullscreen_on_close(|| app.invoke_close_player());
        assert!(!app.get_player_open());
        assert!(app.get_is_fullscreen());
        assert!(player.state.transition_window_held.load(Ordering::SeqCst));
        assert!(player.state.catalog_maximized.load(Ordering::SeqCst));
        assert!(player.wait_for_source("Resolving stream…"));
        assert!(
            app.window().is_fullscreen(),
            "resolver loading keeps the same window"
        );
        player.source_failed("Could not resolve this stream.");
        assert!(
            app.window().is_fullscreen(),
            "a failed next source retains fullscreen"
        );
        player.play("https://example.invalid/next", 0.0).unwrap();
        assert!(app.window().is_fullscreen());
        player.close();
        assert!(
            !app.window().is_fullscreen(),
            "explicit Close still returns to catalog"
        );
        assert!(
            app.window().is_maximized(),
            "the original catalog geometry survives the whole handoff"
        );

        player.set_playback_fullscreen_preference(true);
        player.restore_playback_fullscreen(&app);
        app.set_player_open(true);
        player.with_preserved_fullscreen_on_close(|| app.invoke_close_player());
        app.window().set_fullscreen(false); // An explicit WM exit in the picker.
        player
            .play("https://example.invalid/windowed-next", 0.0)
            .unwrap();
        assert!(
            !app.window().is_fullscreen(),
            "picker-time fullscreen exit must win"
        );
        assert!(!player.playback_fullscreen_preference());
        player.close();
        assert!(app.window().is_maximized());

        player.set_playback_fullscreen_preference(true);
        player.restore_playback_fullscreen(&app);
        app.set_player_open(true);
        player.with_preserved_fullscreen_on_close(|| app.invoke_close_player());
        player.release_window_transition();
        assert!(
            !app.window().is_fullscreen(),
            "Back from a handoff picker returns to catalog geometry"
        );
        assert!(app.window().is_maximized());

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            player.with_preserved_fullscreen_on_close(|| panic!("interrupted transition"));
        }));
        assert!(result.is_err());
        assert!(
            !player.state.preserve_close_window.load(Ordering::SeqCst),
            "panic must release the scoped close policy"
        );

        // Exercise the real startup tick with a local video and null outputs.
        // Loading's paused UI flag must be replaced before the wake callback
        // selects its timer, and later progress ticks must not restart it.
        let directory =
            std::env::temp_dir().join(format!("nova-osd-startup-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(directory.clone());
        let source = directory.join("video.y4m");
        let mut video = b"YUV4MPEG2 W32 H18 F30:1 Ip A1:1 C420jpeg\n".to_vec();
        for _ in 0..300 {
            video.extend(b"FRAME\n");
            video.extend([128u8; 32 * 18 * 3 / 2]);
        }
        std::fs::write(&source, video).unwrap();
        let mpv = Mpv::with_initializer(|init| {
            init.set_property("config", false)?;
            init.set_property("vo", "null")?;
            init.set_property("ao", "null")
        })
        .unwrap();
        apply_load_playback_options(&mpv, false, None).unwrap();
        mpv_load::load_file_at(&mpv, source.to_str().unwrap(), 0.0).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while mpv.get_property::<f64>("duration").unwrap_or(0.0) <= 0.0 {
            assert!(Instant::now() < deadline, "startup video did not load");
            std::thread::sleep(Duration::from_millis(10));
        }
        *player.state.mpv.lock().unwrap() = Some(mpv);
        *player.state.preference_source.lock().unwrap() = source.to_str().unwrap().to_owned();
        app.set_player_open(true);
        app.set_is_paused(true);
        let wakes = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let recorded = wakes.clone();
        let weak = app.as_weak();
        app.on_osd_mouse_moved(move || {
            recorded
                .borrow_mut()
                .push(weak.upgrade().unwrap().get_is_paused());
        });
        player.tick();
        assert!(app.get_playback_started());
        assert_eq!(
            &*wakes.borrow(),
            &[false],
            "startup chooses the playing timeout"
        );
        player.tick();
        assert_eq!(
            wakes.borrow().len(),
            1,
            "progress does not keep controls awake"
        );
        player.set_paused(true);
        let readiness = player.playback_readiness();
        assert!(readiness.seekable);
        assert!(!readiness.buffering, "a user pause is not cache starvation");
        assert!(
            readiness.buffered_ahead > 2.0,
            "local files need no packet cache"
        );

        // Ordinary EOF keeps the source loaded and pauses exactly once. Its
        // retained timeline can seek backwards, then explicit Play resumes
        // from the selected position without loading a second source.
        {
            let guard = player.state.mpv.lock().unwrap();
            let mpv = guard.as_ref().unwrap();
            mpv.set_property("time-pos", mpv.get_property::<f64>("duration").unwrap())
                .unwrap();
            mpv.set_property("pause", false).unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while !player.playback_readiness().ended {
            assert!(Instant::now() < deadline, "ordinary EOF was not retained");
            std::thread::sleep(Duration::from_millis(10));
        }
        app.set_is_paused(false);
        player.tick();
        assert!(app.get_player_open());
        assert!(app.get_playback_started());
        assert!(app.get_is_paused());
        assert!(player.is_paused());
        assert!(app.get_position() > app.get_duration() - 0.1);
        assert_eq!(&*wakes.borrow(), &[false, true]);
        let terminal_position = app.get_position();
        player.tick();
        assert_eq!(app.get_position(), terminal_position);
        assert_eq!(wakes.borrow().len(), 2, "steady EOF does not rearm the OSD");
        {
            let guard = player.state.mpv.lock().unwrap();
            let mpv = guard.as_ref().unwrap();
            assert!(!mpv.get_property::<bool>("idle-active").unwrap());
            assert_eq!(
                mpv.get_property::<String>("path").unwrap(),
                source.to_str().unwrap()
            );
        }
        player.seek_relative(-5.0);
        let deadline = Instant::now() + Duration::from_secs(5);
        while player.playback_readiness().ended {
            assert!(Instant::now() < deadline, "seek did not leave EOF");
            player.tick();
            std::thread::sleep(Duration::from_millis(10));
        }
        player.tick();
        assert!(
            app.get_is_paused(),
            "seeking alone preserves terminal pause"
        );
        let selected_position = app.get_position();
        assert!(selected_position > 0.0 && selected_position < 8.0);
        player.set_paused(false);
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.get_position() <= selected_position + 0.05 {
            assert!(
                Instant::now() < deadline,
                "Play did not resume the sought position"
            );
            player.tick();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(app.get_player_open());
        assert!(!app.get_is_paused());
        assert!(!player.playback_readiness().ended);
        player.set_paused(true);

        let native_rate = || {
            player
                .state
                .mpv
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .get_property::<f64>("speed")
                .unwrap()
        };
        player.set_party_playback_rate(Some(1.03));
        assert!((native_rate() - 1.03).abs() < 0.00001);
        for rate in [0.485, 1.455, 1.545, 2.06] {
            player.set_party_playback_rate(Some(rate));
            assert!((native_rate() - rate).abs() < 0.00001);
        }
        player.set_party_playback_rate(Some(f64::NAN));
        player.set_party_playback_rate(Some(f64::INFINITY));
        assert!(
            (native_rate() - 2.06).abs() < 0.00001,
            "invalid input must retain party ownership"
        );
        player.set_party_playback_rate(Some(1.03));
        player.set_title_speed(1.6);
        player.set_speed(2.0);
        assert!(
            (native_rate() - 1.03).abs() < 0.00001,
            "the clock owns the live rate"
        );
        assert!((player.effective_playback_speed() - 1.03).abs() < 0.00001);
        player.tick();
        assert!((native_rate() - 1.03).abs() < 0.00001);
        player.set_party_playback_rate(None);
        assert!(
            (native_rate() - 1.6).abs() < 0.00001,
            "restore the updated title rate"
        );
        player.set_speed(2.0);
        assert_eq!(
            native_rate(),
            2.0,
            "normal gesture preview still owns its native rate"
        );
        player.set_speed(player.effective_playback_speed());
        assert!((native_rate() - 1.6).abs() < 0.00001);

        player.set_party_playback_rate(Some(1.03));
        player.restore_playback_default(
            2,
            &nova_config::CacheSettings {
                playback_speed: 1.25,
                ..Default::default()
            },
        );
        assert!((native_rate() - 1.03).abs() < 0.00001);
        player.set_party_playback_rate(None);
        assert_eq!(
            native_rate(),
            1.25,
            "a changed default also survives the clock override"
        );

        *player.state.preference_source.lock().unwrap() = "https://example.invalid/stale".into();
        assert_eq!(player.playback_readiness(), PlaybackReadiness::default());
        player.set_party_playback_rate(Some(0.97));
        assert!(
            native_rate() == 1.25,
            "stale native sources receive no clock command"
        );
        *player.state.pending.lock().unwrap() = Some(source.to_str().unwrap().to_owned());
        *player.state.preference_source.lock().unwrap() = source.to_str().unwrap().to_owned();
        player.set_party_playback_rate(Some(1.02));
        assert!(
            native_rate() == 1.25,
            "queued loads receive no premature clock command"
        );
        assert_eq!(player.playback_readiness(), PlaybackReadiness::default());
        *player.state.pending.lock().unwrap() = None;
        player.set_party_playback_rate(Some(1.0));
        {
            let guard = player.state.mpv.lock().unwrap();
            let mpv = guard.as_ref().unwrap();
            mpv.set_property("time-pos", mpv.get_property::<f64>("duration").unwrap())
                .unwrap();
            mpv.set_property("pause", false).unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while !player.playback_readiness().ended {
            assert!(Instant::now() < deadline, "party EOF was not retained");
            std::thread::sleep(Duration::from_millis(10));
        }
        let party_eof_paused = player.is_paused();
        player.tick();
        assert!(
            app.get_player_open(),
            "the party coordinator owns terminal close"
        );
        assert_eq!(
            player.is_paused(),
            party_eof_paused,
            "the player tick preserves the party's terminal pause state"
        );
        assert!(player.playback_readiness().ended);
        *player.state.preference_source.lock().unwrap() = "https://example.invalid/stale".into();
        assert!(
            !player.playback_readiness().ended,
            "a stale source cannot end the party"
        );
        player.close();
        assert_eq!(player.playback_readiness(), PlaybackReadiness::default());
        player.set_party_playback_rate(None);
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn desktop_decoder_policy_is_accepted_by_mpv() {
        let mpv = Mpv::with_initializer(|init| {
            init.set_property("config", false)?;
            init.set_property("vo", "null")?;
            init.set_property("ao", "null")
        })
        .unwrap();
        configure_desktop_decoder(&mpv).unwrap();
        let expected = if cfg!(any(target_os = "windows", target_os = "linux")) {
            desktop_decoder::requested_mode(nova_config::active_cache_settings().desktop_hwdec)
        } else {
            "auto"
        };
        assert_eq!(mpv.get_property::<String>("hwdec").unwrap(), expected);
    }

    #[test]
    fn header_fields_preserve_commas_backslashes_and_clear_between_streams() {
        let fields = mpv_http_header_fields(&[
            ("Accept".into(), "video/mp4, */*".into()),
            ("Cookie".into(), "value=one\\two".into()),
            ("Bad".into(), "value\r\nInjected: yes".into()),
            ("Host".into(), "evil.test".into()),
        ]);
        assert_eq!(
            fields,
            vec!["Accept: video/mp4, */*", "Cookie: value=one\\two"]
        );
        assert!(mpv_http_header_fields(&[]).is_empty());
        // Exercise mpv's node-array setter without creating a window/video.
        let mpv = Mpv::with_initializer(|init| {
            init.set_property("config", false)?;
            init.set_property("vo", "null")?;
            init.set_property("ao", "null")
        })
        .unwrap();
        set_http_header_fields(&mpv, &[("Accept".into(), "video/mp4, */*".into())]).unwrap();
        assert_eq!(
            mpv.get_property::<String>("http-header-fields").unwrap(),
            "Accept: video/mp4, */*"
        );
        set_http_header_fields(&mpv, &[]).unwrap();
        assert_eq!(
            mpv.get_property::<String>("http-header-fields").unwrap(),
            ""
        );
        let subtitles = vec!["https://example.com/sub,one.vtt".to_owned()];
        set_string_list(&mpv, c"sub-files", subtitles).unwrap();
        assert_eq!(
            mpv.get_property::<String>("sub-files").unwrap(),
            "https://example.com/sub,one.vtt"
        );
        set_string_list(&mpv, c"sub-files", Vec::new()).unwrap();
        assert_eq!(mpv.get_property::<String>("sub-files").unwrap(), "");
    }

    /// The bundled-font unpack must not rewrite a file of the same size (so a
    /// normal start reuses it) but must rewrite on a size change (app update).
    #[test]
    fn write_if_size_differs_skips_and_rewrites() {
        let dir = std::env::temp_dir().join(format!("nova-font-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Roboto-Regular.ttf");

        write_if_size_differs(&path, b"abc").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"abc");

        // Same size: existing content is left untouched.
        std::fs::write(&path, b"xyz").unwrap();
        write_if_size_differs(&path, b"123").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"xyz");

        // Different size: rewritten.
        write_if_size_differs(&path, b"12345").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"12345");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_timing_uses_video_deadline_callbacks_without_blocking_ui() {
        let mpv = Mpv::new().expect("initialize libmpv");
        mpv.set_property("video-timing-offset", 0.05_f64).unwrap();
        configure_render_timing(&mpv).unwrap();
        assert_eq!(mpv.get_property::<f64>("video-timing-offset").unwrap(), 0.0);
    }

    #[test]
    fn android_display_rate_returns_to_video_only_after_ui_motion_finishes() {
        let desired = |visible, paused, started, opacity, animations| {
            if ui_needs_display_rate(visible, paused, started, opacity, animations) {
                None
            } else {
                effective_frame_rate(Some(24.0), 1.0)
            }
        };
        assert_eq!(desired(false, false, true, 0.0, false), Some(24.0));
        // Wake/fade-in, open/retained controls, fade-out, then video-only.
        assert_eq!(desired(true, false, true, 0.0, false), None);
        assert_eq!(desired(true, false, true, 0.5, true), None);
        assert_eq!(desired(true, false, true, 1.0, false), None);
        assert_eq!(desired(false, false, true, 0.4, true), None);
        assert_eq!(desired(false, false, true, 0.0, false), Some(24.0));
        // Independent popup/gesture animation also lifts the hint, without
        // permanently forcing a specific 60/120Hz refresh rate.
        assert_eq!(desired(false, false, true, 0.0, true), None);
        assert_eq!(desired(false, false, true, 0.0, false), Some(24.0));
        assert_eq!(desired(false, true, true, 0.0, false), None);
        assert_eq!(desired(false, false, false, 0.0, false), None);
    }

    /// Informational real-renderer diagnostic: no synthetic timing threshold.
    /// Use an existing local clip through NOVA_PLAYER_TIMING_SOURCE, or the
    /// default 24fps lavfi source when the host mpv includes that input device.
    /// NOVA_PLAYER_TIMING_BASELINE=1 restores both former timing settings in
    /// this test binary only, so identical runs can compare both paths.
    #[test]
    #[cfg(not(target_os = "android"))]
    #[ignore = "requires native OpenGL display and libmpv video input"]
    fn measure_native_video_ui_composite_timing() {
        use i_slint_backend_testing::{AccessibleRole, ElementHandle};
        use std::cell::{Cell, RefCell};
        #[derive(Clone, Copy)]
        struct Sample {
            phase: usize,
            composite: f64,
            interval: Option<f64>,
            subtitle: f64,
            video: f64,
        }
        const PHASES: [&str; 5] = [
            "startup",
            "live-fade",
            "live-menus",
            "paused-fade",
            "paused-menus",
        ];
        let app = AppWindow::new().unwrap();
        nova_ui::initialize_fonts(app.window());
        nova_ui::apply_theme(&app, false);
        app.set_show_home(false);
        app.set_osd_visible(true);
        app.set_desktop_decoder_available(true);
        app.window().set_size(slint::PhysicalSize::new(1280, 800));
        let phase = Rc::new(Cell::new(0));
        let observed_phase = phase.clone();
        let before = Rc::new(RefCell::new(None::<(Instant, bool, usize)>));
        let previous_active = Rc::new(RefCell::new(None::<(Instant, usize)>));
        let samples = Rc::new(RefCell::new(Vec::<Sample>::new()));
        let collected = samples.clone();
        let native_state = Rc::new(RefCell::new(None::<Arc<State>>));
        let observed_native = native_state.clone();
        let weak = app.as_weak();
        let player = Player::setup_with_render_observer(&app, move |state| match state {
            RenderingState::BeforeRendering => {
                let active = weak.upgrade().is_some_and(|app| {
                    app.get_playback_started() && app.window().has_active_animations()
                });
                *before.borrow_mut() = Some((Instant::now(), active, observed_phase.get()));
            }
            RenderingState::AfterRendering => {
                if let Some((start, active, phase)) = before.borrow_mut().take() {
                    if active && phase > 0 {
                        let interval = previous_active
                            .borrow_mut()
                            .replace((start, phase))
                            .filter(|(_, previous_phase)| *previous_phase == phase)
                            .map(|(previous, _)| {
                                start.duration_since(previous).as_secs_f64() * 1000.0
                            });
                        let state = observed_native.borrow();
                        let state = state.as_ref().unwrap();
                        collected.borrow_mut().push(Sample {
                            phase,
                            composite: start.elapsed().as_secs_f64() * 1000.0,
                            interval,
                            subtitle: state.timing_subtitle_us.load(Ordering::Relaxed) as f64
                                / 1000.0,
                            video: state.timing_mpv_us.load(Ordering::Relaxed) as f64 / 1000.0,
                        });
                    } else {
                        *previous_active.borrow_mut() = None;
                    }
                }
            }
            _ => {}
        });
        *native_state.borrow_mut() = Some(player.state.clone());
        let callbacks = Rc::new(RefCell::new(Vec::<(usize, &'static str, f64)>::new()));
        let callback_phase = phase.clone();
        let recorded = callbacks.clone();
        let p = player.clone();
        app.on_settings_opened(move || {
            let start = Instant::now();
            let _ = &p;
            #[cfg(any(target_os = "windows", target_os = "linux"))]
            p.refresh_decoder();
            recorded.borrow_mut().push((
                callback_phase.get(),
                "decoder-refresh",
                start.elapsed().as_secs_f64() * 1000.0,
            ));
        });
        let callback_phase = phase.clone();
        let recorded = callbacks.clone();
        let p = player.clone();
        app.on_tracks_popup_opened(move |_| {
            let start = Instant::now();
            p.refresh_tracks();
            recorded.borrow_mut().push((
                callback_phase.get(),
                "tracks-refresh",
                start.elapsed().as_secs_f64() * 1000.0,
            ));
        });
        let weak = app.as_weak();
        app.on_osd_mouse_moved(move || {
            if let Some(app) = weak.upgrade() {
                app.set_osd_visible(true);
            }
        });
        let baseline = std::env::var_os("NOVA_PLAYER_TIMING_BASELINE").is_some();
        if baseline {
            player.state.timing_baseline.store(true, Ordering::Relaxed);
            player
                .state
                .mpv
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .set_property("video-timing-offset", 0.05_f64)
                .unwrap();
        }
        let source = std::env::var("NOVA_PLAYER_TIMING_SOURCE")
            .unwrap_or_else(|_| "av://lavfi:testsrc=size=320x180:rate=24".into());
        player.play(&source, 0.0).unwrap();
        app.show().unwrap();
        let start = Instant::now();
        let last_action = Rc::new(Cell::new(Instant::now()));
        let action_step = Rc::new(Cell::new(0usize));
        let weak = app.as_weak();
        let tick_player = player.clone();
        let callback_phase = phase.clone();
        let recorded = callbacks.clone();
        let tick = slint::Timer::default();
        tick.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(250),
            move || {
                let now = Instant::now();
                tick_player.tick();
                recorded.borrow_mut().push((
                    callback_phase.get(),
                    "player-tick",
                    now.elapsed().as_secs_f64() * 1000.0,
                ));
            },
        );
        let driver_player = player.clone();
        let active_phase = phase.clone();
        let recorded = callbacks.clone();
        let driver = slint::Timer::default();
        driver.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(100),
            move || {
                let app = weak.upgrade().unwrap();
                if active_phase.get() == 0 {
                    if !app.get_playback_started() {
                        assert!(
                            start.elapsed() < Duration::from_secs(15),
                            "diagnostic source failed to start: {}",
                            app.get_player_status()
                        );
                        return;
                    }
                    active_phase.set(1);
                    action_step.set(0);
                    last_action.set(Instant::now());
                }
                if last_action.get().elapsed() < Duration::from_millis(400) {
                    return;
                }
                last_action.set(Instant::now());
                let current = active_phase.get();
                let step = action_step.get();
                if current == 1 || current == 3 {
                    app.set_osd_visible(step.is_multiple_of(2));
                } else {
                    app.set_osd_visible(true);
                    let event_start = Instant::now();
                    if step == 7 {
                        for text in [
                            slint::platform::WindowEvent::KeyPressed {
                                text: slint::platform::Key::Escape.into(),
                            },
                            slint::platform::WindowEvent::KeyReleased {
                                text: slint::platform::Key::Escape.into(),
                            },
                        ] {
                            app.window().dispatch_event(text);
                        }
                    } else {
                        let item = match step {
                            0 => ElementHandle::find_by_element_id(
                                &app,
                                "PlayerOverlay::settings_gear",
                            )
                            .next(),
                            2 | 4 | 6 => {
                                ElementHandle::find_by_element_id(&app, "SettingsMenu::back_touch")
                                    .next()
                            }
                            _ => {
                                let label = match step {
                                    1 => "Subtitles",
                                    3 => "Audio",
                                    5 => "Playback speed",
                                    _ => unreachable!(),
                                };
                                ElementHandle::find_by_accessible_label(&app, label).find(|item| {
                                    item.accessible_role() == Some(AccessibleRole::Button)
                                })
                            }
                        }
                        .unwrap_or_else(|| {
                            panic!("missing menu control phase={current} step={step}")
                        });
                        let origin = item.absolute_position();
                        let size = item.size();
                        let position = slint::LogicalPosition::new(
                            origin.x + size.width / 2.0,
                            origin.y + size.height / 2.0,
                        );
                        for event in [
                            slint::platform::WindowEvent::PointerPressed {
                                position,
                                button: slint::platform::PointerEventButton::Left,
                            },
                            slint::platform::WindowEvent::PointerReleased {
                                position,
                                button: slint::platform::PointerEventButton::Left,
                            },
                        ] {
                            app.window().dispatch_event(event);
                        }
                    }
                    recorded.borrow_mut().push((
                        current,
                        "menu-input",
                        event_start.elapsed().as_secs_f64() * 1000.0,
                    ));
                }
                if step == 7 {
                    if current == 4 {
                        slint::quit_event_loop().unwrap();
                    } else {
                        active_phase.set(current + 1);
                        action_step.set(0);
                        // Let the controls finish appearing before the next
                        // phase queries/clicks the gear's actual visible bounds.
                        if current == 1 || current == 3 {
                            app.set_osd_visible(true);
                        }
                        if current == 2 {
                            driver_player.set_paused(true);
                        }
                    }
                } else {
                    action_step.set(step + 1);
                }
            },
        );
        slint::run_event_loop().unwrap();
        let quantiles = |mut values: Vec<f64>| {
            values.sort_by(f64::total_cmp);
            if values.is_empty() {
                None
            } else {
                Some((
                    values[values.len() / 2],
                    values[(values.len() - 1) * 95 / 100],
                ))
            }
        };
        let samples = samples.borrow();
        for (phase, label) in PHASES.iter().enumerate().skip(1) {
            let frames = samples
                .iter()
                .filter(|sample| sample.phase == phase)
                .copied()
                .collect::<Vec<_>>();
            assert!(!frames.is_empty(), "no animated frames in {label}");
            eprintln!(
                "native player timing baseline={baseline} phase={label} frames={} composite_ms(med,p95)={:?} interval_ms(med,p95)={:?} subtitle_ms(med,p95)={:?} mpv_ms(med,p95)={:?}",
                frames.len(),
                quantiles(frames.iter().map(|s| s.composite).collect()),
                quantiles(frames.iter().filter_map(|s| s.interval).collect()),
                quantiles(frames.iter().map(|s| s.subtitle).collect()),
                quantiles(frames.iter().map(|s| s.video).collect())
            );
            for kind in [
                "decoder-refresh",
                "tracks-refresh",
                "player-tick",
                "menu-input",
            ] {
                let durations = callbacks
                    .borrow()
                    .iter()
                    .filter(|sample| sample.0 == phase && sample.1 == kind)
                    .map(|sample| sample.2)
                    .collect::<Vec<_>>();
                eprintln!(
                    "native player timing baseline={baseline} phase={label} callback={kind} ms(med,p95)={:?}",
                    quantiles(durations)
                );
            }
        }
        player.close();
    }

    /// The display hint must follow the effective presentation rate, not the
    /// container's nominal rate, across the whole 0.5–2.0× range.
    #[test]
    fn effective_frame_rate_scales_with_speed() {
        assert_eq!(effective_frame_rate(Some(24.0), 1.0), Some(24.0));
        assert_eq!(effective_frame_rate(Some(24.0), 2.0), Some(48.0));
        assert_eq!(effective_frame_rate(Some(24.0), 0.5), Some(12.0));
        assert_eq!(effective_frame_rate(Some(30.0), 1.5), Some(45.0));
        assert_eq!(effective_frame_rate(Some(24.0), 1.25), Some(30.0));
        assert_eq!(effective_frame_rate(None, 2.0), None);
    }
}
