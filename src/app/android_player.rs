//! Android player-gesture system bridges: swipe volume/brightness.
//!
//! The backdrop swipes drive platform state, not mpv: volume steps the music
//! stream through any `Context` (the system volume panel is the readout, so
//! nothing is mirrored back); brightness steps the activity window itself,
//! so it takes the stashed `NativeActivity` from `nova-player` — the
//! `ndk-context` `Context` is not necessarily an `Activity`, which is why an
//! earlier revision silently no-op'd behind an `instanceof` check. Brightness
//! is session-scoped, handed back to the system default on close. All Android
//! API calls live in `PlayerFx` (`android/java/dev/misob/nova/PlayerFx.java`);
//! Rust only loads the class through the app class loader and calls static
//! methods (see `android_bg`).
#![cfg(target_os = "android")]

use std::sync::Mutex;
use std::sync::OnceLock;

use jni::objects::{GlobalRef, JObject, JValue};

const FX_CLASS: &str = "dev.misob.nova.PlayerFx";
/// Cached global ref to the effects class (loaded once through the app class
/// loader; avoids a loader round-trip on every swipe notch).
static FX_CLASS_REF: OnceLock<GlobalRef> = OnceLock::new();

/// Session brightness (0.05–1.0) once a swipe sets it; `None` until then (the
/// window follows the system default). Reset on player close so a dimmed
/// window never leaks into the catalog.
static SESSION_BRIGHTNESS: Mutex<Option<f32>> = Mutex::new(None);

/// Brightness travel per swipe notch (40px of vertical drag).
const BRIGHTNESS_PER_NOTCH: f32 = 0.05;

/// The stashed activity raw pointer, or `None` before `android_main`.
fn activity_ptr() -> Option<*mut std::ffi::c_void> {
    let act = crate::player::android_activity_ptr();
    (!act.is_null()).then_some(act)
}

/// Step the music-stream volume (`dir` = ±1 per swipe notch). Binder call,
/// safe from the Slint UI thread; the system volume panel is the readout.
pub(crate) fn player_volume_step(dir: i32) {
    let dir = dir.signum();
    if dir == 0 {
        return;
    }
    let _ = super::android_bg::with_app_context(|env, raw| {
        let context = unsafe { JObject::from_raw(raw) };
        let class = super::android_bg::app_class(env, &context, FX_CLASS, &FX_CLASS_REF)?;
        env.call_static_method(
            &class,
            "adjustVolume",
            "(Landroid/content/Context;I)V",
            &[JValue::Object(&context), JValue::Int(dir)],
        )?;
        Ok(())
    });
}

/// Step the session brightness (`steps` notches). Returns the new level for
/// the Slint readout. The first touch reads the live window value (a system
/// default of `-1` starts from 0.5); the player close resets to the default.
pub(crate) fn player_brightness_step(steps: i32) -> f32 {
    if steps == 0 {
        return SESSION_BRIGHTNESS.lock().unwrap().unwrap_or(0.5);
    }
    let mut slot = SESSION_BRIGHTNESS.lock().unwrap();
    let base = slot.unwrap_or_else(read_brightness);
    let next = (base + steps as f32 * BRIGHTNESS_PER_NOTCH).clamp(0.05, 1.0);
    *slot = Some(next);
    set_brightness(next);
    next
}

/// Forget the session brightness and hand the window back to the system
/// default. Called when the player closes.
pub(crate) fn player_brightness_reset() {
    *SESSION_BRIGHTNESS.lock().unwrap() = None;
    set_brightness(-1.0);
}

/// Live window brightness (0..1), or 0.5 when unreadable or following the
/// system default. A plain field read; safe off the Java main thread.
fn read_brightness() -> f32 {
    let Some(act) = activity_ptr() else {
        return 0.5;
    };
    let level = super::android_bg::with_app_context(|env, _raw| {
        // Safety: the stashed activity global ref, valid for the process
        // (see `nova-player`'s `set_android_runtime`); borrowed per call.
        let activity = unsafe { JObject::from_raw(act as *mut _) };
        let class = super::android_bg::app_class(env, &activity, FX_CLASS, &FX_CLASS_REF)?;
        let value = env.call_static_method(
            &class,
            "getBrightness",
            "(Landroid/app/Activity;)F",
            &[JValue::Object(&activity)],
        )?;
        Ok(value.f()?)
    });
    level.filter(|v| v.is_finite() && *v > 0.0).unwrap_or(0.5)
}

/// Push a brightness level to the window (-1 = system default). The Java
/// side marshals onto the main looper; `Window` methods are UI-thread only.
fn set_brightness(level: f32) {
    let Some(act) = activity_ptr() else {
        return;
    };
    let _ = super::android_bg::with_app_context(|env, _raw| {
        // Safety: see `read_brightness`.
        let activity = unsafe { JObject::from_raw(act as *mut _) };
        let class = super::android_bg::app_class(env, &activity, FX_CLASS, &FX_CLASS_REF)?;
        env.call_static_method(
            &class,
            "setBrightness",
            "(Landroid/app/Activity;F)V",
            &[JValue::Object(&activity), JValue::Float(level)],
        )?;
        Ok(())
    });
}
