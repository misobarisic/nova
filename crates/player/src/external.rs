//! Android runtime glue: the JVM/activity pointers stashed by `android_main`,
//! the external-player fallback used when in-app mpv cannot take a stream, and
//! the immersive-mode toggle that hides the system bars during playback.
//!
//! The pointers feed three things: the `ACTION_VIEW` intent below, the
//! status/navigation-bar hiding in [`set_system_bars_hidden`], and (from
//! `src/player.rs`) the `av_jni_set_java_vm` registration that mpv's direct
//! MediaCodec path needs — see [`java_vm_ptr`]. Pure JNI, no mpv.
//!
//! In-app playback on Android goes through the same mpv path as desktop
//! (`src/player.rs`, against the prebuilt `libmpv.so` in `vendor/android-libs/`). This
//! module is what the catalog falls back to when the in-app player reports that
//! it cannot take the stream at all — mpv failed to initialize, the window has
//! no OpenGL renderer, or the vendored mpv build cannot decode it. The intent
//! carries an explicit video MIME type (`video/*`, or `application/x-mpegURL`
//! for HLS playlists) so it resolves to video players (VLC / MX Player / …) —
//! never the browser, since streams are media files, not web pages.
//! [`open_browser`] is the counterpart for real web pages (an addon's
//! `/configure` page): the same `ACTION_VIEW` with no MIME type, so the system
//! hands the link to whatever handles web URLs (its browser or custom tab).
//!
//! The pointers are resolved lazily on the first open.

use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicI8, AtomicI64, AtomicPtr, AtomicU32, Ordering};

use jni::objects::JString;
use jni::refs::Global;
use jni::sys::jint;
use jni::{JavaVM, bind_java_type};
use slint::android::AndroidApp;

use crate::alog;

/// Raw runtime pointers stashed from `android_main` (valid for the whole
/// process: the VM and activity outlive every call). Set once before
/// `slint::android::init`; read when a stream goes to the system.
static VM_PTR: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ACTIVITY_PTR: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// The live `AndroidApp`, cloned from `android_main`. Kept so the system-bar
/// calls can be marshalled onto the **Java main / UI thread**: `android_main`
/// (and therefore Slint's event loop and every `tick`) runs on a dedicated
/// native thread, and `Window`/`WindowInsetsController`/`View` methods are only
/// safe on the UI thread. Replaced on every `android_main` (the activity may be
/// recreated), hence a `Mutex` rather than a `OnceLock`.
static ANDROID_APP: Mutex<Option<AndroidApp>> = Mutex::new(None);

/// Desired immersive state: `0` bars shown, `1` bars hidden. Set when the
/// player opens/closes; [`reassert_system_bars`] drives the applied state
/// towards it.
static BARS_WANT: AtomicI8 = AtomicI8::new(0);

/// Last **confirmed applied** immersive state: `0` bars shown, `1` bars hidden.
/// Stays put when a call fails or the insets controller is not attached yet, so
/// the next re-assert retries instead of assuming success. Starts unknown so
/// the first player tick establishes the catalog's edge-to-edge layout before
/// any playback has opened.
static BARS_STATE: AtomicI8 = AtomicI8::new(-1);

/// Probe window focus at most once every this many [`reassert_system_bars`] calls
/// (≈2 s at the player's 250 ms tick). The probe is a cheap `View.hasWindowFocus`
/// JNI read used only to recover the immersive state after the framework shows
/// the bars again (notification shade, recents), never to re-hide on a timer.
const FOCUS_POLL_EVERY: u32 = 8;
static FOCUS_POLL_TICKS: AtomicU32 = AtomicU32::new(0);

/// Last observed window-focus state: `0` unfocused, `1` focused. Starts focused
/// so startup does not fire a spurious re-assert.
static FOCUS_LAST: AtomicI8 = AtomicI8::new(1);

/// Called once from `android_main` in `lib.rs`. This module only compiles
/// on Android, so the call site needs no extra gating.
pub fn set_android_runtime(vm: *mut c_void, activity: *mut c_void) {
    VM_PTR.store(vm, Ordering::SeqCst);
    ACTIVITY_PTR.store(activity, Ordering::SeqCst);
}

/// Stash the `AndroidApp` so [`apply_system_bars`] can post its window calls to
/// the Java main thread. Called once from `android_main` in `lib.rs`, right
/// before `slint::android::init` consumes the handle.
pub fn set_android_app(app: AndroidApp) {
    *ANDROID_APP.lock().unwrap() = Some(app);
}

/// The stashed `JavaVM *` (null before `android_main` runs). Used here for the
/// intent fallback and by `crate::player` to register the VM with libavutil,
/// which is what mpv's direct MediaCodec path needs (see `register_java_vm`).
pub(crate) fn java_vm_ptr() -> *mut c_void {
    VM_PTR.load(Ordering::SeqCst)
}

