//! Android background execution glue.
//!
//! Two Java components live under `android/java/dev/misob/nova/` and are driven
//! from here over JNI:
//!
//! * [`NovaBackgroundService`] — a `dataSync` foreground service that keeps the
//!   process (and its transfer threads) alive while a download is active,
//!   including with the screen off. The download work stays in Rust; Java only
//!   holds the wake lock and shows the notification Android requires.
//! * [`NovaSyncJobService`] — a `JobScheduler` job that wakes the process roughly
//!   every 15 minutes to run one bounded sync pass (the app projects the merged
//!   records into its local state the next time it opens).
//!
//! All Android API calls live in Java; Rust only loads a class through the
//! app's class loader (reachable from the `Context` published by
//! `android-activity` via `ndk-context`) and calls a static method.
#![cfg(target_os = "android")]

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use jni::JavaVM;
use jni::objects::{GlobalRef, JClass, JObject, JString, JValue};
use jni::sys::jobject;

const SERVICE_CLASS: &str = "dev.misob.nova.NovaBackgroundService";
const JOB_CLASS: &str = "dev.misob.nova.NovaSyncJobService";

/// How often the notification text is refreshed while a download runs. The
/// tick fires every 250 ms; re-issuing JNI twice a second is plenty.
const NOTIFICATION_REFRESH: Duration = Duration::from_secs(1);

/// Whether the download foreground service is currently requested.
static DOWNLOAD_SERVICE_ACTIVE: AtomicBool = AtomicBool::new(false);
/// Last notification text and when it was pushed, for throttling.
static LAST_NOTIFICATION: Mutex<Option<(String, Instant)>> = Mutex::new(None);
/// Cached global ref to the service class (loaded once through the app class
/// loader; avoids a loader round-trip on every notification refresh).
static SERVICE_CLASS_REF: OnceLock<GlobalRef> = OnceLock::new();
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

/// Run `f` on the current thread with the app `Context` that `android-activity`
/// published through `ndk-context`. Returns `None` when no Context is available
/// (e.g. a job-only process that never started an Activity). The raw context
/// pointer is passed so the closure can create a `JObject` whose lifetime lines
/// up with its `JNIEnv`.
///
/// Shared with the camera scanner (`android_qr.rs`), which loads its activity
/// the same way.
pub(crate) fn with_app_context<R>(
    f: impl FnOnce(&mut jni::JNIEnv, jobject) -> jni::errors::Result<R>,
) -> Option<R> {
    let ctx = ndk_context::android_context();
    // SAFETY: ndk-context holds these for the process lifetime; the wrappers do
    // not take ownership and Drop does not release them.
    let vm = unsafe { JavaVM::from_raw(ctx.vm() as *mut jni::sys::JavaVM) }.ok()?;
    let mut guard = vm.attach_current_thread().ok()?;
    f(&mut guard, ctx.context() as jobject).ok()
}

/// Resolve an app class through the Context's class loader (the reliable way to
/// find app-dex classes from a native thread), caching the global ref.
/// Shared with the camera scanner (`android_qr.rs`).
pub(crate) fn app_class<'local>(
    env: &mut jni::JNIEnv<'local>,
    context: &JObject<'local>,
    dotted: &str,
    cache: &'static OnceLock<GlobalRef>,
) -> jni::errors::Result<JClass<'local>> {
    if let Some(global) = cache.get() {
        // Alias the cached global ref; `JClass`'s Drop is a no-op for local
        // views, so the cache keeps ownership.
        return Ok(unsafe { JClass::from_raw(global.as_obj().as_raw()) });
    }
    let loader = env
        .call_method(context, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])?
        .l()?;
    let name = env.new_string(dotted)?;
    let class = env
        .call_method(
            &loader,
            "loadClass",
            "(Ljava/lang/String;)Ljava/lang/Class;",
            &[JValue::Object(&name)],
        )?
        .l()?;
    let global = env.new_global_ref(&class)?;
    let _ = cache.set(global);
    Ok(class.into())
}

