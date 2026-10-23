//! Platform lifetime for external receiver playback. The session worker owns
//! this service; Activity suspension and closing the picker do not stop it.
use std::collections::VecDeque;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_os = "android"), allow(dead_code))] // Constructed by Android JNI.
pub(super) enum CastServiceCommand {
    Play,
    Pause,
    Stop,
    ShowControls,
    NetworkChanged,
    ServiceStopped,
}

static GENERATION: AtomicU64 = AtomicU64::new(0);
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
static COMMANDS: Mutex<VecDeque<(u64, CastServiceCommand)>> = Mutex::new(VecDeque::new());
static CANCELLATION: Mutex<Option<(u64, Arc<AtomicBool>)>> = Mutex::new(None);

#[cfg(any(target_os = "android", test))]
fn enqueue(generation: u64, command: CastServiceCommand) {
    if generation == 0 || generation != GENERATION.load(Ordering::Acquire) {
        return;
    }
    if matches!(
        command,
        CastServiceCommand::Stop | CastServiceCommand::ServiceStopped
    ) && let Some((id, cancel)) = CANCELLATION
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
    {
        if *id == generation {
            cancel.store(true, Ordering::Release);
        }
    }
    let mut commands = COMMANDS.lock().unwrap_or_else(|poison| poison.into_inner());
    if command == CastServiceCommand::Stop {
        commands.clear();
    } else if commands
        .iter()
        .any(|(id, queued)| *id == generation && *queued == CastServiceCommand::Stop)
    {
        return;
    } else if matches!(
        command,
        CastServiceCommand::Play | CastServiceCommand::Pause
    ) {
        // These are desired states, not toggles. A quick Play/Pause/Play must
        // retain the last intent instead of discarding its duplicate value.
        commands.retain(|(id, queued)| {
            *id != generation
                || !matches!(queued, CastServiceCommand::Play | CastServiceCommand::Pause)
        });
    } else if commands
        .iter()
        .any(|(id, queued)| *id == generation && *queued == command)
    {
        return;
    }
    if commands.len() >= 32 {
        commands.pop_front();
    }
    commands.push_back((generation, command));
}

fn commands_for(generation: u64) -> Vec<CastServiceCommand> {
    let mut commands = COMMANDS.lock().unwrap_or_else(|p| p.into_inner());
    let mut result = Vec::new();
    commands.retain(|(id, command)| {
        if *id == generation {
            result.push(*command);
            false
        } else {
            true
        }
    });
    result
}

#[derive(Clone)]
pub(super) struct CastServiceLease(Arc<ServiceLifetime>);

struct ServiceLifetime {
    generation: u64,
    can_pause: AtomicBool,
}

impl CastServiceLease {
    pub(super) fn set_can_pause(&self, can_pause: bool) {
        let changed = GENERATION.load(Ordering::Acquire) == self.0.generation
            && self.0.can_pause.swap(can_pause, Ordering::AcqRel) != can_pause;
        #[cfg(not(target_os = "android"))]
        let _ = changed;
        #[cfg(target_os = "android")]
        if changed {
            platform::with_class(|env, _, class| {
                env.call_static_method(
                    class,
                    "setCanPause",
                    "(JZ)V",
                    &[
                        jni::objects::JValue::Long(self.0.generation as i64),
                        jni::objects::JValue::Bool(can_pause.into()),
                    ],
                )?;
                Ok(())
            });
        }
    }
    /// A heartbeat as well as a notification refresh. An old worker cannot
    /// publish labels or consume commands belonging to a replacement session.
    pub(super) fn update(&self, device_name: &str, paused: bool) {
        if GENERATION.load(Ordering::Acquire) != self.0.generation {
            return;
        }
        #[cfg(target_os = "android")]
        platform::update(device_name, paused, self.0.generation);
        #[cfg(not(target_os = "android"))]
        let _ = (device_name, paused);
    }

    pub(super) fn take_commands(&self) -> Vec<CastServiceCommand> {
        commands_for(self.0.generation)
    }

    /// Stop must interrupt blocking media preparation before the command loop
    /// can run again. A Stop already queued during promotion also cancels.
    pub(super) fn register_cancellation(&self, cancel: Arc<AtomicBool>) {
        if GENERATION.load(Ordering::Acquire) != self.0.generation {
            return;
        }
        *CANCELLATION.lock().unwrap_or_else(|p| p.into_inner()) =
            Some((self.0.generation, cancel.clone()));
        if COMMANDS
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .any(|(id, command)| *id == self.0.generation && *command == CastServiceCommand::Stop)
        {
            cancel.store(true, Ordering::Release);
        }
    }

    pub(super) fn stop(&self) {
        stop_generation(self.0.generation);
    }
}

impl Drop for ServiceLifetime {
    fn drop(&mut self) {
        stop_generation(self.generation);
    }
}