/// The stashed activity raw pointer: the `NativeActivity` itself, i.e. a true
/// `Activity` (unlike the `Context` from `ndk-context`, which suffices for
/// service lookups but fails an `instanceof Activity` check). Borrowed per
/// call like every other raw pointer here; null before `android_main` runs.
pub fn android_activity_ptr() -> *mut c_void {
    ACTIVITY_PTR.load(Ordering::SeqCst)
}

/// Last active-network handle seen by [`network_changed`], or `i64::MIN`
/// before the first successful read. Android does not tell native code when
/// connectivity changes (Java gets `ConnectivityManager` callbacks), and
/// subclassing Java `NetworkCallback` from Rust/JNI is impractical, so the app
/// polls this cheap handle and forwards changes to iroh's
/// `Endpoint::network_change`.
static LAST_NETWORK: AtomicI64 = AtomicI64::new(i64::MIN);

/// Poll the current default network's handle and report whether it changed
/// since the previous call. The first successful read only establishes a
/// baseline. `false` while the runtime pointers are unavailable (e.g. before
/// `android_main`), and `true` when connectivity is lost (`handle` 0) or
/// regained, so iroh can re-establish relay/direct paths.
pub fn network_changed() -> bool {
    let Some(handle) = current_network_handle() else {
        return false;
    };
    let previous = LAST_NETWORK.swap(handle, Ordering::SeqCst);
    previous != i64::MIN && previous != handle
}

/// `ConnectivityManager.getActiveNetwork()?.getNetworkHandle()`, or `None`
/// when the runtime pointers or the active network are unavailable.
fn current_network_handle() -> Option<i64> {
    let vm_ptr = VM_PTR.load(Ordering::SeqCst);
    let act_ptr = ACTIVITY_PTR.load(Ordering::SeqCst);
    if vm_ptr.is_null() || act_ptr.is_null() {
        return None;
    }
    let vm = JavaVM::singleton().unwrap_or_else(|_| unsafe {
        // Safety: as in open_external — the VM outlives the process.
        JavaVM::from_raw(vm_ptr as *mut _)
    });
    let mut handle = 0i64;
    vm.attach_current_thread(|env| -> Result<(), jni::errors::Error> {
        let ptr = act_ptr as *mut _;
        // Safety: `act_ptr` is the activity global ref, valid for the process.
        let activity = unsafe { env.as_cast_raw::<Global<NovaActivity>>(&ptr)? };
        let service = JString::from_str(env, "connectivity")?;
        let raw = activity.get_system_service(env, &service)?;
        let manager = env.cast_local::<NovaConnectivityManager>(raw)?;
        let network = manager.get_active_network(env)?;
        if !network.is_null() {
            handle = network.get_network_handle(env)?;
        }
        Ok(())
    })
    .ok()?;
    Some(handle)
}

/// Ask the window's Surface to run at a given content frame rate, or clear the
/// request with `None` so the display returns to its normal maximum.
///
/// Resolved at runtime through `libandroid`: `ANativeWindow_setFrameRate`
/// exists only from API 30 and `ANativeWindow_setFrameRateWithChangeStrategy`
/// from API 31, while minSdk here is 26 — a direct link would fail to load on
/// older devices. The seamless strategy is used so a rate change cannot blink
/// the screen. A no-op when the native window or the symbol is unavailable.
pub fn set_content_frame_rate(fps: Option<f32>) {
    let app = ANDROID_APP.lock().unwrap().clone();
    let Some(app) = app else {
        return;
    };
    // The window only exists between InitWindow and TermWindow; `None` means
    // there is no surface to talk to right now (e.g. mid-teardown).
    let Some(window) = app.native_window() else {
        return;
    };
    let window = window.ptr().as_ptr() as *mut c_void;

    // 0.0 + DEFAULT (0) clears the request; FIXED_SOURCE (1) asks the system to
    // pick a display mode for this content rate.
    let (rate, compatibility) = match fps {
        Some(fps) if fps > 0.0 => (fps, 1_i8),
        _ => (0.0, 0_i8),
    };

    unsafe {
        if let Some(set) = android_set_frame_rate_with_strategy() {
            // ANATIVEWINDOW_CHANGE_FRAME_RATE_ONLY_IF_SEAMLESS = 0.
            set(window, rate, compatibility, 0);
        } else if let Some(set) = android_set_frame_rate() {
            set(window, rate, compatibility);
        }
    }
}

type SetFrameRate = unsafe extern "C" fn(*mut c_void, f32, i8) -> i32;
type SetFrameRateWithStrategy = unsafe extern "C" fn(*mut c_void, f32, i8, i8) -> i32;

