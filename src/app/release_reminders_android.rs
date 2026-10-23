//! JobScheduler/notification adapter; all data access uses explicit profile owners.
use super::*;
use jni::objects::{GlobalRef, JClass, JObject, JString, JValue};
use std::sync::OnceLock;
static CLASS: OnceLock<GlobalRef> = OnceLock::new();
static CANCEL: AtomicBool = AtomicBool::new(false);
static JOB_GENERATION: AtomicU64 = AtomicU64::new(0);
const NAME: &str = "dev.misob.nova.NovaReleaseJobService";
pub(super) fn cancel_pending_tap() {
    release_reminders::cancel_pending_notification_tap();
}
fn call(method: &str, signature: &str) -> Option<bool> {
    android_bg::with_app_context(|env, raw| {
        let context = unsafe { JObject::from_raw(raw) };
        let class = android_bg::app_class(env, &context, NAME, &CLASS)?;
        let result =
            env.call_static_method(class, method, signature, &[JValue::Object(&context)])?;
        if signature.ends_with('Z') {
            result.z()
        } else {
            Ok(true)
        }
    })
}
pub(super) fn configure(enabled: bool) {
    let _ = android_bg::with_app_context(|env, raw| {
        let context = unsafe { JObject::from_raw(raw) };
        let class = android_bg::app_class(env, &context, NAME, &CLASS)?;
        env.call_static_method(
            class,
            "configure",
            "(Landroid/content/Context;Z)V",
            &[JValue::Object(&context), JValue::Bool(enabled.into())],
        )?;
        Ok(())
    });
}
pub(super) fn enable_notifications() -> bool {
    call("enable", "(Landroid/content/Context;)Z").unwrap_or(false)
}
pub(super) fn delivery_available() -> bool {
    call("allowed", "(Landroid/content/Context;)Z").unwrap_or(false)
}
pub(super) fn notify(profile: &str, title: &str, body: &str, alert: bool, event: &str, keys: &str) {
    let _ = android_bg::with_app_context(|env, raw| {
        let context = unsafe { JObject::from_raw(raw) };
        let class = android_bg::app_class(env, &context, NAME, &CLASS)?;
        let profile = env.new_string(profile)?;
        let title = env.new_string(title)?;
        let body = env.new_string(body)?;
        let event = env.new_string(event)?;
        let keys = env.new_string(keys)?;
        env.call_static_method(class,"notify","(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;ZLjava/lang/String;Ljava/lang/String;)V",&[JValue::Object(&context),JValue::Object(&profile),JValue::Object(&title),JValue::Object(&body),JValue::Bool(alert.into()),JValue::Object(&event),JValue::Object(&keys)])?;
        Ok(())
    });
}
pub(super) fn poll_tap(bridge: &Bridge) {
    let tap = android_bg::with_app_context(|env, raw| {
        let context = unsafe { JObject::from_raw(raw) };
        let class = android_bg::app_class(env, &context, NAME, &CLASS)?;
        let value = env
            .call_static_method(class, "consumeTap", "()Ljava/lang/String;", &[])?
            .l()?;
        if value.is_null() {
            return Ok(None);
        }
        let value = JString::from(value);
        let text: String = env.get_string(&value)?.into();
        Ok(Some(text))
    })
    .flatten();
    if let Some((profile, event)) =
        tap.and_then(|s| serde_json::from_str::<(String, String)>(&s).ok())
    {
        release_reminders::queue_notification_tap(profile, event);
    }
    bridge.poll_release_notification_tap();
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_misob_nova_NovaReleaseJobService_nativePrepare(
    _env: jni::JNIEnv,
    _this: JObject,
) {
    JOB_GENERATION.fetch_add(1, Ordering::AcqRel);
    CANCEL.store(false, Ordering::Release);
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_misob_nova_NovaReleaseJobService_nativeCancel(
    _env: jni::JNIEnv,
    _this: JObject,
) {
    CANCEL.store(true, Ordering::Release);
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_misob_nova_NovaReleaseJobService_nativeRun(
    mut env: jni::JNIEnv,
    context: JObject,
) {
    let generation = JOB_GENERATION.load(Ordering::Acquire);
    let (_finished, deadline) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        if matches!(
            deadline.recv_timeout(Duration::from_secs(90)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ) && JOB_GENERATION.load(Ordering::Acquire) == generation
        {
            CANCEL.store(true, Ordering::Release);
        }
    });
    let result = (|| -> Result<(), String> {
        android_bg::initialize_headless_context(&mut env, &context)?;
        if !profiles::is_initialized() {
            profiles::initialize()?;
        }
        let started = std::time::Instant::now();
        for profile in profiles::list()? {
            if CANCEL.load(Ordering::Acquire) || started.elapsed() > Duration::from_secs(90) {
                break;
            }
            let owner = profiles::ProfileStore::for_profile(&profile.id);
            sync::project_headless_profile(&owner).map_err(|e| e.to_string())?;
            release_reminders::refresh_owner_metadata(&owner, &CANCEL, started);
            if !CANCEL.load(Ordering::Acquire) {
                release_reminders::reconcile_owner(&owner)?;
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        eprintln!("nova release job: {error}");
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_misob_nova_NovaReleaseJobService_nativeAcknowledge(
    mut env: jni::JNIEnv,
    _class: JClass,
    context: JObject,
    profile: JString,
    keys: JString,
) {
    let result = (|| -> Result<(), String> {
        android_bg::initialize_headless_context(&mut env, &context)?;
        if !profiles::is_initialized() {
            profiles::initialize()?;
        }
        let profile: String = env.get_string(&profile).map_err(|e| e.to_string())?.into();
        if !profiles::valid_id(&profile) {
            return Err("invalid profile".into());
        }
        let keys: String = env.get_string(&keys).map_err(|e| e.to_string())?.into();
        let keys: Vec<String> = serde_json::from_str(&keys).map_err(|e| e.to_string())?;
        if keys.len() > 1000
            || keys
                .iter()
                .any(|key| serde_json::from_str::<(String, String, String)>(key).is_err())
        {
            return Err("invalid notification identities".into());
        }
        release_reminders::acknowledge_dismissal(
            &profiles::ProfileStore::for_profile(&profile),
            &keys,
        )
    })();
    if let Err(error) = result {
        eprintln!("nova release acknowledgment: {error}");
    }
}
