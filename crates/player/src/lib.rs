//! In-app video playback through **mpv** (`libmpv2`) in the **same window**
//! as the catalog UI — mirroring the `slint_stremio_player` reference
//! prototype. The video renders *underneath* the whole window: mpv draws into
//! the same OpenGL framebuffer the Slint scene is composited onto, at
//! `RenderingState::BeforeRendering`. While a stream plays, the catalog is
//! hidden, the window background turns transparent, and the Slint scene only
//! paints the player overlay (status text + OSD) on top of the video.
//!
//! The same architecture runs on Android, against the prebuilt `libmpv.so`
//! vendored in `vendor/android-libs/` (provenance in that directory's SOURCES, link
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

use libmpv2::Mpv;
use nova_ui::{AppWindow, TrackRow};
use slint::{ComponentHandle, GraphicsAPI, RenderingState, VecModel};
use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_void};
use std::rc::Rc;
#[cfg(target_os = "android")]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[cfg(not(target_os = "android"))]
fn configure_desktop_decoder(mpv: &Mpv) -> libmpv2::Result<()> {
    // The embedded renderer uses Slint's OpenGL context, not an mpv-owned
    // D3D11/ANGLE context. On Windows, copy decoded frames back before GL
    // upload to avoid direct decoder-surface interop as a source of artifacts.
    // auto-copy retains hardware decoding, with mpv's software fallback.
    let mode = if cfg!(target_os = "windows") {
        "auto-copy"
    } else {
        "auto"
    };
    mpv.set_property("hwdec", mode)
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

/// Android activity lifecycle, fed by the `init_with_event_listener` hook in
/// `android_main`. `WAS_PAUSED` records that the activity actually backgrounded
/// (screen lock, home, recents) so a transient `Resume` (permission dialog,
/// notification shade) does not trigger a needless reload. `RESUME_PENDING` is
/// consumed by [`Player::tick`], which reloads the stream when a session was
/// live — the decoder's output surface does not survive a pause/stop, so video
/// would otherwise stay black while audio keeps playing.
#[cfg(target_os = "android")]
static ANDROID_WAS_PAUSED: AtomicBool = AtomicBool::new(false);
#[cfg(target_os = "android")]
static ANDROID_RESUME_PENDING: AtomicBool = AtomicBool::new(false);

/// Record an activity `Pause` (see [`ANDROID_WAS_PAUSED`]).
#[cfg(target_os = "android")]
pub fn note_android_pause() {
    ANDROID_WAS_PAUSED.store(true, Ordering::SeqCst);
}

/// Record an activity `Resume`; arms a one-shot reload when it follows a pause.
#[cfg(target_os = "android")]
pub fn note_android_resume() {
    if ANDROID_WAS_PAUSED.swap(false, Ordering::SeqCst) {
        ANDROID_RESUME_PENDING.store(true, Ordering::SeqCst);
    }
}

/// Bundled subtitle font (Roboto Regular; Apache-2.0, license kept next to the
/// file). The vendored Android libmpv is built without fontconfig and with
/// libass's system-font provider disabled, so on Android libass only ever sees
/// fonts embedded in the media — a text subtitle that brings no font of its own
/// has nothing to rasterize and renders blank (the track is still listed and
/// selectable). Unpacking this font and pointing mpv's `sub-fonts-dir`/
/// `sub-font` at it gives libass a default for exactly those subtitles; media
/// with embedded fonts keeps using them. The family name must match the face.
#[cfg(target_os = "android")]
const SUBTITLE_FONT: &[u8] = include_bytes!("../../../assets/fonts/Roboto-Regular.ttf");
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
    /// Android: GL entry points for the state guard around mpv's render (see
    /// [`gl_state`]). Created from the notifier's `get_proc_address` with the
    /// `libGLESv2` dlsym fallback for extension functions.
    #[cfg(target_os = "android")]
    gl: glow::Context,
}

// The raw pointers are only ever touched from the rendering notifier
// callbacks (and dropped on teardown).
unsafe impl Send for MpvGlUnderlay {}
unsafe impl Sync for MpvGlUnderlay {}