/// `ANativeWindow_setFrameRate` from `libandroid.so`, or `None` below API 30.
fn android_set_frame_rate() -> Option<SetFrameRate> {
    android_symbol(c"ANativeWindow_setFrameRate")
        .map(|p| unsafe { std::mem::transmute::<*mut c_void, SetFrameRate>(p) })
}

/// `ANativeWindow_setFrameRateWithChangeStrategy` (API 31+), or `None`.
fn android_set_frame_rate_with_strategy() -> Option<SetFrameRateWithStrategy> {
    android_symbol(c"ANativeWindow_setFrameRateWithChangeStrategy")
        .map(|p| unsafe { std::mem::transmute::<*mut c_void, SetFrameRateWithStrategy>(p) })
}

/// `name` looked up in `libandroid.so`, or `None` when the symbol does not
/// exist on this device's API level.
fn android_symbol(name: &std::ffi::CStr) -> Option<*mut c_void> {
    static HANDLE: OnceLock<usize> = OnceLock::new();
    let handle = *HANDLE.get_or_init(|| unsafe {
        // bionic keeps dlopen/dlsym in libc (libdl.so is a stub). RTLD_NOW = 2.
        libc::dlopen(c"libandroid.so".as_ptr(), libc::RTLD_NOW) as usize
    });
    if handle == 0 {
        return None;
    }
    let symbol = unsafe { libc::dlsym(handle as *mut c_void, name.as_ptr()) };
    (!symbol.is_null()).then_some(symbol)
}

/// Copy `text` to the system clipboard.
///
/// Android has no NDK clipboard API, so this goes through the activity's
/// `ClipboardManager` over JNI (`getSystemService("clipboard")` →
/// `setPrimaryClip(ClipData.newPlainText(...))`). Safe to call from the native
/// UI thread: `getSystemService` and `setPrimaryClip` are thread-agnostic.
pub fn set_clipboard(text: &str) -> Result<(), String> {
    let vm_ptr = VM_PTR.load(Ordering::SeqCst);
    let act_ptr = ACTIVITY_PTR.load(Ordering::SeqCst);
    if vm_ptr.is_null() || act_ptr.is_null() {
        return Err("Android runtime pointers unavailable".into());
    }
    let vm = JavaVM::singleton().unwrap_or_else(|_| unsafe {
        // Safety: as in open_external — the VM outlives the process.
        JavaVM::from_raw(vm_ptr as *mut _)
    });
    vm.attach_current_thread(|env| -> Result<(), jni::errors::Error> {
        let ptr = act_ptr as *mut _;
        // Safety: `act_ptr` is the activity global ref, valid for the process.
        let activity = unsafe { env.as_cast_raw::<Global<NovaActivity>>(&ptr)? };
        let service = JString::from_str(env, "clipboard")?;
        let raw = activity.get_system_service(env, &service)?;
        let manager = env.cast_local::<NovaClipboardManager>(raw)?;
        let label = JString::from_str(env, "nova")?;
        let value = JString::from_str(env, text)?;
        // `newPlainText` takes `CharSequence`s; upcast the strings.
        let label_cs = env.cast_local::<NovaCharSequence>(label)?;
        let value_cs = env.cast_local::<NovaCharSequence>(value)?;
        let clip = NovaClipData::new_plain_text(env, &label_cs, &value_cs)?;
        manager.set_primary_clip(env, &clip)?;
        Ok(())
    })
    .map_err(|e| {
        alog(&format!("set_clipboard failed: {e:?}"));
        format!("clipboard failed: {e:?}")
    })?;
    Ok(())
}

/// `WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON` (no manifest permission
/// needed, unlike a bright `WakeLock`).
const FLAG_KEEP_SCREEN_ON: jint = 0x0000_0080;

/// Last requested keep-screen-on state: `0` off, `1` on. Transitions marshal
/// one flag flip to the Java main thread; steady states do nothing.
static SCREEN_ON_STATE: AtomicI8 = AtomicI8::new(0);

/// Converge the keep-screen-on flag towards `want` (playing video). Only
/// flips marshal a JNI call; safe from any thread.
pub fn converge_screen_on(want: bool) {
    let desired: i8 = if want { 1 } else { 0 };
    if SCREEN_ON_STATE.swap(desired, Ordering::SeqCst) == desired {
        return;
    }
    let app = ANDROID_APP.lock().unwrap().clone();
    let Some(app) = app else {
        return;
    };
    app.run_on_java_main_thread(Box::new(move || {
        if let Err(e) = apply_screen_on(desired == 1) {
            alog(&format!("keep screen on failed: {e}"));
        }
    }));
}

