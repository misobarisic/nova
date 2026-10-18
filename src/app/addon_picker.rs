//! Native local-addon selection; only bounded bytes cross into the installer.
use super::Bridge;
use nova_ui::backend_text as text;
use std::sync::Arc;

const MAX_ZIP_BYTES: usize = 8 * 1024 * 1024;

pub(super) fn pick_zip() {
    super::bridge::with_global_bridge(|bridge| {
        let Some(app) = bridge.app() else { return };
        if app.get_addon_zip_import_busy() {
            return;
        }
        app.set_addon_zip_import_busy(true);
        app.set_addon_zip_import_status(text::tr("Choose an addon ZIP file…").into());
        launch(bridge.clone());
    });
}

/// A delayed picker must never install into a replacement application root.
fn deliver(bridge: Bridge, result: Result<Option<Vec<u8>>, &'static str>) {
    let _ = crate::app::profiles::invoke_from_event_loop(move || {
        let current = super::bridge::with_global_bridge(|current| {
            Arc::ptr_eq(&current.runtime, &bridge.runtime)
        }) == Some(true);
        if !current || bridge.app().is_none() {
            return;
        }
        match result {
            Ok(Some(bytes)) => bridge.import_addon_zip(bytes),
            Ok(None) => {
                if let Some(app) = bridge.app() {
                    app.set_addon_zip_import_busy(false);
                    app.set_addon_zip_import_status("".into());
                }
            }
            Err(message) => bridge.addon_zip_import_failed(message),
        }
    });
}

#[cfg(not(target_os = "android"))]
fn launch(bridge: Bridge) {
    crate::app::profiles::thread::spawn(move || {
        use std::io::Read;
        let Some(path) = rfd::FileDialog::new()
            .set_title(text::tr("Choose an addon ZIP file…"))
            .add_filter("ZIP", &["zip"])
            .pick_file()
        else {
            deliver(bridge, Ok(None));
            return;
        };
        let status_bridge = bridge.clone();
        let _ = crate::app::profiles::invoke_from_event_loop(move || {
            if let Some(app) = status_bridge.app() {
                app.set_addon_zip_import_status(text::tr("Reading addon ZIP…").into());
            }
        });
        let result = (|| {
            let file = std::fs::File::open(path).map_err(|_| "The addon ZIP could not be read")?;
            let mut bytes = Vec::new();
            file.take((MAX_ZIP_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|_| "The addon ZIP could not be read")?;
            if bytes.len() > MAX_ZIP_BYTES {
                return Err("The addon ZIP exceeds the 8 MiB size limit");
            }
            Ok(Some(bytes))
        })();
        deliver(bridge, result);
    });
}

#[cfg(target_os = "android")]
mod android {
    use super::*;
    use jni::objects::{GlobalRef, JByteArray, JObject, JValue};
    use jni::sys::{jint, jlong};
    use std::sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    };

    const CLASS: &str = "dev.misob.nova.AddonImportActivity";
    static CLASS_REF: OnceLock<GlobalRef> = OnceLock::new();
    static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);
    struct Pending {
        request: u64,
        bridge: Bridge,
        context: crate::app::profiles::Context,
    }
    static PENDING: Mutex<Option<Pending>> = Mutex::new(None);

    pub(super) fn launch(bridge: Bridge) {
        let request = NEXT_REQUEST.fetch_add(1, Ordering::Relaxed);
        *PENDING.lock().unwrap() = Some(Pending {
            request,
            bridge,
            context: crate::app::profiles::capture(),
        });
        let launched = super::super::android_bg::with_app_context(|env, raw| {
            let context = unsafe { JObject::from_raw(raw) };
            let class = super::super::android_bg::app_class(env, &context, CLASS, &CLASS_REF)?;
            env.call_static_method(
                &class,
                "start",
                "(Landroid/content/Context;J)V",
                &[JValue::Object(&context), JValue::Long(request as jlong)],
            )?;
            Ok(())
        })
        .is_some();
        if !launched {
            complete(request, Err("The addon file picker could not be opened"));
        }
    }

    fn complete(request: u64, result: Result<Option<Vec<u8>>, &'static str>) {
        let pending = {
            let mut pending = PENDING.lock().unwrap();
            if pending
                .as_ref()
                .is_none_or(|pending| pending.request != request)
            {
                return;
            }
            pending.take()
        };
        if let Some(pending) = pending {
            // JNI runs on a foreign thread. Preserve the profile that opened
            // the picker so its queued completion is discarded after a switch.
            crate::app::profiles::with_context(pending.context, || {
                deliver(pending.bridge, result);
            });
        }
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_misob_nova_AddonImportActivity_nativeResult(
        env: jni::JNIEnv,
        _class: JObject,
        request: jlong,
        bytes: JByteArray,
        outcome: jint,
    ) {
        let result = match outcome {
            0 => Ok(None),
            1 => {
                // Check Java's size before JNI allocates a Rust copy as well.
                match env.get_array_length(&bytes) {
                    Ok(size) if size as usize <= MAX_ZIP_BYTES => env
                        .convert_byte_array(&bytes)
                        .map(Some)
                        .map_err(|_| "The addon ZIP could not be read"),
                    _ => Err("The addon ZIP exceeds the 8 MiB size limit"),
                }
            }
            2 => Err("The addon ZIP exceeds the 8 MiB size limit"),
            _ => Err("The addon ZIP could not be read"),
        };
        complete(request as u64, result);
    }
}

#[cfg(target_os = "android")]
use android::launch;
