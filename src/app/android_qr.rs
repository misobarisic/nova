//! Android camera QR scanner glue.
//!
//! [`QrScanActivity`] (`android/java/dev/misob/nova/QrScanActivity.java`) owns
//! the camera entirely in Java — runtime permission, Camera2 session, preview,
//! and an `ImageReader` — because `cargo-apk2` compiles Java against a fixed
//! classpath (android.jar only), so no scanning library can be bundled. This
//! module does the one thing Java cannot do cheaply: decode the QR from the
//! camera's luma plane with `rqrr`.
//!
//! The Java side calls [`Java_dev_misob_nova_QrScanActivity_nativeOnFrame`] for
//! each throttled frame, passing the Y plane, dimensions and row stride. A
//! successful decode is validated against the invite-ticket codec and marshalled
//! onto the UI thread, where the normal join path runs; the activity closes
//! itself when the native call reports a hit.
#![cfg(target_os = "android")]

use jni::objects::{GlobalRef, JByteArray, JObject, JValue};
use jni::sys::{JNI_FALSE, JNI_TRUE, jboolean, jint};
use std::sync::OnceLock;

const SCAN_CLASS: &str = "dev.misob.nova.QrScanActivity";
/// Cached global ref to the scanner activity class (loaded through the app
/// class loader; see `android_bg::app_class`).
static SCAN_CLASS_REF: OnceLock<GlobalRef> = OnceLock::new();

/// Launch the scanner activity. Called on the UI thread from Settings → Sync;
/// the activity decodes a ticket and calls `sync_join_invite` itself.
pub(crate) fn start_scan() {
    super::android_bg::with_app_context(|env, raw| {
        let context = unsafe { JObject::from_raw(raw) };
        let class = super::android_bg::app_class(env, &context, SCAN_CLASS, &SCAN_CLASS_REF)?;
        env.call_static_method(
            &class,
            "start",
            "(Landroid/content/Context;)V",
            &[JValue::Object(&context)],
        )?;
        Ok(())
    });
}

/// JNI entry: one camera frame's luma plane. Returns `true` when it decoded a
/// valid invite ticket (the Java side then finishes the activity).
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_misob_nova_QrScanActivity_nativeOnFrame(
    env: jni::JNIEnv,
    _this: JObject,
    frame: JByteArray,
    width: jint,
    height: jint,
    row_stride: jint,
) -> jboolean {
    let bytes = match env.convert_byte_array(&frame) {
        Ok(bytes) => bytes,
        Err(_) => return JNI_FALSE,
    };
    let (width, height, stride) = (width as usize, height as usize, row_stride as usize);
    let Some(ticket) = decode_frame(&bytes, width, height, stride) else {
        return JNI_FALSE;
    };
    // Decoding happens on the camera's ImageReader thread; the engine and
    // Slint must be touched on the UI thread.
    let _ = slint::invoke_from_event_loop(move || {
        crate::app::bridge::with_global_bridge(|bridge| bridge.sync_join_invite(&ticket));
    });
    JNI_TRUE
}

/// Decode the QR in one YUV luma plane. `row_stride` is the camera's row pitch
/// in bytes (typically ≥ `width`), so we index `y * row_stride + x` rather than
/// assuming a tightly packed plane.
fn decode_frame(bytes: &[u8], width: usize, height: usize, row_stride: usize) -> Option<String> {
    if width == 0 || height == 0 || row_stride < width || bytes.len() < row_stride * height {
        return None;
    }
    let mut image = rqrr::PreparedImage::prepare_from_greyscale(width, height, |x, y| {
        bytes[y * row_stride + x]
    });
    for grid in image.detect_grids() {
        if let Ok((_, content)) = grid.decode() {
            // Only accept our own invite format, so scanning an unrelated QR
            // (or a stray symbol) never feeds garbage into the join path.
            if nova_sync::decode_ticket(&content).is_ok() {
                return Some(content);
            }
        }
    }
    None
}