impl Drop for MpvGlUnderlay {
    fn drop(&mut self) {
        unsafe {
            if !self.render_ctx.is_null() {
                mpv_render_context_set_update_callback(self.render_ctx, None, std::ptr::null_mut());
                if !self.app_weak_ptr.is_null() {
                    let _ = Box::from_raw(self.app_weak_ptr as *mut slint::Weak<AppWindow>);
                }
                mpv_render_context_free(self.render_ctx);
            }
        }
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

/// The saved playback rate for this device (Settings → Player), clamped into
/// the selectable range. Read on every `play` so a rate change applies to the
/// next stream even if the player was never told about it.
fn saved_playback_speed() -> f32 {
    nova_config::clamp_playback_speed(nova_config::active_cache_settings().playback_speed)
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

/// Open `url` in the user's chosen external video app (Settings → Player →
/// External). Desktop only — Android uses the `ACTION_VIEW` intent instead
/// (see `external::open_external`).
///
/// The program comes from the active [`nova_config::DesktopExternalApp`]
/// preference (VLC, mpv, or the platform's system default handler). Detached:
/// the child owns its process, like the previous system-default fallback.
#[cfg(not(target_os = "android"))]
pub fn open_external(url: &str) -> Result<(), String> {
    let external_app = nova_config::active_cache_settings().desktop_external_app;
    #[cfg(target_os = "windows")]
    if external_app == nova_config::DesktopExternalApp::SystemDefault {
        return shell_open_url(url);
    }
    let program = external_app.program();
    std::process::Command::new(program)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("could not launch {program}: {e}"))
}

/// Open `url` in the desktop's default **browser** — the web-link counterpart
/// of [`open_external`], which launches the *video* app chosen in
/// Settings → Player. Used for links the app only points at, e.g. an addon's
/// configuration page (Settings → Addons → Configure). Detached, like the
/// external-player launch.
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

/// Save/restore the GL state around mpv's render.
///
/// mpv restores the context to *OpenGL defaults* when it is done, but Skia
/// caches the state it last set — which, right after it has cleared the window
/// and drawn, is generally not the default. On the next frame Skia therefore
/// skips "redundant" state changes and ends up drawing the OSD's opacity layer
/// (a `saveLayer`) with mpv's program, VAO, buffers or textures still bound.
/// Some drivers (Mali) happen to tolerate the mismatch; stricter ones (Adreno,
/// Xclipse on the S25 FE) corrupt the frame instead of showing the OSD.
///
/// Slint documents exactly this for `set_rendering_notifier`: "make sure to
/// save and restore state such as `TEXTURE_BINDING_2D` or
/// `ARRAY_BUFFER_BINDING` perfectly". This guard brackets
/// `mpv_render_context_render` with a snapshot/restore of every piece of state
/// Skia tracks.
#[cfg(target_os = "android")]
mod gl_state {
    // Thin FFI wrapper: every `glow::HasContext` call is unsafe by contract
    // (the GL context must be current), so the whole module opts out of the
    // per-call `unsafe {}` requirement rather than nesting blocks around each.
    #![allow(unsafe_op_in_unsafe_fn)]

    use glow::HasContext;
    use std::num::NonZeroU32;

    // `GL_TEXTURE_EXTERNAL_OES` (bind target) and `GL_TEXTURE_BINDING_EXTERNAL_OES`
    // (query name) share this enum value; glow exports neither.
    const TEXTURE_EXTERNAL_OES: u32 = 0x8D65;

    // Covers what Skia/mpv use on GLES3; bounds the per-frame query cost.
    const MAX_UNITS_SAVED: i32 = 16;

    struct Unit {
        tex_2d: i32,
        tex_cube: i32,
        tex_2d_array: i32,
        tex_external: i32,
        sampler: i32,
    }

    pub struct Saved {
        viewport: [i32; 4],
        scissor_box: [i32; 4],
        scissor_test: bool,
        clear_color: [f32; 4],
        color_mask: [i32; 4],
        program: i32,
        active_texture: i32,
        array_buffer: i32,
        element_buffer: i32,
        vertex_array: i32,
        draw_fbo: i32,
        read_fbo: i32,
        renderbuffer: i32,
        blend: bool,
        blend_src_rgb: i32,
        blend_dst_rgb: i32,
        blend_src_alpha: i32,
        blend_dst_alpha: i32,
        blend_eq_rgb: i32,
        blend_eq_alpha: i32,
        depth_test: bool,
        depth_func: i32,
        depth_mask: i32,
        cull_face: bool,
        cull_mode: i32,
        front_face: i32,
        stencil_test: bool,
        stencil_front: [i32; 3],
        stencil_front_op: [i32; 3],
        stencil_front_mask: i32,
        stencil_back: [i32; 3],
        stencil_back_op: [i32; 3],
        stencil_back_mask: i32,
        dither: bool,
        unpack_alignment: i32,
        pack_alignment: i32,
        units: Vec<Unit>,
    }

    /// Zero is "unbound" for every GL object, so map 0 to `None`.
    fn id<T>(value: i32, wrap: impl FnOnce(NonZeroU32) -> T) -> Option<T> {
        NonZeroU32::new(value as u32).map(wrap)
    }

    unsafe fn set_cap(gl: &glow::Context, cap: u32, on: bool) {
        if on {
            gl.enable(cap);
        } else {
            gl.disable(cap);
        }
    }

    /// Snapshot the state Skia tracks, before mpv is allowed to touch it.
    pub unsafe fn save(gl: &glow::Context) -> Saved {
        let max_units = gl
            .get_parameter_i32(glow::MAX_COMBINED_TEXTURE_IMAGE_UNITS)
            .clamp(1, MAX_UNITS_SAVED);
        let active_texture = gl.get_parameter_i32(glow::ACTIVE_TEXTURE);

        let mut viewport = [0i32; 4];
        gl.get_parameter_i32_slice(glow::VIEWPORT, &mut viewport);
        let mut scissor_box = [0i32; 4];
        gl.get_parameter_i32_slice(glow::SCISSOR_BOX, &mut scissor_box);
        let mut clear_color = [0.0f32; 4];
        gl.get_parameter_f32_slice(glow::COLOR_CLEAR_VALUE, &mut clear_color);
        let mut color_mask = [0i32; 4];
        gl.get_parameter_i32_slice(glow::COLOR_WRITEMASK, &mut color_mask);

        // Texture-unit bindings are per unit, so walk them from a known unit and
        // restore the original active unit afterwards.
        let mut units = Vec::with_capacity(max_units as usize);
        for unit in 0..max_units {
            gl.active_texture(glow::TEXTURE0 + unit as u32);
            units.push(Unit {
                tex_2d: gl.get_parameter_i32(glow::TEXTURE_BINDING_2D),
                tex_cube: gl.get_parameter_i32(glow::TEXTURE_BINDING_CUBE_MAP),
                tex_2d_array: gl.get_parameter_i32(glow::TEXTURE_BINDING_2D_ARRAY),
                tex_external: gl.get_parameter_i32(TEXTURE_EXTERNAL_OES),
                sampler: gl.get_parameter_i32(glow::SAMPLER_BINDING),
            });
        }
        gl.active_texture(active_texture as u32);

        Saved {
            viewport,
            scissor_box,
            scissor_test: gl.is_enabled(glow::SCISSOR_TEST),
            clear_color,
            color_mask,
            program: gl.get_parameter_i32(glow::CURRENT_PROGRAM),
            active_texture,
            array_buffer: gl.get_parameter_i32(glow::ARRAY_BUFFER_BINDING),
            element_buffer: gl.get_parameter_i32(glow::ELEMENT_ARRAY_BUFFER_BINDING),
            vertex_array: gl.get_parameter_i32(glow::VERTEX_ARRAY_BINDING),
            draw_fbo: gl.get_parameter_i32(glow::DRAW_FRAMEBUFFER_BINDING),
            read_fbo: gl.get_parameter_i32(glow::READ_FRAMEBUFFER_BINDING),
            renderbuffer: gl.get_parameter_i32(glow::RENDERBUFFER_BINDING),
            blend: gl.is_enabled(glow::BLEND),
            blend_src_rgb: gl.get_parameter_i32(glow::BLEND_SRC_RGB),
            blend_dst_rgb: gl.get_parameter_i32(glow::BLEND_DST_RGB),
            blend_src_alpha: gl.get_parameter_i32(glow::BLEND_SRC_ALPHA),
            blend_dst_alpha: gl.get_parameter_i32(glow::BLEND_DST_ALPHA),
            blend_eq_rgb: gl.get_parameter_i32(glow::BLEND_EQUATION_RGB),
            blend_eq_alpha: gl.get_parameter_i32(glow::BLEND_EQUATION_ALPHA),
            depth_test: gl.is_enabled(glow::DEPTH_TEST),
            depth_func: gl.get_parameter_i32(glow::DEPTH_FUNC),
            depth_mask: gl.get_parameter_i32(glow::DEPTH_WRITEMASK),
            cull_face: gl.is_enabled(glow::CULL_FACE),
            cull_mode: gl.get_parameter_i32(glow::CULL_FACE_MODE),
            front_face: gl.get_parameter_i32(glow::FRONT_FACE),
            stencil_test: gl.is_enabled(glow::STENCIL_TEST),
            stencil_front: [
                gl.get_parameter_i32(glow::STENCIL_FUNC),
                gl.get_parameter_i32(glow::STENCIL_REF),
                gl.get_parameter_i32(glow::STENCIL_VALUE_MASK),
            ],
            stencil_front_op: [
                gl.get_parameter_i32(glow::STENCIL_FAIL),
                gl.get_parameter_i32(glow::STENCIL_PASS_DEPTH_FAIL),
                gl.get_parameter_i32(glow::STENCIL_PASS_DEPTH_PASS),
            ],
            stencil_front_mask: gl.get_parameter_i32(glow::STENCIL_WRITEMASK),
            stencil_back: [
                gl.get_parameter_i32(glow::STENCIL_BACK_FUNC),
                gl.get_parameter_i32(glow::STENCIL_BACK_REF),
                gl.get_parameter_i32(glow::STENCIL_BACK_VALUE_MASK),
            ],
            stencil_back_op: [
                gl.get_parameter_i32(glow::STENCIL_BACK_FAIL),
                gl.get_parameter_i32(glow::STENCIL_BACK_PASS_DEPTH_FAIL),
                gl.get_parameter_i32(glow::STENCIL_BACK_PASS_DEPTH_PASS),
            ],
            stencil_back_mask: gl.get_parameter_i32(glow::STENCIL_BACK_WRITEMASK),
            dither: gl.is_enabled(glow::DITHER),
            unpack_alignment: gl.get_parameter_i32(glow::UNPACK_ALIGNMENT),
            pack_alignment: gl.get_parameter_i32(glow::PACK_ALIGNMENT),
            units,
        }
    }

    /// Put the context back exactly as Skia left it.
    pub unsafe fn restore(gl: &glow::Context, s: &Saved) {
        gl.use_program(id(s.program, glow::NativeProgram));

        gl.bind_vertex_array(id(s.vertex_array, glow::NativeVertexArray));
        gl.bind_buffer(glow::ARRAY_BUFFER, id(s.array_buffer, glow::NativeBuffer));
        gl.bind_buffer(
            glow::ELEMENT_ARRAY_BUFFER,
            id(s.element_buffer, glow::NativeBuffer),
        );

        for (unit, state) in s.units.iter().enumerate() {
            let slot = glow::TEXTURE0 + unit as u32;
            gl.active_texture(slot);
            gl.bind_texture(glow::TEXTURE_2D, id(state.tex_2d, glow::NativeTexture));
            gl.bind_texture(
                glow::TEXTURE_CUBE_MAP,
                id(state.tex_cube, glow::NativeTexture),
            );
            gl.bind_texture(
                glow::TEXTURE_2D_ARRAY,
                id(state.tex_2d_array, glow::NativeTexture),
            );
            gl.bind_texture(
                TEXTURE_EXTERNAL_OES,
                id(state.tex_external, glow::NativeTexture),
            );
            gl.bind_sampler(slot, id(state.sampler, glow::NativeSampler));
        }
        gl.active_texture(s.active_texture as u32);

        gl.bind_framebuffer(
            glow::DRAW_FRAMEBUFFER,
            id(s.draw_fbo, glow::NativeFramebuffer),
        );
        gl.bind_framebuffer(
            glow::READ_FRAMEBUFFER,
            id(s.read_fbo, glow::NativeFramebuffer),
        );
        gl.bind_renderbuffer(
            glow::RENDERBUFFER,
            id(s.renderbuffer, glow::NativeRenderbuffer),
        );

        gl.viewport(s.viewport[0], s.viewport[1], s.viewport[2], s.viewport[3]);
        gl.scissor(
            s.scissor_box[0],
            s.scissor_box[1],
            s.scissor_box[2],
            s.scissor_box[3],
        );
        set_cap(gl, glow::SCISSOR_TEST, s.scissor_test);

        gl.clear_color(
            s.clear_color[0],
            s.clear_color[1],
            s.clear_color[2],
            s.clear_color[3],
        );
        gl.color_mask(
            s.color_mask[0] != 0,
            s.color_mask[1] != 0,
            s.color_mask[2] != 0,
            s.color_mask[3] != 0,
        );

        set_cap(gl, glow::BLEND, s.blend);
        gl.blend_func_separate(
            s.blend_src_rgb as u32,
            s.blend_dst_rgb as u32,
            s.blend_src_alpha as u32,
            s.blend_dst_alpha as u32,
        );
        gl.blend_equation_separate(s.blend_eq_rgb as u32, s.blend_eq_alpha as u32);

        set_cap(gl, glow::DEPTH_TEST, s.depth_test);
        gl.depth_func(s.depth_func as u32);
        gl.depth_mask(s.depth_mask != 0);

        set_cap(gl, glow::CULL_FACE, s.cull_face);
        gl.cull_face(s.cull_mode as u32);
        gl.front_face(s.front_face as u32);

        set_cap(gl, glow::STENCIL_TEST, s.stencil_test);
        gl.stencil_func_separate(
            glow::FRONT,
            s.stencil_front[0] as u32,
            s.stencil_front[1],
            s.stencil_front[2] as u32,
        );
        gl.stencil_op_separate(
            glow::FRONT,
            s.stencil_front_op[0] as u32,
            s.stencil_front_op[1] as u32,
            s.stencil_front_op[2] as u32,
        );
        gl.stencil_mask_separate(glow::FRONT, s.stencil_front_mask as u32);
        gl.stencil_func_separate(
            glow::BACK,
            s.stencil_back[0] as u32,
            s.stencil_back[1],
            s.stencil_back[2] as u32,
        );
        gl.stencil_op_separate(
            glow::BACK,
            s.stencil_back_op[0] as u32,
            s.stencil_back_op[1] as u32,
            s.stencil_back_op[2] as u32,
        );
        gl.stencil_mask_separate(glow::BACK, s.stencil_back_mask as u32);

        set_cap(gl, glow::DITHER, s.dither);
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, s.unpack_alignment);
        gl.pixel_store_i32(glow::PACK_ALIGNMENT, s.pack_alignment);
    }
}

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
struct State {
    mpv: Mutex<Option<Mpv>>,
    /// Set when mpv could not be initialized / the renderer is not OpenGL.
    mpv_error: Mutex<Option<String>>,
    underlay: Mutex<Option<MpvGlUnderlay>>,
    /// URL queued for loadfile; consumed on the next `BeforeRendering` (mpv
    /// must have its render context attached before the file is loaded, or
    /// early frames would be dropped).
    pending: Mutex<Option<String>>,
    /// Request headers paired with the queued URL. Consumed with `pending`.
    pending_headers: Mutex<Vec<(String, String)>>,
    /// External subtitle files to load with the next stream.
    pending_subtitles: Mutex<Vec<String>>,
    /// Playback of the current session actually produced frames.
    started: AtomicBool,
    /// The OSD thumb (or a programmatic seek) moved ahead of mpv and a seek is
    /// in flight. The `tick` scrub guard only engages while this is set, so an
    /// initial resume (opened at the saved position via the load-time `start`
    /// option, with the UI still at 0) mirrors mpv instead of being mistaken
    /// for a scrub and frozen.
    seek_pending: AtomicBool,
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
    /// Android: URL snapshot taken when the GL surface is recreated
    /// mid-playback (screen lock/unlock, rotation). The decoder's output
    /// surface dies with the old surface while audio keeps its own clock, so
    /// `tick` reloads this URL at the live position. Set only while a session
    /// is active; cleared by `close` semantics via the `last_url` match.
    #[cfg(target_os = "android")]
    surface_pending_url: Mutex<Option<String>>,
    /// Android: restore the paused state after a surface-resume reload (the
    /// `BeforeRendering` load path always unpauses).
    #[cfg(target_os = "android")]
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

/// FFmpeg's JavaVM registration (`libavutil/jni.h`).
///
/// mpv's **direct** MediaCodec hwdec (`hwdec_aimagereader`) resolves a `JNIEnv`
/// through `mp_jni_get_env()`, which takes the VM from
/// `av_jni_get_java_vm(NULL)` — something the embedding application has to
/// register, and which nothing in this app did. The mpv 0.37 build vendored in
/// `vendor/android-libs/` *asserts* on the missing env (`hwdec_aimagereader.c:171`), so
/// the whole process aborts instead of reporting a failure (mpv ≥ 0.38 turned
/// that same spot into a soft `return -1`). The vendored `libmpv.so` exports
/// `av_jni_set_java_vm` (checked with `readelf -sW --dyn-syms`), so registering
/// the VM we already stash for JNI is what makes the zero-copy path usable at
/// all. Returns whether the VM is registered: because the failure mode here is a
/// crash rather than a fallback, the caller keeps direct MediaCodec off when
/// this returns false.
#[cfg(target_os = "android")]
fn register_java_vm() -> bool {
    // Safety: an FFmpeg (libavutil) symbol the vendored libmpv.so exports;
    // libmpv2-sys already links `-lmpv`, so it resolves from there at load.
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
    // `android_main`; libavutil only stores it here.
    let rc = unsafe { av_jni_set_java_vm(vm, std::ptr::null_mut()) };
    if rc != 0 {
        alog(&format!(
            "av_jni_set_java_vm failed ({rc}); direct mediacodec disabled"
        ));
        return false;
    }
    alog("JavaVM registered with libavutil (direct mediacodec possible)");
    true
}

/// The in-app player. Cheap to clone (shares [`Arc`]); clone into the UI
/// callbacks, the 250 ms state timer and the `Bridge`.
#[derive(Clone)]
pub struct Player {
    state: Arc<State>,
}

impl Player {
    /// Create the mpv core and install the rendering notifier on `app`'s
    /// window (the underlay render context is created at the first
    /// `RenderingSetup`, when the GL context is current).
    pub fn setup(app: &AppWindow) -> Self {
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
                // Android: audio out through OpenSL ES (the vendored libmpv
                // links it) and no config/script discovery — the app owns mpv's
                // configuration, and there is no user-editable mpv.conf to
                // honour inside the app's private directory.
                #[cfg(target_os = "android")]
                {
                    let _ = mpv.set_property("ao", "opensles");
                    let _ = mpv.set_property("config", "no");
                    let _ = mpv.set_property("load-scripts", "no");
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
                    // Bitmap subtitles (Blu-ray HDMV PGS) are decoded by
                    // FFmpeg's `hdmv_pgs_subtitle`, which the vendored Android
                    // libmpv provides only in the `full` flavor (the `default`
                    // flavor omits it). No mpv option here can render them if a
                    // `default`-flavor libmpv is linked — see vendor/android-libs/SOURCES.
                    alog("subtitles: PGS/bitmap decode requires the full-flavor libmpv");
                }
                #[cfg(not(target_os = "android"))]
                let _ = configure_desktop_decoder(mpv);
                None
            }
            Err(e) => Some(format!("failed to initialize mpv: {e}")),
        };

