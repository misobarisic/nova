//! Constrained no-Activity JobScheduler adapter. Never starts an FGS.
#![cfg(target_os = "android")]
use super::*;
use jni::objects::{GlobalRef, JObject, JValue};
use std::sync::OnceLock;
static CLASS: OnceLock<GlobalRef> = OnceLock::new();
static CANCEL: AtomicBool = AtomicBool::new(false);
pub(super) fn schedule(device: &crate::download::offline::DevicePolicy) {
    android_bg::with_app_context(|env, raw| {
        let context = unsafe { JObject::from_raw(raw) };
        let class = android_bg::app_class(
            env,
            &context,
            "dev.misob.nova.NovaOfflineJobService",
            &CLASS,
        )?;
        env.call_static_method(
            class,
            "configure",
            "(Landroid/content/Context;ZZ)V",
            &[
                JValue::Object(&context),
                JValue::Bool((!device.enabled_profiles.is_empty()).into()),
                JValue::Bool(device.charging_only.into()),
            ],
        )?;
        Ok(())
    });
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_misob_nova_NovaOfflineJobService_nativeEnvironment(
    _env: jni::JNIEnv,
    _class: jni::objects::JClass,
    flags: jni::sys::jint,
) {
    smart_offline::set_environment(crate::download::offline::Environment {
        connected: flags & 1 != 0,
        wifi_or_ethernet: flags & 2 != 0,
        unmetered: flags & 4 != 0,
        charging: flags & 8 != 0,
    });
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_misob_nova_NovaOfflineJobService_nativePrepare(
    _env: jni::JNIEnv,
    _this: JObject,
) {
    CANCEL.store(false, Ordering::Release);
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_misob_nova_NovaOfflineJobService_nativeCancel(
    _env: jni::JNIEnv,
    _this: JObject,
) {
    CANCEL.store(true, Ordering::Release);
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_misob_nova_NovaOfflineJobService_nativeRun(
    mut env: jni::JNIEnv,
    this: JObject,
) -> jni::sys::jstring {
    crate::diagnostics::init();
    let result = (|| -> Result<String, String> {
        android_bg::initialize_headless_context(&mut env, &this)?;
        sync::migrate_legacy_profiles().map_err(|e| e.to_string())?;
        if !profiles::is_initialized() {
            profiles::initialize()?;
        }
        let coordinator =
            DownloadCoordinator::new_headless(nova_config::app_data_dir().join("downloads"));
        if coordinator.foreground_attached() {
            smart_offline::trigger();
            return Ok("foreground coordinator owns transfers".into());
        }
        for profile in profiles::list()? {
            sync::project_headless_profile(&profiles::ProfileStore::for_profile(&profile.id))
                .map_err(|e| e.to_string())?;
        }
        let owned_torrent_engine = crate::torrent::engine().is_none();
        let started = std::time::Instant::now();
        // Cancel the same shared worker, retaining HTTP partials, when Android
        // revokes the window. The Activity coordinator is reused if present.
        let finished = Arc::new(AtomicBool::new(false));
        let done = finished.clone();
        let monitor = coordinator.clone();
        let watchdog = std::thread::spawn(move || {
            while !done.load(Ordering::Acquire) {
                if CANCEL.load(Ordering::Acquire) || started.elapsed() >= Duration::from_secs(90) {
                    CANCEL.store(true, Ordering::Release);
                    if !monitor.foreground_attached() {
                        monitor.pause_background_smart();
                    }
                    break;
                }
                std::thread::park_timeout(Duration::from_millis(200));
            }
        });
        let result = smart_offline::refresh_headless_inventory(&CANCEL)
            .and_then(|()| smart_offline::reconcile(&coordinator, &CANCEL, None));
        while result.is_ok()
            && !CANCEL.load(Ordering::Acquire)
            && coordinator.has_active_work()
            && started.elapsed() < Duration::from_secs(90)
        {
            coordinator.reap_finished_worker();
            std::thread::park_timeout(Duration::from_millis(200));
        }
        if !coordinator.foreground_attached() {
            coordinator.pause_background_smart();
            if owned_torrent_engine {
                crate::torrent::uninstall();
            }
        }
        finished.store(true, Ordering::Release);
        let _ = watchdog.join();
        result?;
        Ok("checkpointed; pending background opportunity".into())
    })();
    env.new_string(result.unwrap_or_else(|e| format!("deferred: {e}")))
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}