/// Add/clear `FLAG_KEEP_SCREEN_ON` on the activity window. Runs on the Java
/// main thread (`Window` methods are UI-thread only).
fn apply_screen_on(keep: bool) -> Result<(), String> {
    let vm_ptr = VM_PTR.load(Ordering::SeqCst);
    let act_ptr = ACTIVITY_PTR.load(Ordering::SeqCst);
    if vm_ptr.is_null() || act_ptr.is_null() {
        return Err("Android runtime pointers unavailable".into());
    }
    let vm = JavaVM::singleton().unwrap_or_else(|_| unsafe {
        // Safety: as in open_external — the VM outlives the process.
        JavaVM::from_raw(vm_ptr as *mut _)
    });
    vm.attach_current_thread(|env| -> Result<(), jni::errors::Error> {
        let ptr = act_ptr as *mut _;
        // Safety: `act_ptr` is the activity global ref, valid for the process.
        let activity = unsafe { env.as_cast_raw::<Global<NovaActivity>>(&ptr)? };
        let window = activity.get_window(env)?;
        if keep {
            window.add_flags(env, FLAG_KEEP_SCREEN_ON)?;
        } else {
            window.clear_flags(env, FLAG_KEEP_SCREEN_ON)?;
        }
        Ok(())
    })
    .map_err(|e| format!("{e:?}"))?;
    Ok(())
}

/// Background the app without destroying it (system back on the root
/// screen). `Activity.moveTaskToBack` keeps the process — sync engine, back
/// stack and UI state — alive, so returning restores the same screen instead
/// of relaunching. Safe to call from the Slint UI thread (`moveTaskToBack`
/// is thread-agnostic, like the clipboard calls above).
pub fn move_task_to_back() {
    let vm_ptr = VM_PTR.load(Ordering::SeqCst);
    let act_ptr = ACTIVITY_PTR.load(Ordering::SeqCst);
    if vm_ptr.is_null() || act_ptr.is_null() {
        return;
    }
    let vm = JavaVM::singleton().unwrap_or_else(|_| unsafe {
        // Safety: as in open_external — the VM outlives the process.
        JavaVM::from_raw(vm_ptr as *mut _)
    });
    if let Err(e) = vm.attach_current_thread(|env| -> Result<(), jni::errors::Error> {
        let ptr = act_ptr as *mut _;
        // Safety: `act_ptr` is the activity global ref, valid for the process.
        let activity = unsafe { env.as_cast_raw::<Global<NovaActivity>>(&ptr)? };
        activity.move_task_to_back(env, true)?;
        Ok(())
    }) {
        alog(&format!("move_task_to_back failed: {e:?}"));
    }
}

/// The device model (e.g. "Pixel 8"), or an empty string when unavailable.
/// Used as the default sync device name, overridable in Settings.
pub fn device_model() -> String {
    let vm_ptr = VM_PTR.load(Ordering::SeqCst);
    if vm_ptr.is_null() {
        return String::new();
    }
    let vm = JavaVM::singleton().unwrap_or_else(|_| unsafe {
        // Safety: as in open_external — the VM outlives the process.
        JavaVM::from_raw(vm_ptr as *mut _)
    });
    let mut model = String::new();
    let _ = vm.attach_current_thread(|env| -> Result<(), jni::errors::Error> {
        let value = NovaBuild::model(env)?;
        model = value.to_string();
        Ok(())
    });
    model
}

/// Open `url` in an external **video player** via `ACTION_VIEW`.
///
/// The fallback for streams the in-app mpv player cannot take: the system
/// picks the app, and the explicit MIME type keeps that choice among video
/// players instead of the browser.
pub fn open_external(url: &str) -> Result<(), String> {
    // HLS playlists need their own MIME type or video players won't claim
    // the intent; everything else goes out as generic video.
    let mime = if url.split('?').next().unwrap_or(url).ends_with(".m3u8") {
        "application/x-mpegURL"
    } else {
        "video/*"
    };
    let vm_ptr = VM_PTR.load(Ordering::SeqCst);
    let act_ptr = ACTIVITY_PTR.load(Ordering::SeqCst);
    if vm_ptr.is_null() || act_ptr.is_null() {
        return Err("Android runtime pointers unavailable".into());
    }
    let vm = JavaVM::singleton().unwrap_or_else(|_| unsafe {
        // Safety: documented android-activity pattern; the VM outlives
        // the process and from_raw only initializes the singleton.
        JavaVM::from_raw(vm_ptr as *mut _)
    });
    vm.attach_current_thread(|env| -> Result<(), jni::errors::Error> {
        let ptr = act_ptr as *mut _;
        // Safety: the stashed pointer is the activity global ref, valid
        // for the whole process; the cast only borrows it for this call.
        let activity = unsafe { env.as_cast_raw::<Global<NovaActivity>>(&ptr)? };
        let action = JString::from_str(env, "android.intent.action.VIEW")?;
        let raw = JString::from_str(env, url)?;
        let uri = NovaUri::parse(env, &raw)?;
        let intent = NovaIntent::new(env, &action, &uri)?;
        let mime_str = JString::from_str(env, mime)?;
        // setDataAndType returns the same intent (builder style); the
        // return value is discarded.
        let _ = intent.set_data_and_type(env, &uri, &mime_str)?;
        activity.start_activity(env, &intent)?;
        Ok(())
    })
    .map_err(|e| {
        alog(&format!("open_external failed: {e:?}"));
        format!("no video player can open this stream ({e:?})")
    })?;
    alog("open_external: video intent fired");
    Ok(())
}