        let state = Arc::new(State {
            mpv: Mutex::new(mpv.ok()),
            mpv_error: Mutex::new(mpv_error),
            underlay: Mutex::new(None),
            pending: Mutex::new(None),
            pending_headers: Mutex::new(Vec::new()),
            pending_subtitles: Mutex::new(Vec::new()),
            started: AtomicBool::new(false),
            seek_pending: AtomicBool::new(false),
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
            surface_pending_url: Mutex::new(None),
            #[cfg(target_os = "android")]
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

        let notifier_state = Arc::clone(&state);
        if let Err(e) =
            app.window()
                .set_rendering_notifier(move |state, graphics_api| match state {
                    RenderingState::RenderingSetup => {
                        let mpv = notifier_state.mpv.lock().unwrap();
                        let Some(mpv) = mpv.as_ref() else {
                            return; // mpv init failed; play() reports the error
                        };
                        if notifier_state.underlay.lock().unwrap().is_some() {
                            // A render context is already installed. On Android
                            // a fresh `RenderingSetup` means a *new* GL context
                            // (Skia builds one per surface), so the old mpv
                            // context is stale — a sign its `RenderingTeardown`
                            // was missed on a dead surface. Logged so logcat can
                            // confirm; the activity-`Resume` reload recovers
                            // lock/unlock video independently of this path.
                            #[cfg(target_os = "android")]
                            alog("render setup: underlay already present (teardown missed?)");
                            return; // already created
                        }

                        let GraphicsAPI::NativeOpenGL { get_proc_address } = graphics_api else {
                            let msg = "cannot play in-app: the mpv player needs an OpenGL renderer";
                            *notifier_state.mpv_error.lock().unwrap() = Some(msg.to_string());
                            #[cfg(target_os = "android")]
                            alog(msg);
                            return;
                        };

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
                            if mpv_render_context_create(
                                &mut render_ctx,
                                raw_ctx,
                                params.as_mut_ptr(),
                            ) < 0
                            {
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
                                #[cfg(target_os = "android")]
                                gl,
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
                            // The decoder's output surface died with the old
                            // GL surface while audio kept its own clock: if a
                            // session was actively playing, snapshot its URL
                            // so `tick` reloads it at the live position.
                            // First creation has no playback (`started` is
                            // false) and skips; `close` clears `last_url`, so
                            // a teardown after close arms nothing.
                            #[cfg(target_os = "android")]
                            if notifier_state.started.load(Ordering::SeqCst) {
                                if let Some(url) = notifier_state.last_url.lock().unwrap().clone() {
                                    alog("surface recreated mid-playback; arming resume reload");
                                    *notifier_state.surface_pending_url.lock().unwrap() = Some(url);
                                }
                            }
                        }
                    }
                    RenderingState::BeforeRendering => {
                        let Some(app) = notifier_state.app.upgrade() else {
                            return;
                        };
                        let underlay = notifier_state.underlay.lock().unwrap();
                        let Some(underlay) = underlay.as_ref() else {
                            return;
                        };

                        // Start playback of a freshly picked stream.
                        if let Some(url) = notifier_state.pending.lock().unwrap().take() {
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
                                // mpv reads this option when opening the URL;
                                // set it before every initial or reloaded file.
                                let _ = set_http_header_fields(mpv, &headers);
                                let _ = set_string_list(mpv, c"sub-files", subtitles);
                                // Android decoder reloads pass the resume
                                // point as a per-file option: the global
                                // `start` property is only honored for the
                                // first load, so a second `loadfile` would
                                // otherwise restart at 0.
                                #[cfg(target_os = "android")]
                                match start {
                                    Some(pos) => {
                                        let opts = format!("start={pos}");
                                        let _ = mpv.command(
                                            "loadfile",
                                            &[url.as_str(), "replace", "-1", opts.as_str()],
                                        );
                                    }
                                    None => {
                                        let _ = mpv.command("loadfile", &[url.as_str()]);
                                    }
                                }
                                #[cfg(not(target_os = "android"))]
                                let _ = mpv.command("loadfile", &[url.as_str()]);
                                // Ensure the new file actually starts: if
                                // the previous session left mpv paused,
                                // the file would load but not play.
                                let _ = mpv.set_property("pause", false);
                                // A surface-resume reload restores a paused
                                // session as paused instead.
                                #[cfg(target_os = "android")]
                                if notifier_state.pending_paused.swap(false, Ordering::SeqCst) {
                                    let _ = mpv.set_property("pause", true);
                                }
                            }
                        }

                        // Draw the video only while the player overlay is up (the
                        // catalog view paints over it anyway otherwise).
                        if !app.get_player_open() {
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

                        // The default framebuffer (FBO 0) is presented with a
                        // bottom-left origin on both desktop GL and Android's
                        // GLES/EGL window surface, i.e. vertically flipped
                        // relative to the decoded image. Passing flip_y=1
                        // cancels that, so it is needed on Android too (it was
                        // previously assumed unnecessary there, which left the
                        // video upside down).
                        let mut flip = 1_i32;

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
                                    type_: MPV_RENDER_PARAM_INVALID,
                                    data: std::ptr::null_mut(),
                                },
                            ];

                            // Android: Skia tracks the GL state it has set, and
                            // mpv leaves the context changed behind Skia's back.
                            // Without this bracket, the OSD's opacity layer is
                            // drawn with mpv's program/VAO/textures still bound
                            // and corrupts the frame on stricter drivers. Save
                            // before, restore after (see [`gl_state`]).
                            #[cfg(target_os = "android")]
                            let saved = gl_state::save(&underlay.gl);

                            mpv_render_context_render(underlay.render_ctx, params.as_mut_ptr());

                            #[cfg(target_os = "android")]
                            gl_state::restore(&underlay.gl, &saved);
                        }
                    }
                    RenderingState::RenderingTeardown => {
                        #[cfg(target_os = "android")]
                        alog("render teardown: dropping mpv render context");
                        *notifier_state.underlay.lock().unwrap() = None;
                    }
                    _ => {}
                })
        {
            *state.mpv_error.lock().unwrap() =
                Some(format!("cannot install rendering notifier: {e}"));
        }

