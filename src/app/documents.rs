//! Platform document providers. Callbacks run off the UI thread.
use std::io::{Read, Write};

pub(crate) const MAX_BYTES: usize = 128 * 1024 * 1024;
fn scoped_done<T>(
    done: impl FnOnce(Result<T, String>) + Send + 'static,
) -> impl FnOnce(Result<T, String>) + Send + 'static {
    let context = crate::app::profiles::capture();
    move |result| crate::app::profiles::with_context(context, || done(result))
}
#[cfg(target_os = "android")]
type Completion = Box<dyn FnOnce(Result<Option<Vec<u8>>, String>) + Send>;

#[cfg(not(target_os = "android"))]
pub(crate) fn open(
    extension: &str,
    done: impl FnOnce(Result<Option<Vec<u8>>, String>) + Send + 'static,
) {
    let done = scoped_done(done);
    let extension = extension.to_owned();
    std::thread::spawn(move || {
        let result = (|| {
            let Some(path) = rfd::FileDialog::new()
                .add_filter("Nova", &[&extension])
                .pick_file()
            else {
                return Ok(None);
            };
            let mut bytes = Vec::new();
            std::fs::File::open(path)
                .map_err(|e| e.to_string())?
                .take((MAX_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() > MAX_BYTES {
                return Err("Document exceeds the size limit".into());
            }
            Ok(Some(bytes))
        })();
        done(result);
    });
}

#[cfg(not(target_os = "android"))]
pub(crate) fn save(
    name: &str,
    _mime: &str,
    bytes: Vec<u8>,
    done: impl FnOnce(Result<bool, String>) + Send + 'static,
) {
    let done = scoped_done(done);
    let name = name.to_owned();
    std::thread::spawn(move || {
        let result = (|| {
            let Some(path) = rfd::FileDialog::new().set_file_name(name).save_file() else {
                return Ok(false);
            };
            let temporary = path.with_extension(format!("nova-tmp-{}", nova_config::now_ms()));
            let write = (|| {
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&temporary)
                    .map_err(|e| e.to_string())?;
                file.write_all(&bytes).map_err(|e| e.to_string())?;
                file.sync_all().map_err(|e| e.to_string())?;
                replace_document(&temporary, &path).map_err(|e| e.to_string())?;
                #[cfg(unix)]
                if let Some(parent) = path.parent() {
                    std::fs::File::open(parent)
                        .and_then(|directory| directory.sync_all())
                        .map_err(|e| e.to_string())?;
                }
                Ok(true)
            })();
            if write.is_err() {
                let _ = std::fs::remove_file(&temporary);
            }
            write
        })();
        done(result);
    });
}

#[cfg(all(not(target_os = "android"), not(target_os = "windows")))]
fn replace_document(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> std::io::Result<()> {
    std::fs::rename(source, destination)
}

#[cfg(target_os = "windows")]
fn replace_document(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, replacement: *const u16, flags: u32) -> i32;
    }
    let source = source
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    // Replace in the same directory after fsync. Windows rename alone rejects
    // an existing destination; MoveFileEx leaves the previous file on failure.
    if unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), 0x1 | 0x8) } == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(target_os = "android")]
pub(crate) fn open(
    _extension: &str,
    done: impl FnOnce(Result<Option<Vec<u8>>, String>) + Send + 'static,
) {
    android::launch(
        "",
        "application/octet-stream",
        None,
        Box::new(scoped_done(done)),
    );
}
#[cfg(target_os = "android")]
pub(crate) fn save(
    name: &str,
    mime: &str,
    bytes: Vec<u8>,
    done: impl FnOnce(Result<bool, String>) + Send + 'static,
) {
    android::launch(
        name,
        mime,
        Some(bytes),
        Box::new(scoped_done(
            move |result: Result<Option<Vec<u8>>, String>| {
                done(result.map(|value| value.is_some()))
            },
        )),
    );
}

#[cfg(target_os = "android")]
mod android {
    use super::*;
    use jni::objects::{GlobalRef, JByteArray, JObject, JString, JValue};
    use jni::sys::{jint, jlong};
    use std::sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    };
    static CLASS: OnceLock<GlobalRef> = OnceLock::new();
    static NEXT: AtomicU64 = AtomicU64::new(1);
    static PENDING: Mutex<Option<(u64, Completion)>> = Mutex::new(None);
    pub(super) fn launch(name: &str, mime: &str, bytes: Option<Vec<u8>>, done: Completion) {
        let request = NEXT.fetch_add(1, Ordering::Relaxed);
        let mut pending = PENDING.lock().unwrap();
        if pending.is_some() {
            drop(pending);
            done(Err("A document picker is already open".into()));
            return;
        }
        *pending = Some((request, done));
        drop(pending);
        let launched = crate::app::android_bg::with_app_context(|env, raw| {
            let context = unsafe { JObject::from_raw(raw) };
            let class = crate::app::android_bg::app_class(
                env,
                &context,
                "dev.misob.nova.DocumentActivity",
                &CLASS,
            )?;
            let name = env.new_string(name)?;
            let mime = env.new_string(mime)?;
            let data = bytes
                .as_ref()
                .map(|bytes| env.byte_array_from_slice(bytes))
                .transpose()?;
            let null = JObject::null();
            env.call_static_method(
                &class,
                "start",
                "(Landroid/content/Context;JLjava/lang/String;Ljava/lang/String;[B)V",
                &[
                    JValue::Object(&context),
                    JValue::Long(request as jlong),
                    JValue::Object(&name),
                    JValue::Object(&mime),
                    JValue::Object(data.as_ref().map(|value| value.as_ref()).unwrap_or(&null)),
                ],
            )?;
            Ok(())
        })
        .is_some();
        if !launched {
            complete(request, Err("Document picker could not be opened".into()));
        }
    }
    fn complete(request: u64, result: Result<Option<Vec<u8>>, String>) {
        let callback = {
            let mut slot = PENDING.lock().unwrap();
            if slot.as_ref().is_none_or(|(id, _)| *id != request) {
                return;
            }
            slot.take().map(|(_, done)| done)
        };
        if let Some(done) = callback {
            std::thread::spawn(move || done(result));
        }
    }
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_misob_nova_DocumentActivity_nativeResult(
        env: jni::JNIEnv,
        _class: JObject,
        request: jlong,
        bytes: JByteArray,
        outcome: jint,
        message: JString,
    ) {
        let mut env = env;
        let result = match outcome {
            0 => Ok(None),
            1 => match env.get_array_length(&bytes) {
                Ok(len) if len as usize <= MAX_BYTES => env
                    .convert_byte_array(&bytes)
                    .map(Some)
                    .map_err(|e| e.to_string()),
                _ => Err("Document exceeds the size limit".into()),
            },
            _ => Err(env
                .get_string(&message)
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_else(|_| "Document operation failed".into())),
        };
        complete(request as u64, result);
    }
}