/// `Intent.FLAG_ACTIVITY_NEW_TASK`: start the target in its own task. Set on
/// link intents: an implicit launch must not depend on the calling task being
/// the resolved one, and the flag is required whenever the Context handed to
/// `startActivity` is not an Activity (the app context, see the QR scanner).
const FLAG_ACTIVITY_NEW_TASK: jint = 0x1000_0000;

/// Open `url` in the system's **browser** via `ACTION_VIEW`.
///
/// Used for links the app only points at — the addon configuration pages
/// (`<addon base>/configure`) that Settings → Addons → Configure opens. Unlike
/// [`open_external`] no MIME type is attached: with an `http(s)` URI and no
/// explicit type, the intent resolves to whatever handles web links (the
/// browser or a custom tab), which an explicit `video/*` would exclude. The
/// link is additionally declared `BROWSABLE`, so only apps that themselves
/// handle web links can claim it.
pub fn open_browser(url: &str) -> Result<(), String> {
    let vm_ptr = VM_PTR.load(Ordering::SeqCst);
    let act_ptr = ACTIVITY_PTR.load(Ordering::SeqCst);
    if vm_ptr.is_null() || act_ptr.is_null() {
        return Err("Android runtime pointers unavailable".into());
    }
    let vm = JavaVM::singleton().unwrap_or_else(|_| unsafe {
        // Safety: documented android-activity pattern; the VM outlives
        // the process and from_raw only initializes the singleton.
        JavaVM::from_raw(vm_ptr as *mut _)
    });
    vm.attach_current_thread(|env| -> Result<(), jni::errors::Error> {
        let ptr = act_ptr as *mut _;
        // Safety: the stashed pointer is the activity global ref, valid
        // for the whole process; the cast only borrows it for this call.
        let activity = unsafe { env.as_cast_raw::<Global<NovaActivity>>(&ptr)? };
        let action = JString::from_str(env, "android.intent.action.VIEW")?;
        let raw = JString::from_str(env, url)?;
        let uri = NovaUri::parse(env, &raw)?;
        let intent = NovaIntent::new(env, &action, &uri)?;
        let browsable = JString::from_str(env, "android.intent.category.BROWSABLE")?;
        let _ = intent.add_category(env, &browsable)?;
        let _ = intent.add_flags(env, FLAG_ACTIVITY_NEW_TASK)?;
        activity.start_activity(env, &intent)?;
        Ok(())
    })
    .map_err(|e| {
        alog(&format!("open_browser failed: {e:?}"));
        format!("no app can open this link ({e:?})")
    })?;
    alog("open_browser: link intent fired");
    Ok(())
}

/// `View.SYSTEM_UI_FLAG_*` bits for immersive-sticky mode. Used on API 26–29
/// (where `WindowInsetsController` does not exist); API 30+ hides the same two
/// bars through the controller instead.
// Keep catalog backgrounds under visible bars; Slint forwards their insets
// so pages can protect text and controls independently from the artwork.
const EDGE_TO_EDGE: jint = 0x0000_0100 | 0x0000_0200 | 0x0000_0400;
const IMMERSIVE_STICKY: jint = 0x0000_0002 // SYSTEM_UI_FLAG_HIDE_NAVIGATION
    | 0x0000_0004 // SYSTEM_UI_FLAG_FULLSCREEN
    | EDGE_TO_EDGE
    | 0x0000_1000; // SYSTEM_UI_FLAG_IMMERSIVE_STICKY

/// `WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE`: a swipe
/// reveals the bars temporarily, then they auto-hide again.
const BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE: jint = 2;

/// Request the Android system bars — the status bar and the navigation bar —
/// be hidden (immersive) or restored around in-app playback.
///
/// Only records the desired state and posts an apply; the actual window call
/// happens on the Java main thread (see [`enqueue_apply`]). Applying separately
/// from the request is what makes the state converge:
/// [`reassert_system_bars`] keeps retrying until the change is confirmed, which
/// is what fixes bars sticking around after the player closes and bars not
/// always hiding when it opens.
///
/// Slint's Android backend only adjusts the safe-area insets when
/// `Window::set_fullscreen` flips; it never hides the bars themselves, so the
/// player drives this directly. API 30+ goes through
/// `WindowInsetsController.hide` (the supported path, notably under the
/// edge-to-edge enforcement of Android 15 / target SDK 35); API 26–29 falls
/// back to the deprecated `View.setSystemUiVisibility` immersive flags.
pub fn set_system_bars_hidden(hidden: bool) -> Result<(), String> {
    BARS_WANT.store(if hidden { 1 } else { 0 }, Ordering::SeqCst);
    enqueue_apply();
    Ok(())
}

