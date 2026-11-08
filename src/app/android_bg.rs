//! Android background execution glue.
//!
//! Three Java components live under `android/java/dev/misob/nova/` and are driven
//! from here over JNI:
//!
//! * [`NovaBackgroundService`] — a `dataSync` foreground service that keeps the
//!   process (and its transfer threads) alive while a download is active,
//!   including with the screen off. The download work stays in Rust; Java only
//!   holds the wake lock and shows the notification Android requires.
//! * [`NovaAuthService`] — an independent, bounded foreground service protecting
//!   browser OAuth reception and token exchange while Nova is in the background.
//! * [`NovaSyncJobService`] — a `JobScheduler` job that wakes the process roughly
//!   every 15 minutes to run one bounded sync pass (the app projects the merged
//!   records into its local state the next time it opens).
//!
//! All Android API calls live in Java; Rust only loads a class through the
//! app's class loader (reachable from the Application Context retained during
//! foreground or headless bootstrap) and calls a static method.
#![cfg(target_os = "android")]

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use jni::objects::{GlobalRef, JObject, JValue};
use nova_android::context::JniDriver;
use nova_android::service::DownloadService;

// The Application context, class lookup and exception handling are shared with
// Nova Reader through nova-android.
pub(crate) use nova_android::context::{
    app_class, initialize_foreground_context, with_app_context,
};

const SERVICE_CLASS: &str = "dev.misob.nova.NovaBackgroundService";
const AUTH_CLASS: &str = "dev.misob.nova.NovaAuthService";
const JOB_CLASS: &str = "dev.misob.nova.NovaSyncJobService";

/// Start/update/stop state of the download foreground service.
static DOWNLOAD_SERVICE: DownloadService<JniDriver> =
    DownloadService::new(JniDriver::new(SERVICE_CLASS));
static AUTH_CLASS_REF: OnceLock<GlobalRef> = OnceLock::new();
/// Cached global ref to the sync-job class.
static JOB_CLASS_REF: OnceLock<GlobalRef> = OnceLock::new();
static JOB_CANCEL: AtomicBool = AtomicBool::new(false);

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_misob_nova_NovaSyncJobService_nativeCancelSync(
    _env: jni::JNIEnv,
    _this: JObject,
) {
    JOB_CANCEL.store(true, Ordering::Release);
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_misob_nova_NovaSyncJobService_nativePrepareSync(
    _env: jni::JNIEnv,
    _this: JObject,
) {
    JOB_CANCEL.store(false, Ordering::Release);
}

/// Start, update, or stop the download foreground service. `busy` is whether
/// any download is queued or transferring; `title`/`text` label the ongoing
/// notification. Cheap to call every tick: the JNI/notification work is
/// throttled to state changes and one refresh per second.
pub(crate) fn set_download_service(busy: bool, title: &str, text: &str) {
    DOWNLOAD_SERVICE.set(busy, title, text);
}

/// Wait for foreground promotion before opening the browser. The tracker actor
/// invokes this off the UI thread; failed JNI/platform starts fail sign-in.
pub(crate) fn start_auth_service(title: &str, text: &str) -> Option<()> {
    with_app_context(|env, raw| {
        let context = unsafe { JObject::from_raw(raw) };
        let class = app_class(env, &context, AUTH_CLASS, &AUTH_CLASS_REF)?;
        let title = env.new_string(title)?;
        let text = env.new_string(text)?;
        env.call_static_method(
            &class,
            "start",
            "(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;)Z",
            &[
                JValue::Object(&context),
                JValue::Object(&title),
                JValue::Object(&text),
            ],
        )?
        .z()
    })
    .filter(|ready| *ready)
    .map(|_| ())
}

pub(crate) fn stop_auth_service() {
    with_app_context(|env, raw| {
        let context = unsafe { JObject::from_raw(raw) };
        let class = app_class(env, &context, AUTH_CLASS, &AUTH_CLASS_REF)?;
        env.call_static_method(
            &class,
            "stop",
            "(Landroid/content/Context;)V",
            &[JValue::Object(&context)],
        )?;
        Ok(())
    });
}

/// Schedule (enabled) or cancel the periodic sync job. The job itself needs no
/// Context from the UI process: `JobScheduler` rebuilds it across process death.
pub(crate) fn set_periodic_sync(enabled: bool) {
    with_app_context(|env, raw| {
        let context = unsafe { JObject::from_raw(raw) };
        let class = app_class(env, &context, JOB_CLASS, &JOB_CLASS_REF)?;
        let method = if enabled { "schedule" } else { "cancel" };
        env.call_static_method(
            &class,
            method,
            "(Landroid/content/Context;)V",
            &[JValue::Object(&context)],
        )?;
        Ok(())
    });
}

/// JNI entry for `NovaSyncJobService.nativeRunSync()`. Runs on the job's worker
/// thread with the JobService instance (which is a `Context`) so a process
/// started only for the job can initialize its files dir and storage. Returns a
/// short summary so the Java side can put it in logcat.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_misob_nova_NovaSyncJobService_nativeRunSync(
    mut env: jni::JNIEnv,
    this: JObject,
) -> jni::sys::jstring {
    crate::diagnostics::init();
    let summary = match run_headless_sync(&mut env, &this) {
        Ok(summary) => summary,
        Err(error) => format!("failed: {error}"),
    };
    match env.new_string(summary) {
        Ok(s) => s.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

fn run_headless_sync(env: &mut jni::JNIEnv, context: &JObject) -> Result<String, String> {
    let started = Instant::now();
    // A retained Activity engine can be frozen along with its process. The
    // job must borrow it for protected work instead of treating its presence
    // as evidence that an ordinary interval pass is executing.
    // Initialize app paths + redb *before* reading settings. A job-only process
    // has no Activity (and thus no `ndk-context` Context), so nothing has opened
    // the store yet — and `read_settings` reads redb. The open is lazy and
    // caches a permanent `None` if no path is set, so reading first would make
    // sync look permanently disabled for the life of the process.
    initialize_headless_context(env, context)?;

    let settings = nova_sync::read_settings();
    if !settings.enabled || !settings.background_enabled || JOB_CANCEL.load(Ordering::Acquire) {
        return Ok("skipped: sync disabled".to_string());
    }

    // An upgraded job-only process can sync before the first Activity launch.
    // Move legacy records before advertising the profile-aware protocol.
    super::sync::migrate_legacy_profiles().map_err(|error| error.to_string())?;
    match nova_sync::background_engine() {
        Ok(Some(lease)) => Ok(format!(
            "{:?}",
            lease.engine.one_shot(
                &JOB_CANCEL,
                Duration::from_secs(120).saturating_sub(started.elapsed())
            )
        )),
        Ok(None) => Ok("skipped: another background job owns engine".to_string()),
        Err(error) => Err(format!("setup: {error:#}")),
    }
}

/// Bootstrap shared by no-Activity sync/reminder/offline jobs. The app context
/// is retained for JNI adapters; no Activity/Slint/Bridge is constructed.
pub(crate) fn initialize_headless_context(
    env: &mut jni::JNIEnv,
    context: &JObject,
) -> Result<(), String> {
    let path = nova_android::context::files_dir(env, context)?;
    nova_config::set_android_files_dir(path);
    crate::storage::init_at(&nova_config::app_data_dir());
    super::profiles::install_record_policy();
    super::addon_packages::configure_blob_store();

    nova_android::context::retain_app_context(env, context).map_err(|error| error.to_string())?;
    Ok(())
}