        Self { state }
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
        if let Some(err) = self.state.mpv_error.lock().unwrap().as_ref() {
            return Err(err.clone());
        }
        let Some(app) = self.state.app.upgrade() else {
            return Err("player window gone".into());
        };

        *self.state.pending.lock().unwrap() = Some(url.to_string());
        *self.state.pending_headers.lock().unwrap() = headers.to_vec();
        let mut subtitle_bytes = 0usize;
        let subtitles = subtitles
            .iter()
            .take(20)
            .filter(|url| {
                subtitle_bytes += url.len();
                subtitle_bytes <= 32 * 1024
                    && url.len() <= 4096
                    && (url.starts_with("https://") || url.starts_with("http://"))
                    && !url.bytes().any(|byte| byte < b' ' || byte == 127)
            })
            .cloned()
            .collect::<Vec<_>>();
        *self.state.pending_subtitles.lock().unwrap() = subtitles.clone();
        #[cfg(target_os = "android")]
        {
            // Keep the URL for a decoder fallback reload; then arm the chain
            // from the user's Settings → Player preference. A new stream drops
            // any runtime override the previous stream picked.
            *self.state.last_url.lock().unwrap() = Some(url.to_string());
            *self.state.last_headers.lock().unwrap() = headers.to_vec();
            *self.state.last_subtitles.lock().unwrap() = subtitles;
            *self.state.last_hwdec_log.lock().unwrap() = String::from("<unset>");
            *self.state.session_hwdec.lock().unwrap() = None;
            self.state.hwdec_checked.store(false, Ordering::SeqCst);
            self.arm_decoder_chain();
        }
        self.state.started.store(false, Ordering::SeqCst);
        self.state.seek_pending.store(false, Ordering::SeqCst);
        self.state.reported.store(false, Ordering::SeqCst);
        *self.state.opened_at.lock().unwrap() = Instant::now();