/// Start, update, or stop the download foreground service. `busy` is whether
/// any download is queued or transferring; `title`/`text` label the ongoing
/// notification. Cheap to call every tick: the JNI/notification work is
/// throttled to state changes and one refresh per second.
pub(crate) fn set_download_service(busy: bool, title: &str, text: &str) {
    let was_active = DOWNLOAD_SERVICE_ACTIVE.swap(busy, Ordering::AcqRel);
    let changed = was_active != busy;
    if !busy {
        if !changed {
            return;
        }
        with_app_context(|env, raw| {
            let context = unsafe { JObject::from_raw(raw) };
            let class = app_class(env, &context, SERVICE_CLASS, &SERVICE_CLASS_REF)?;
            env.call_static_method(
                &class,
                "stop",
                "(Landroid/content/Context;)V",
                &[JValue::Object(&context)],
            )?;
            Ok(())
        });
        return;
    }

    // Active: throttle text updates, but always act on the rising edge.
    let body = format!("{title}\n{text}");
    {
        let mut last = LAST_NOTIFICATION.lock().unwrap();
        if !changed
            && last.as_ref().is_some_and(|(previous, at)| {
                previous == &body && at.elapsed() < NOTIFICATION_REFRESH
            })
        {
            return;
        }
        *last = Some((body.clone(), Instant::now()));
    }
    with_app_context(|env, raw| {
        let context = unsafe { JObject::from_raw(raw) };
        let class = app_class(env, &context, SERVICE_CLASS, &SERVICE_CLASS_REF)?;
        if !was_active {
            env.call_static_method(
                &class,
                "start",
                "(Landroid/content/Context;)V",
                &[JValue::Object(&context)],
            )?;
        }
        let title = env.new_string(title)?;
        let text = env.new_string(text)?;
        env.call_static_method(
            &class,
            "update",
            "(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;)V",
            &[
                JValue::Object(&context),
                JValue::Object(&title),
                JValue::Object(&text),
            ],
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
    // The app is alive: its own interval loop owns sync, so the job is a no-op.
    if nova_sync::is_running() {
        return Ok("skipped: app sync already running".to_string());
    }

    // Initialize app paths + redb *before* reading settings. A job-only process
    // has no Activity (and thus no `ndk-context` Context), so nothing has opened
    // the store yet — and `read_settings` reads redb. The open is lazy and
    // caches a permanent `None` if no path is set, so reading first would make
    // sync look permanently disabled for the life of the process.
    let files = env
        .call_method(context, "getFilesDir", "()Ljava/io/File;", &[])
        .map_err(|e| e.to_string())?
        .l()
        .map_err(|e| e.to_string())?;
    let path = env
        .call_method(&files, "getAbsolutePath", "()Ljava/lang/String;", &[])
        .map_err(|e| e.to_string())?
        .l()
        .map_err(|e| e.to_string())?;
    let jpath = unsafe { JString::from_raw(path.into_raw()) };
    let path: String = env.get_string(&jpath).map_err(|e| e.to_string())?.into();
    nova_config::set_android_files_dir(PathBuf::from(path));
    crate::storage::init_at(&nova_config::app_data_dir());

    let settings = nova_sync::read_settings();
    if !settings.enabled || !settings.background_enabled || JOB_CANCEL.load(Ordering::Acquire) {
        return Ok("skipped: sync disabled".to_string());
    }

    match nova_sync::background_engine() {
        Ok(Some(lease)) => Ok(format!(
            "{:?}",
            lease.engine.one_shot(
                &JOB_CANCEL,
                Duration::from_secs(120).saturating_sub(started.elapsed())
            )
        )),
        Ok(None) => Ok("skipped: foreground sync owns engine".to_string()),
        Err(error) => Err(format!("setup: {error:#}")),
    }
}