fn stop_generation(generation: u64) {
    // Revocation precedes Java teardown; onDestroy and old PendingIntents are
    // then harmless. Never clear a newer session's cancellation or commands.
    let _ = GENERATION.compare_exchange(generation, 0, Ordering::AcqRel, Ordering::Acquire);
    let mut cancellation = CANCELLATION.lock().unwrap_or_else(|p| p.into_inner());
    if cancellation
        .as_ref()
        .is_some_and(|(id, _)| *id == generation)
    {
        cancellation.take();
    }
    drop(cancellation);
    let _ = commands_for(generation);
    #[cfg(target_os = "android")]
    platform::stop(generation);
}

/// Acquired only around a bounded user-requested search. Independent IDs make
/// overlapping discovery attempts safe: dropping an old lease cannot release
/// a newer search's multicast lock.
pub(super) struct DiscoveryLease {
    #[cfg(target_os = "android")]
    id: u64,
}

pub(super) fn discovery_lock() -> Result<DiscoveryLease, String> {
    #[cfg(target_os = "android")]
    {
        let id = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed).max(1);
        let acquired = platform::with_class(|env, context, class| {
            env.call_static_method(
                class,
                "acquireDiscovery",
                "(Landroid/content/Context;J)Z",
                &[
                    jni::objects::JValue::Object(context),
                    jni::objects::JValue::Long(id as i64),
                ],
            )?
            .z()
        })
        .unwrap_or(false);
        if !acquired {
            return Err("Local network discovery is unavailable".into());
        }
        Ok(DiscoveryLease { id })
    }
    #[cfg(not(target_os = "android"))]
    Ok(DiscoveryLease {})
}

impl Drop for DiscoveryLease {
    fn drop(&mut self) {
        #[cfg(target_os = "android")]
        platform::with_class(|env, _, class| {
            env.call_static_method(
                class,
                "releaseDiscovery",
                "(J)V",
                &[jni::objects::JValue::Long(self.id as i64)],
            )?;
            Ok(())
        });
    }
}

/// Call off the UI thread. Android confirms actual foreground promotion before
/// media setup begins, so a denied service start cannot strand a remote load.
pub(super) fn start(device_name: &str, paused: bool) -> Result<CastServiceLease, String> {
    let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed).max(1);
    GENERATION
        .compare_exchange(0, generation, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| "A casting session is already active".to_string())?;
    COMMANDS.lock().unwrap_or_else(|p| p.into_inner()).clear();
    #[cfg(target_os = "android")]
    if !platform::start(device_name, paused, generation) {
        GENERATION
            .compare_exchange(generation, 0, Ordering::AcqRel, Ordering::Acquire)
            .ok();
        platform::stop(generation);
        return Err("Background casting could not be started".into());
    }
    #[cfg(not(target_os = "android"))]
    let _ = (device_name, paused);
    Ok(CastServiceLease(Arc::new(ServiceLifetime {
        generation,
        can_pause: AtomicBool::new(false),
    })))
}

#[cfg(target_os = "android")]
mod platform {
    use super::*;
    use jni::objects::{GlobalRef, JClass, JObject, JObjectArray, JValue};
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    static CLASS: OnceLock<GlobalRef> = OnceLock::new();
    static LAST_UPDATE: Mutex<Option<(u64, String, bool, Instant)>> = Mutex::new(None);

    pub(super) fn with_class<R>(
        call: impl FnOnce(&mut jni::JNIEnv, &JObject, JClass) -> jni::errors::Result<R>,
    ) -> Option<R> {
        super::super::android_bg::with_app_context(|env, raw| {
            // An already-attached Activity/native thread may live for hours.
            // Release notification strings and arrays after every heartbeat,
            // rather than relying on thread detachment to free local refs.
            env.with_local_frame(32, |env| {
                let context = unsafe { JObject::from_raw(raw) };
                let result = super::super::android_bg::app_class(
                    env,
                    &context,
                    "dev.misob.nova.NovaCastService",
                    &CLASS,
                )
                .and_then(|class| call(env, &context, class));
                // A platform denial must not leave a pending Java exception
                // that poisons later work on this long-lived native thread.
                if env.exception_check().unwrap_or(false) {
                    let _ = env.exception_clear();
                }
                result
            })
        })
    }