/// Drive the bars towards the last requested state and keep tracking window
/// focus. Safe and cheap to call from the player tick.
///
/// Once the requested state is confirmed this stops touching the window: a
/// repeated `hide()` while the framework shows the bars transiently (after an
/// edge swipe) keeps resetting the system's auto-hide timeout, so the bars never
/// disappear. Retrying only while unconfirmed still covers the flaky first hide
/// when the insets controller is not attached yet. The periodic focus probe is
/// what recovers a hidden state after the framework re-shows the bars on a real
/// focus change.
pub fn reassert_system_bars() -> Result<(), String> {
    let want = BARS_WANT.load(Ordering::SeqCst);
    // Only immersive playback needs the focus probe (and it is what would need
    // to re-hide); a shown state just retries until it sticks and goes quiet.
    if want == 1 && FOCUS_POLL_TICKS.fetch_add(1, Ordering::Relaxed) + 1 >= FOCUS_POLL_EVERY {
        FOCUS_POLL_TICKS.store(0, Ordering::Relaxed);
        check_window_focus();
    }
    if BARS_STATE.load(Ordering::SeqCst) == want {
        return Ok(());
    }
    enqueue_apply();
    Ok(())
}

/// Probe `View.hasWindowFocus()` on the Java main thread; on the false→true
/// edge while immersive, invalidate the confirmed state so the next
/// [`reassert_system_bars`] re-hides the bars the framework restored.
///
/// Bars shown transiently by a swipe do not remove window focus, so this never
/// fights the transient state — it only fires after a real focus loss/regain
/// (notification shade, recents, dialogs).
fn check_window_focus() {
    let app = ANDROID_APP.lock().unwrap().clone();
    let Some(app) = app else {
        return;
    };
    app.run_on_java_main_thread(Box::new(|| match read_window_focus() {
        Ok(focused) => {
            let was = FOCUS_LAST.swap(if focused { 1 } else { 0 }, Ordering::SeqCst);
            if focused && was == 0 {
                // Focus came back: force the next tick to re-apply.
                BARS_STATE.store(0, Ordering::SeqCst);
                alog("system UI: window refocused; re-asserting bars");
            }
        }
        Err(e) => alog(&format!("system UI: {e}")),
    }));
}

/// Read the activity decor view's window focus. Runs on the Java main thread.
fn read_window_focus() -> Result<bool, String> {
    let vm_ptr = VM_PTR.load(Ordering::SeqCst);
    let act_ptr = ACTIVITY_PTR.load(Ordering::SeqCst);
    if vm_ptr.is_null() || act_ptr.is_null() {
        return Err("Android runtime pointers unavailable".into());
    }
    let vm = JavaVM::singleton().unwrap_or_else(|_| unsafe {
        // Safety: as in open_external — the VM outlives the process.
        JavaVM::from_raw(vm_ptr as *mut _)
    });
    let mut focused = false;
    vm.attach_current_thread(|env| -> Result<(), jni::errors::Error> {
        let ptr = act_ptr as *mut _;
        // Safety: `act_ptr` is the activity global ref, valid for the process.
        let activity = unsafe { env.as_cast_raw::<Global<NovaActivity>>(&ptr)? };
        let window = activity.get_window(env)?;
        let decor = window.get_decor_view(env)?;
        focused = decor.has_window_focus(env)?;
        Ok(())
    })
    .map_err(|e| format!("focus probe failed: {e:?}"))?;
    Ok(focused)
}

/// Post one system-bar apply to the Java main thread.
///
/// `Window`, `WindowInsetsController` and `View` must be touched on the UI
/// thread; `android_main` (and with it Slint's event loop and every `tick`)
/// runs on a separate native thread, so calling them here directly is what left
/// the bars untouched on the first open and stuck hidden after close. The
/// closure only reads the [`BARS_WANT`] atomic, so posting is cheap and always
/// converges on the latest request.
fn enqueue_apply() {
    let app = ANDROID_APP.lock().unwrap().clone();
    let Some(app) = app else {
        return;
    };
    app.run_on_java_main_thread(Box::new(|| {
        if let Err(e) = apply_system_bars() {
            alog(&format!("system UI: {e}"));
        }
    }));
}