        // A new stream always starts playing, regardless of how the previous
        // one ended: mpv's `pause` property survives `stop`/EOF, so clear it
        // here (and again after loadfile in the render callback) — otherwise
        // the second playback would silently start paused. `start` is set on
        // every play (even 0.0): it persists across files, so a previous
        // resume must not leak into the next playback.
        if let Ok(mpv) = self.state.mpv.lock()
            && let Some(mpv) = mpv.as_ref()
        {
            let _ = mpv.set_property("pause", false);
            let _ = mpv.set_property("start", start_pos_secs.max(0.0));
            // Rate is per device (Settings → Player / the player's own
            // settings panel); `speed` also survives stop/EOF, so set it
            // from the saved value on every play instead of letting the
            // previous session's rate leak into this one.
            let _ = mpv.set_property("speed", saved_playback_speed() as f64);
        }

        // Android: immersive playback — hide the status and navigation bars
        // for as long as the overlay is up. `tick` re-asserts this; `close`
        // requests them back.
        #[cfg(target_os = "android")]
        let _ = set_system_bars_hidden(true);
        // Loading counts as engaged: keep the screen awake from open.
        #[cfg(target_os = "linux")]
        converge_idle_inhibit(true);
        #[cfg(target_os = "android")]
        converge_screen_on(true);