    fn labels<'local>(env: &mut jni::JNIEnv<'local>) -> jni::errors::Result<JObjectArray<'local>> {
        let labels = [
            "Casting",
            "Playing",
            "Paused",
            "Play",
            "Pause",
            "Stop casting",
        ];
        let array =
            env.new_object_array(labels.len() as i32, "java/lang/String", JObject::null())?;
        for (index, label) in labels.into_iter().enumerate() {
            let value = env.new_string(super::super::text::tr(label))?;
            env.set_object_array_element(&array, index as i32, value)?;
        }
        Ok(array)
    }

    pub(super) fn start(device: &str, paused: bool, generation: u64) -> bool {
        with_class(|env, context, class| {
            let device = env.new_string(device)?;
            let labels = labels(env)?;
            env.call_static_method(
                class,
                "start",
                "(Landroid/content/Context;Ljava/lang/String;ZJ[Ljava/lang/String;)Z",
                &[
                    JValue::Object(context),
                    JValue::Object(&device),
                    JValue::Bool(paused.into()),
                    JValue::Long(generation as i64),
                    JValue::Object(&labels),
                ],
            )?
            .z()
        })
        .unwrap_or(false)
    }

    pub(super) fn update(device: &str, paused: bool, generation: u64) {
        if generation == 0 {
            return;
        }
        {
            let mut last = LAST_UPDATE.lock().unwrap_or_else(|p| p.into_inner());
            if last.as_ref().is_some_and(|(id, name, was_paused, at)| {
                *id == generation
                    && name == device
                    && *was_paused == paused
                    && at.elapsed() < Duration::from_secs(2)
            }) {
                return;
            }
            *last = Some((generation, device.into(), paused, Instant::now()));
        }
        with_class(|env, context, class| {
            let device = env.new_string(device)?;
            let labels = labels(env)?;
            env.call_static_method(
                class,
                "update",
                "(Landroid/content/Context;Ljava/lang/String;ZJ[Ljava/lang/String;)V",
                &[
                    JValue::Object(context),
                    JValue::Object(&device),
                    JValue::Bool(paused.into()),
                    JValue::Long(generation as i64),
                    JValue::Object(&labels),
                ],
            )?;
            Ok(())
        });
    }

    pub(super) fn stop(generation: u64) {
        if generation == 0 {
            return;
        }
        with_class(|env, context, class| {
            env.call_static_method(
                class,
                "stop",
                "(Landroid/content/Context;J)V",
                &[JValue::Object(context), JValue::Long(generation as i64)],
            )?;
            Ok(())
        });
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_misob_nova_NovaCastService_nativeCommand(
        _env: jni::JNIEnv,
        _class: JClass,
        generation: jni::sys::jlong,
        command: jni::sys::jint,
    ) {
        let command = match command {
            1 => CastServiceCommand::Play,
            2 => CastServiceCommand::Pause,
            3 => CastServiceCommand::Stop,
            4 => CastServiceCommand::ShowControls,
            5 => CastServiceCommand::NetworkChanged,
            6 => CastServiceCommand::ServiceStopped,
            _ => return,
        };
        enqueue(generation as u64, command);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_notification_actions_cannot_control_a_new_cast() {
        // All queue assertions share one test: these are process-global hooks.
        GENERATION.store(51, Ordering::Release);
        COMMANDS.lock().unwrap().clear();
        enqueue(50, CastServiceCommand::Stop);
        enqueue(0, CastServiceCommand::Play);
        assert!(commands_for(51).is_empty());
        enqueue(51, CastServiceCommand::Pause);
        enqueue(51, CastServiceCommand::Pause);
        enqueue(51, CastServiceCommand::NetworkChanged);
        assert_eq!(
            commands_for(51),
            [
                CastServiceCommand::Pause,
                CastServiceCommand::NetworkChanged
            ]
        );
        enqueue(51, CastServiceCommand::Play);
        enqueue(51, CastServiceCommand::Pause);
        enqueue(51, CastServiceCommand::Play);
        assert_eq!(commands_for(51), [CastServiceCommand::Play]);
        enqueue(51, CastServiceCommand::Play);
        enqueue(51, CastServiceCommand::Stop);
        enqueue(51, CastServiceCommand::Play);
        assert_eq!(commands_for(51), [CastServiceCommand::Stop]);
        GENERATION.store(52, Ordering::Release);
        enqueue(51, CastServiceCommand::ServiceStopped);
        enqueue(52, CastServiceCommand::ShowControls);
        let old = CastServiceLease(Arc::new(ServiceLifetime {
            generation: 51,
            can_pause: AtomicBool::new(false),
        }));
        let current = CastServiceLease(Arc::new(ServiceLifetime {
            generation: 52,
            can_pause: AtomicBool::new(false),
        }));
        old.stop();
        assert_eq!(GENERATION.load(Ordering::Acquire), 52);
        assert!(old.take_commands().is_empty());
        assert_eq!(current.take_commands(), [CastServiceCommand::ShowControls]);
        let cancellation = Arc::new(AtomicBool::new(false));
        current.register_cancellation(cancellation.clone());
        enqueue(51, CastServiceCommand::Stop);
        assert!(!cancellation.load(Ordering::Acquire));
        enqueue(52, CastServiceCommand::Stop);
        assert!(cancellation.load(Ordering::Acquire));
        drop(old);
        assert_eq!(current.take_commands(), [CastServiceCommand::Stop]);
        drop(current);
        assert_eq!(GENERATION.load(Ordering::Acquire), 0);
        assert!(CANCELLATION.lock().unwrap().is_none());
    }
}