/// Apply [`BARS_WANT`] through the API the running Android version supports and
/// record the confirmed state. Runs on the Java main thread. Returns `Err`
/// without recording anything when the insets controller is not attached yet,
/// so [`reassert_system_bars`] retries.
fn apply_system_bars() -> Result<(), String> {
    let hidden = BARS_WANT.load(Ordering::SeqCst) == 1;
    let desired: i8 = if hidden { 1 } else { 0 };
    let vm_ptr = VM_PTR.load(Ordering::SeqCst);
    let act_ptr = ACTIVITY_PTR.load(Ordering::SeqCst);
    if vm_ptr.is_null() || act_ptr.is_null() {
        return Err("Android runtime pointers unavailable".into());
    }
    let vm = JavaVM::singleton().unwrap_or_else(|_| unsafe {
        // Safety: as in open_external — the VM outlives the process.
        JavaVM::from_raw(vm_ptr as *mut _)
    });

    let mut applied = false;
    let mut not_ready = false;
    vm.attach_current_thread(|env| -> Result<(), jni::errors::Error> {
        let ptr = act_ptr as *mut _;
        // Safety: `act_ptr` is the activity global ref, valid for the process.
        let activity = unsafe { env.as_cast_raw::<Global<NovaActivity>>(&ptr)? };
        let window = activity.get_window(env)?;

        // `setDecorFitsSystemWindows` re-dispatches insets and fights the
        // controller, so it only runs when the state actually flips. The
        // system-bars *behavior* is instead (re)applied on every hide: a swipe
        // reveal is transient only while `BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE`
        // is in effect, and the framework can drop it across focus changes.
        let transition = BARS_STATE.load(Ordering::SeqCst) != desired;
        if transition {
            // A transparent status bar reveals Home's backdrop on API 26–34
            // too; Android 15 enforces this for our target SDK already.
            window.add_flags(env, 0x8000_0000_u32 as jint)?; // FLAG_DRAWS_SYSTEM_BAR_BACKGROUNDS
            window.clear_flags(env, 0x0400_0000)?; // FLAG_TRANSLUCENT_STATUS
            window.set_status_bar_color(env, 0)?;
        }

        // API 30+: the supported path. On API 26–29 `getInsetsController` does
        // not exist, so the lookup errors and the legacy flags below do the
        // work. A present-but-null controller means the view is not attached
        // yet: report it so the next re-assert retries instead of pretending
        // the change went through.
        match window.get_insets_controller(env) {
            Ok(controller) if !controller.is_null() => {
                let types = NovaWindowInsetsType::system_bars(env)?;
                if transition {
                    // Restoring visible bars must retain edge-to-edge drawing.
                    // Safe-area insets still protect the catalog's controls.
                    if let Err(e) = window.set_decor_fits_system_windows(env, false) {
                        alog(&format!(
                            "system UI: setDecorFitsSystemWindows failed: {e:?}"
                        ));
                    }
                }
                if hidden {
                    // Set the behavior immediately before the hide so that, if
                    // it was lost (focus reset, fullscreen flip), a swipe still
                    // reveals the bars transiently and lets them auto-hide.
                    if let Err(e) = controller
                        .set_system_bars_behavior(env, BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE)
                    {
                        alog(&format!("system UI: setSystemBarsBehavior failed: {e:?}"));
                    }
                    controller.hide(env, types)?;
                } else {
                    controller.show(env, types)?;
                }
                applied = true;
            }
            Ok(_) => not_ready = true,
            Err(_) => {
                // API 26–29: the legacy immersive flags are the only mechanism.
                let decor = window.get_decor_view(env)?;
                let flags = if hidden {
                    IMMERSIVE_STICKY
                } else {
                    EDGE_TO_EDGE
                };
                decor.set_system_ui_visibility(env, flags)?;
                applied = true;
            }
        }
        Ok(())
    })
    .map_err(|e| {
        alog(&format!("system UI: apply(hidden={hidden}) failed: {e:?}"));
        format!("apply_system_bars(hidden={hidden}) failed: {e:?}")
    })?;

    if not_ready {
        return Err("insets controller not attached yet".into());
    }
    if applied && BARS_STATE.swap(desired, Ordering::SeqCst) != desired {
        alog(&format!(
            "system UI: bars {}",
            if hidden { "hidden" } else { "shown" }
        ));
    }
    Ok(())
}

// The Android classes the intent needs. Slint's android backend binds its own
// copies for its helper; these are local to the ACTION_VIEW dance above and to
// the immersive-mode call in `set_system_bars_hidden`.