        app.set_player_open(true);
        app.set_playback_started(false);
        app.set_position(0.0);
        app.set_duration(0.0);
        app.set_is_paused(true);
        app.set_time_text(slint::SharedString::from("00:00 / 00:00"));
        app.set_player_status(slint::SharedString::from("Loading stream…"));
        // Android: show the decoder that was just armed right away, so the
        // badge is populated before the first frame and never reads "unknown"
        // after an orientation change. `tick` corrects it to the decoder mpv
        // actually engages (following any fallback). Other targets keep it
        // empty — their overlay has no badge.
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
        #[cfg(not(target_os = "android"))]
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
        let Ok(mpv) = self.state.mpv.lock() else {
            return;
        };
        let Some(mpv) = mpv.as_ref() else {
            return;
        };
        let paused: bool = mpv.get_property("pause").unwrap_or(false);
        let new_paused = !paused;
        let _ = mpv.set_property("pause", new_paused);
        if let Some(app) = self.state.app.upgrade() {
            app.set_is_paused(new_paused);
        }
        // Screens stay awake only while actually playing (mpv/VLC behavior).
        #[cfg(target_os = "linux")]
        converge_idle_inhibit(!new_paused);
        #[cfg(target_os = "android")]
        converge_screen_on(!new_paused);
    }

    /// Seek to `pos` seconds (OSD slider).
    pub fn seek(&self, pos: f32) {
        // Mark the seek in flight: the tick scrub guard only suppresses
        // mirroring while the thumb is ahead of mpv (see `seek_pending`).
        self.state.seek_pending.store(true, Ordering::SeqCst);
        let Ok(mpv) = self.state.mpv.lock() else {
            return;
        };
        if let Some(mpv) = mpv.as_ref() {
            let _ = mpv.set_property("time-pos", pos as f64);
        }
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

    /// Set the playback rate (0.5–2.0×). The caller owns the value (it is a
    /// per-device setting); this only pushes it into the running session — the
    /// next `play` re-reads the saved rate, so an idle player needs no state.
    pub fn set_speed(&self, speed: f32) {
        let Ok(mpv) = self.state.mpv.lock() else {
            return;
        };
        if let Some(mpv) = mpv.as_ref() {
            let _ = mpv.set_property("speed", nova_config::clamp_playback_speed(speed) as f64);
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

    /// Toggle window fullscreen.
    pub fn toggle_fullscreen(&self) {
        if let Some(app) = self.state.app.upgrade() {
            let current = app.get_is_fullscreen();
            app.window().set_fullscreen(!current);
            app.set_is_fullscreen(!current);
        }
    }

    /// Select an audio track by its mpv track id.
    pub fn pick_audio_track(&self, track_id: i32) {
        {
            let Ok(mpv) = self.state.mpv.lock() else {
                return;
            };
            if let Some(mpv) = mpv.as_ref() {
                let val = if track_id < 0 {
                    "no".to_string()
                } else {
                    track_id.to_string()
                };
                let _ = mpv.set_property("aid", val.as_str());
            }
        }
        // Re-read the track list so the popup's checkmark follows the pick.
        self.refresh_tracks();
    }

    /// Select a subtitle track by its mpv track id. Negative = off.
    pub fn pick_sub_track(&self, track_id: i32) {
        {
            let Ok(mpv) = self.state.mpv.lock() else {
                return;
            };
            if let Some(mpv) = mpv.as_ref() {
                let val = if track_id < 0 {
                    "no".to_string()
                } else {
                    track_id.to_string()
                };
                let _ = mpv.set_property("sid", val.as_str());
            }
        }
        // Re-read the track list so the popup's checkmark follows the pick.
        self.refresh_tracks();
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
        // MPV_FORMAT_STRING on a node property returns its JSON form.
        let raw: String = match mpv.get_property("track-list") {
            Ok(s) => s,
            Err(_) => return, // no file loaded; keep whatever is shown
        };
        let parsed = match serde_json::from_str::<serde_json::Value>(&raw) {
            Ok(v) => v,
            Err(_) => return,
        };

        // Selection source of truth. Some libmpv builds (Android) don't keep
        // `track-list/N/selected` in sync with a just-applied `sid`/`aid`, so
        // read the selection properties directly and fall back to the
        // track-list flag when they don't name a numeric track (`no`/`auto`).
        let track_id_prop = |name: &str| -> Option<i32> {
            mpv.get_property::<String>(name)
                .ok()
                .and_then(|raw| raw.parse::<i32>().ok())
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

        let sub_label = Self::track_selection_label(&sub_rows);
        let audio_label = Self::track_selection_label(&audio_rows);
        app.set_audio_tracks(Rc::new(VecModel::from(audio_rows)).into());
        app.set_sub_tracks(Rc::new(VecModel::from(sub_rows)).into());
        app.set_sub_track_label(sub_label);
        app.set_audio_track_label(audio_label);
    }

    /// Stop playback and return to the catalog.
    pub fn close(&self) {
        if let Ok(mpv) = self.state.mpv.lock()
            && let Some(mpv) = mpv.as_ref()
        {
            let _ = mpv.command("stop", &[]);
        }
        *self.state.pending.lock().unwrap() = None;
        self.state.pending_headers.lock().unwrap().clear();
        self.state.pending_subtitles.lock().unwrap().clear();
        #[cfg(target_os = "android")]
        {
            *self.state.last_url.lock().unwrap() = None;
            self.state.last_headers.lock().unwrap().clear();
            self.state.last_subtitles.lock().unwrap().clear();
            *self.state.session_hwdec.lock().unwrap() = None;
        }
        self.state.started.store(false, Ordering::SeqCst);
        self.state.seek_pending.store(false, Ordering::SeqCst);
        self.state.reported.store(false, Ordering::SeqCst);

        if let Some(app) = self.state.app.upgrade() {
            app.set_player_open(false);
            app.set_playback_started(false);
            app.set_position(0.0);
            app.set_duration(0.0);
            app.set_is_paused(true);
            app.set_time_text(slint::SharedString::from("00:00 / 00:00"));
            app.set_player_status(slint::SharedString::default());
            app.set_decode_label(slint::SharedString::default());
            app.set_decoder_selected(-1);
            app.set_sub_track_label(slint::SharedString::default());
            app.set_audio_track_label(slint::SharedString::default());
            // Android: leave fullscreen with the player, so a fullscreen press
            // cannot leak into the catalog: Slint zeros safe-area insets for
            // fullscreen, but catalog controls still need those insets even
            // when backgrounds such as Home's artwork draw under the bars.
            #[cfg(target_os = "android")]
            if app.get_is_fullscreen() {
                let _ = app.window().set_fullscreen(false);
                app.set_is_fullscreen(false);
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
        converge_screen_on(false);
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

        if !app.get_player_open() {
            return;
        }

        /// One tick's worth of mpv state.
        struct Snapshot {
            pos: f64,
            dur: f64,
            paused: bool,
            vol: f64,
            muted: bool,
            eof: bool,
            /// The UI position (slider/seek) is more than 2 s from mpv's
            /// `time-pos`. Checked after the Android resume-reload so a
            /// rebuilt render context blanking `time-pos` cannot strand it.
            diverged: bool,
            /// Android: the currently active decoder (mpv `hwdec-current`).
            #[cfg(target_os = "android")]
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

        let snapshot = {
            let Ok(mpv) = self.state.mpv.lock() else {
                return;
            };
            let Some(mpv) = mpv.as_ref() else {
                return;
            };

            let pos: f64 = mpv.get_property("time-pos").unwrap_or(0.0);
            let dur: f64 = mpv.get_property("duration").unwrap_or(0.0);
            let paused: bool = mpv.get_property("pause").unwrap_or(false);

            // While the user is scrubbing the OSD slider — or right after a
            // seek, until mpv actually applies it — the slider value diverges
            // from mpv's time-pos. Don't fight the thumb then: mirroring would
            // keep overwriting the new position with the stale one and the
            // slider would fall back to its previous state. Resume as soon as
            // mpv's time-pos catches up with the slider. Only a seek the UI
            // actually issued arms this (`seek_pending`), so an initial resume
            // opened at the saved position (UI still at 0) mirrors mpv instead
            // of being frozen here. Handled *after* the Android resume-reload
            // below, because freeing/recreating the mpv render context
            // (surface recreation) briefly blanks `time-pos`, which would
            // otherwise trip this guard and strand the reload.
            let seek_pending = self.state.seek_pending.load(Ordering::SeqCst);
            let diverged = seek_pending && (app.get_position() as f64 - pos).abs() > 2.0;
            if seek_pending && !diverged {
                // mpv caught up with the seek: disengage the guard.
                self.state.seek_pending.store(false, Ordering::SeqCst);
            }

            Snapshot {
                pos,
                dur,
                paused,
                // Mirror volume/mute (cheap reads; keeps UI in sync if changed
                // via keyboard shortcuts or external controls).
                vol: mpv.get_property("volume").unwrap_or(100.0),
                muted: mpv.get_property("mute").unwrap_or(false),
                eof: mpv.get_property("eof-reached").unwrap_or(false),
                diverged,
                #[cfg(target_os = "android")]
                hwdec_current: mpv.get_property("hwdec-current").unwrap_or_default(),
                #[cfg(target_os = "android")]
                container_fps: mpv.get_property("container-fps").unwrap_or(0.0),
            }
        };

        // Android: the video output died — the GL surface was recreated
        // mid-playback (rotation) or the activity was paused/backgrounded
        // (screen lock/unlock), taking the decoder's output surface with it.
        // Reload the same URL at the live position so video resumes instead
        // of staying black while audio continues. Two triggers feed one path:
        // `surface_pending_url` (armed from `RenderingSetup`) and
        // `ANDROID_RESUME_PENDING` (armed from the activity `Resume` event).
        // Skipped when the session moved on (`close`/`play` change `last_url`,
        // and `started` only flips true once the new file progresses) or
        // already ended.
        #[cfg(target_os = "android")]
        {
            let armed = self.state.surface_pending_url.lock().unwrap().take();
            let resumed = if ANDROID_RESUME_PENDING.swap(false, Ordering::SeqCst) {
                self.state.last_url.lock().unwrap().clone()
            } else {
                None
            };
            if let Some(armed) = armed.or(resumed) {
                let current = self.state.last_url.lock().unwrap().clone();
                let live = Some(armed.as_str()) == current.as_deref();
                let started = self.state.started.load(Ordering::SeqCst);
                if live && started && !snapshot.eof {
                    let at = self.current_position_secs();
                    alog(&format!("surface resume: reloading at {at:.1}s"));
                    if self.reload_current_url(Some(at)) && snapshot.paused {
                        // The load path unpauses; restore the paused state.
                        self.state.pending_paused.store(true, Ordering::SeqCst);
                    }
                } else {
                    alog(&format!(
                        "surface resume: skipped (live={live}, started={started}, eof={})",
                        snapshot.eof
                    ));
                }
            }
        }

        // Handled after the resume-reload above (see the `diverged` comment).
        // Keep the time label on the thumb's position while mpv catches up with
        // the seek (the text would otherwise show the stale spot).
        if snapshot.diverged {
            if snapshot.dur > 0.0 {
                let ui_pos = app.get_position() as f64;
                app.set_time_text(slint::SharedString::from(format!(
                    "{} / {}",
                    format_time(ui_pos),
                    format_time(snapshot.dur)
                )));
            }
            return;
        }

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

        // Android: drive the display refresh rate from the OSD state and the
        // content's frame rate.
        #[cfg(target_os = "android")]
        self.sync_frame_rate(&app, snapshot.container_fps);

        if snapshot.dur > 0.0 || snapshot.pos > 0.0 {
            // Frames are progressing: reveal the video (transparent window
            // background) and drop the loading status.
            if !self.state.started.swap(true, Ordering::SeqCst) {
                app.set_player_status(slint::SharedString::default());
                // Arm the 3s OSD auto-hide countdown as playback begins, so
                // the controls fade even if the mouse never moves.
                app.invoke_osd_mouse_moved();
            }
            app.set_playback_started(true);
            app.set_position(snapshot.pos as f32);
            app.set_duration(snapshot.dur.max(1.0) as f32);
            app.set_is_paused(snapshot.paused);
            // OSD visibility is owned solely by the auto-hide countdown in
            // `run.rs` (3 s playing, 5 s paused): never force it here, or a
            // paused bar could never fade.
            app.set_time_text(slint::SharedString::from(format!(
                "{} / {}",
                format_time(snapshot.pos),
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
                app.set_is_paused(true);
                app.set_player_status(slint::SharedString::from(
                    "Cannot play this stream (mpv could not start playback).",
                ));
            }
        }

        app.set_volume(snapshot.vol as f32);
        app.set_muted(snapshot.muted);

        // Natural end of the stream: stop and return to the catalog.
        if self.state.started.load(Ordering::SeqCst) && snapshot.eof && !snapshot.paused {
            self.close();
        }
    }

    /// Android: last resort for a stream the in-app player cannot start — hand
    /// it to a system video player (`ACTION_VIEW`, the path this app used before
    /// it had in-app playback) and leave the overlay. Returns `false` when no
    /// external player took it, in which case the caller keeps its on-screen
    /// error. Must not be called while the mpv lock is held ([`Self::close`]
    /// takes it).
    #[cfg(target_os = "android")]
    fn hand_off_to_external_player(&self) -> bool {
        if !self.state.last_headers.lock().unwrap().is_empty()
            || !self.state.last_subtitles.lock().unwrap().is_empty()
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
        let osd_engaged = app.get_osd_visible()
            || app.get_is_paused()
            || !app.get_playback_started()
            || app.get_osd_opacity() > 0.001;

        let desired = if osd_engaged {
            None
        } else {
            effective_frame_rate(
                *self.state.content_fps.lock().unwrap(),
                saved_playback_speed(),
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
        let mut applied = self.state.applied_frame_rate.lock().unwrap();
        if *applied != Some(None) {
            set_content_frame_rate(None);
            *applied = Some(None);
            alog("frame rate: requested None (osd active)");
        }
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
        *self.state.pending_headers.lock().unwrap() =
            self.state.last_headers.lock().unwrap().clone();
        *self.state.pending_subtitles.lock().unwrap() =
            self.state.last_subtitles.lock().unwrap().clone();
        *self.state.pending_start.lock().unwrap() = resume_at.map(|p| p.max(0.0));
        // Re-arm the load: the `BeforeRendering` handler picks `pending` up
        // and reloads at the queued start.
        *self.state.pending.lock().unwrap() = Some(url);
        self.state.started.store(false, Ordering::SeqCst);
        self.state.hwdec_checked.store(false, Ordering::SeqCst);
        *self.state.last_hwdec_log.lock().unwrap() = String::from("<unset>");
        *self.state.opened_at.lock().unwrap() = Instant::now();
        if let Some(app) = self.state.app.upgrade() {
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
        let expected = if cfg!(target_os = "windows") {
            "auto-copy"
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