bind_java_type! {
    NovaActivity => "android.app.Activity",
    type_map = {
        NovaIntent => "android.content.Intent",
        NovaWindow => "android.view.Window",
    },
    methods {
        fn start_activity { name = "startActivity", sig = (intent: NovaIntent), },
        fn get_window { name = "getWindow", sig = () -> NovaWindow, },
        fn get_system_service { name = "getSystemService", sig = (name: JString) -> JObject, },
        fn move_task_to_back { name = "moveTaskToBack", sig = (non_root: jboolean) -> jboolean, },
    }
}

bind_java_type! {
    NovaWindow => "android.view.Window",
    type_map = {
        NovaView => "android.view.View",
        NovaWindowInsetsController => "android.view.WindowInsetsController",
    },
    methods {
        fn get_decor_view { name = "getDecorView", sig = () -> NovaView, },
        fn get_insets_controller { name = "getInsetsController", sig = () -> NovaWindowInsetsController, },
        fn set_status_bar_color { name = "setStatusBarColor", sig = (color: jint), },
        fn set_decor_fits_system_windows { name = "setDecorFitsSystemWindows", sig = (decor_fits: jboolean), },
        fn add_flags { name = "addFlags", sig = (mask: jint), },
        fn clear_flags { name = "clearFlags", sig = (mask: jint), },
    }
}

bind_java_type! {
    NovaView => "android.view.View",
    methods {
        fn set_system_ui_visibility { name = "setSystemUiVisibility", sig = (visibility: jint), },
        fn has_window_focus { name = "hasWindowFocus", sig = () -> jboolean, },
    }
}

bind_java_type! {
    NovaWindowInsetsController => "android.view.WindowInsetsController",
    methods {
        fn hide { name = "hide", sig = (types: jint), },
        fn show { name = "show", sig = (types: jint), },
        fn set_system_bars_behavior { name = "setSystemBarsBehavior", sig = (behavior: jint), },
    }
}

bind_java_type! {
    NovaWindowInsetsType => "android.view.WindowInsets$Type",
    methods {
        static fn status_bars { name = "statusBars", sig = () -> jint, },
        static fn navigation_bars { name = "navigationBars", sig = () -> jint, },
        static fn system_bars { name = "systemBars", sig = () -> jint, },
    }
}

bind_java_type! {
    NovaUri => "android.net.Uri",
    methods {
        static fn parse { name = "parse", sig = (uri_string: JString) -> NovaUri, },
    }
}

bind_java_type! {
    NovaIntent => "android.content.Intent",
    type_map = {
        NovaUri => "android.net.Uri",
    },
    constructors {
        fn new { sig = (action: JString, uri: NovaUri), },
    },
    methods {
        fn set_data_and_type { name = "setDataAndType", sig = (uri: NovaUri, mime: JString) -> NovaIntent, },
        fn add_category { name = "addCategory", sig = (category: JString) -> NovaIntent, },
        fn add_flags { name = "addFlags", sig = (flags: jint) -> NovaIntent, },
    },
}

// Clipboard support (Settings → Sync "Copy"): the NDK has no clipboard API, so
// the endpoint id is handed to the activity's `ClipboardManager` through a
// `ClipData` (both obtained over JNI).
bind_java_type! {
    NovaClipboardManager => "android.content.ClipboardManager",
    type_map = {
        NovaClipData => "android.content.ClipData",
    },
    methods {
        fn set_primary_clip { name = "setPrimaryClip", sig = (clip: NovaClipData), },
    }
}

// `newPlainText` takes `CharSequence`s; this bound type gives the JNI
// descriptor the right parameter type, and is the cast target for the strings
// handed to it. It is also listed in `NovaClipData`'s `type_map` below, which is
// what lets the signature macro resolve the name.
bind_java_type! {
    NovaCharSequence => "java.lang.CharSequence",
}

bind_java_type! {
    NovaClipData => "android.content.ClipData",
    type_map = {
        NovaCharSequence => "java.lang.CharSequence",
    },
    methods {
        static fn new_plain_text {
            name = "newPlainText",
            sig = (label: NovaCharSequence, text: NovaCharSequence) -> NovaClipData,
        },
    }
}

// Connectivity polling (Android doesn't surface network changes to native
// code): the active `Network` handle is read on a timer and changes are
// forwarded to iroh via `Endpoint::network_change` (see [`network_changed`]).
bind_java_type! {
    NovaConnectivityManager => "android.content.ConnectivityManager",
    type_map = {
        NovaNetwork => "android.net.Network",
    },
    methods {
        fn get_active_network { name = "getActiveNetwork", sig = () -> NovaNetwork, },
    }
}

bind_java_type! {
    NovaNetwork => "android.net.Network",
    methods {
        fn get_network_handle { name = "getNetworkHandle", sig = () -> jlong, },
    }
}

// Device model for the default sync name (Settings → Sync).
bind_java_type! {
    NovaBuild => "android.os.Build",
    fields {
        static model { name = "MODEL", sig = JString },
    }
}
