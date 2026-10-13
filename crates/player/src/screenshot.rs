//! Frame screenshots use mpv's subtitle composition, never the shared Slint framebuffer.
//! Direct Android MediaCodec frames use a private GPU target on the render thread.
#[cfg(target_os = "android")]
use slint::ComponentHandle;
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::Player;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScreenshotSaved {
    /// A filesystem path on desktop/older Android, or Pictures/Nova/name on Gallery Android.
    pub display_path: String,
}

/// Deliberately excludes raw mpv/JNI errors: they can contain authenticated URLs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenshotError {
    Unavailable,
    StaleSession,
    CaptureFailed,
    TimedOut,
    SaveFailed,
}
impl std::fmt::Display for ScreenshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unavailable => "No video frame is available",
            Self::StaleSession => "Playback changed before the screenshot was saved",
            Self::CaptureFailed => "The video frame could not be captured",
            Self::TimedOut => "The screenshot capture timed out",
            Self::SaveFailed => "The screenshot could not be saved",
        })
    }
}
impl std::error::Error for ScreenshotError {}

fn check_session(expected: u64, current: u64, started: bool) -> Result<(), ScreenshotError> {
    if expected != current {
        Err(ScreenshotError::StaleSession)
    } else if !started {
        Err(ScreenshotError::Unavailable)
    } else {
        Ok(())
    }
}

static SEQUENCE: AtomicU64 = AtomicU64::new(0);
fn filename() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "nova-{nanos}-{}-{}.png",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

struct Temporary(PathBuf);
impl Temporary {
    fn new(base: &Path) -> Result<Self, ScreenshotError> {
        for _ in 0..32 {
            let directory = base.join(filename());
            let builder = {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::DirBuilderExt;
                    let mut builder = fs::DirBuilder::new();
                    builder.mode(0o700);
                    builder
                }
                #[cfg(not(unix))]
                {
                    fs::DirBuilder::new()
                }
            };
            match builder.create(&directory) {
                Ok(()) => return Ok(Self(directory)),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(ScreenshotError::SaveFailed),
            }
        }
        Err(ScreenshotError::SaveFailed)
    }
}
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct CaptureClient(*mut libmpv2_sys::mpv_handle);
impl Drop for CaptureClient {
    fn drop(&mut self) {
        // This client belongs only to the screenshot worker. The main/render client stays alive.
        unsafe { libmpv2_sys::mpv_destroy(self.0) };
    }
}
impl CaptureClient {
    fn enqueue(mpv: &libmpv2::Mpv, image: &Path) -> Result<Self, ScreenshotError> {
        let image_arg = CString::new(image.to_str().ok_or(ScreenshotError::SaveFailed)?)
            .map_err(|_| ScreenshotError::SaveFailed)?;
        let handle = unsafe {
            libmpv2_sys::mpv_create_client(mpv.ctx.as_ptr(), c"nova-screenshot".as_ptr())
        };
        if handle.is_null() {
            return Err(ScreenshotError::Unavailable);
        }
        let client = Self(handle);
        let mut args = [
            c"screenshot-to-file".as_ptr(),
            image_arg.as_ptr(),
            c"subtitles".as_ptr(),
            std::ptr::null(),
        ];
        if unsafe { libmpv2_sys::mpv_command_async(client.0, 1, args.as_mut_ptr()) } < 0 {
            return Err(ScreenshotError::CaptureFailed);
        }
        Ok(client)
    }
    fn wait(&self, cancelled: impl Fn() -> bool) -> Result<(), ScreenshotError> {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if cancelled() {
                unsafe { libmpv2_sys::mpv_abort_async_command(self.0, 1) };
                return Err(ScreenshotError::StaleSession);
            }
            // Event data is borrowed until the next wait; copy only its scalar status here.
            let event = unsafe { &*libmpv2_sys::mpv_wait_event(self.0, 0.1) };
            if event.event_id == libmpv2_sys::mpv_event_id_MPV_EVENT_COMMAND_REPLY
                && event.reply_userdata == 1
            {
                return if event.error < 0 {
                    Err(ScreenshotError::CaptureFailed)
                } else {
                    Ok(())
                };
            }
            if event.event_id == libmpv2_sys::mpv_event_id_MPV_EVENT_SHUTDOWN {
                return Err(ScreenshotError::Unavailable);
            }
            if Instant::now() >= deadline {
                unsafe { libmpv2_sys::mpv_abort_async_command(self.0, 1) };
                return Err(ScreenshotError::TimedOut);
            }
        }
    }
}

enum CaptureJob {
    Mpv(CaptureClient),
    #[cfg(target_os = "android")]
    Gpu(crate::gpu_screenshot::Pending),
}

impl Player {
    /// Capture on a worker with the token read on the UI thread when the button was pressed.
    pub fn screenshot_session(&self) -> u64 {
        self.state.screenshot_session.load(Ordering::SeqCst)
    }

    pub fn capture_screenshot(
        &self,
        expected_session: u64,
    ) -> Result<ScreenshotSaved, ScreenshotError> {
        if self.screenshot_session() != expected_session {
            return Err(ScreenshotError::StaleSession);
        }
        let temporary = Temporary::new(&temporary_base()?)?;
        let image = temporary.0.join("frame.png");
        let job = {
            // Rendering consumes pending before taking mpv; retain that lock order.
            let pending = self
                .state
                .pending
                .lock()
                .map_err(|_| ScreenshotError::Unavailable)?;
            let mpv = self
                .state
                .mpv
                .lock()
                .map_err(|_| ScreenshotError::Unavailable)?;
            check_session(
                expected_session,
                self.screenshot_session(),
                self.state.started.load(Ordering::SeqCst),
            )?;
            let mpv = mpv.as_ref().ok_or(ScreenshotError::Unavailable)?;
            if pending.is_some()
                || mpv.get_property::<bool>("idle-active").unwrap_or(true)
                || mpv.get_property::<bool>("eof-reached").unwrap_or(true)
            {
                return Err(ScreenshotError::Unavailable);
            }
            // A separate event queue avoids stealing playback events. Dispatch is serialized
            // with replacement/close, but image encoding and disk IO never hold the mpv lock.
            #[cfg(target_os = "android")]
            if mpv
                .get_property::<String>("hwdec-current")
                .unwrap_or_default()
                == crate::HWDEC_PREFERRED
            {
                let (width, height) = crate::gpu_screenshot::dimensions(
                    mpv.get_property("video-out-params/dw").unwrap_or(0),
                    mpv.get_property("video-out-params/dh").unwrap_or(0),
                    mpv.get_property("video-out-params/rotate").unwrap_or(0),
                )?;
                let (request, capture) =
                    crate::gpu_screenshot::Request::new(expected_session, width, height);
                let mut queued = self
                    .state
                    .gpu_screenshot
                    .lock()
                    .map_err(|_| ScreenshotError::Unavailable)?;
                if queued.as_ref().is_some_and(|request| !request.cancelled()) {
                    return Err(ScreenshotError::Unavailable);
                }
                *queued = Some(request);
                CaptureJob::Gpu(capture)
            } else {
                CaptureJob::Mpv(CaptureClient::enqueue(mpv, &image)?)
            }
            #[cfg(not(target_os = "android"))]
            CaptureJob::Mpv(CaptureClient::enqueue(mpv, &image)?)
        };
        match job {
            CaptureJob::Mpv(client) => {
                let result = client.wait(|| self.screenshot_session() != expected_session);
                // Finish cancellation before a staging directory can be removed.
                drop(client);
                result?;
            }
            #[cfg(target_os = "android")]
            CaptureJob::Gpu(capture) => {
                // Paused playback has no new video update to wake the renderer.
                let app = self.state.app.clone();
                slint::invoke_from_event_loop(move || {
                    if let Some(app) = app.upgrade() {
                        app.window().request_redraw();
                    }
                })
                .map_err(|_| ScreenshotError::Unavailable)?;
                let frame = capture.wait(|| self.screenshot_session() != expected_session)?;
                if self.screenshot_session() != expected_session {
                    return Err(ScreenshotError::StaleSession);
                }
                frame.write_png(&image)?;
            }
        }
        if self.screenshot_session() != expected_session {
            return Err(ScreenshotError::StaleSession);
        }
        verify_image(&image)?;
        publish(&image)
    }
}

fn verify_image(path: &Path) -> Result<(), ScreenshotError> {
    let meta = fs::metadata(path).map_err(|_| ScreenshotError::SaveFailed)?;
    if meta.is_file() && meta.len() > 0 {
        Ok(())
    } else {
        Err(ScreenshotError::SaveFailed)
    }
}

fn publish_file(image: &Path, directory: &Path) -> Result<ScreenshotSaved, ScreenshotError> {
    fs::create_dir_all(directory).map_err(|_| ScreenshotError::SaveFailed)?;
    for _ in 0..32 {
        let destination = directory.join(filename());
        let mut output = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
        {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(ScreenshotError::SaveFailed),
        };
        let result = (|| {
            let mut input = File::open(image).map_err(|_| ScreenshotError::SaveFailed)?;
            std::io::copy(&mut input, &mut output).map_err(|_| ScreenshotError::SaveFailed)?;
            output.sync_all().map_err(|_| ScreenshotError::SaveFailed)?;
            verify_image(&destination)
        })();
        drop(output);
        if let Err(err) = result {
            let _ = fs::remove_file(&destination);
            return Err(err);
        }
        return Ok(ScreenshotSaved {
            display_path: destination.display().to_string(),
        });
    }
    Err(ScreenshotError::SaveFailed)
}

#[cfg(not(target_os = "android"))]
fn temporary_base() -> Result<PathBuf, ScreenshotError> {
    Ok(std::env::temp_dir())
}
#[cfg(not(target_os = "android"))]
fn publish(image: &Path) -> Result<ScreenshotSaved, ScreenshotError> {
    publish_file(image, &pictures_directory()?.join("Nova"))
}

#[cfg(not(any(target_os = "android", target_os = "windows")))]
fn pictures_directory() -> Result<PathBuf, ScreenshotError> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or(ScreenshotError::SaveFailed)?;
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".config"));
    if let Ok(contents) = fs::read_to_string(config.join("user-dirs.dirs"))
        && let Some(path) = xdg_pictures(&contents, &home)
    {
        return Ok(path);
    }
    Ok(home.join("Pictures"))
}
#[cfg(not(any(target_os = "android", target_os = "windows")))]
fn xdg_pictures(contents: &str, home: &Path) -> Option<PathBuf> {
    let value = contents
        .lines()
        .find_map(|line| line.trim().strip_prefix("XDG_PICTURES_DIR="))?;
    let value = value.trim().strip_prefix('"')?.strip_suffix('"')?;
    let path = if value == "$HOME" {
        home.to_owned()
    } else if let Some(rest) = value.strip_prefix("$HOME/") {
        home.join(rest)
    } else {
        PathBuf::from(value)
    };
    // user-dirs is data, never evaluate shell substitutions or arbitrary variables.
    (path.is_absolute()
        && !value.contains('`')
        && !value.contains("$(")
        && !value.trim_start_matches("$HOME").contains('$'))
    .then_some(path)
}

#[cfg(target_os = "windows")]
fn pictures_directory() -> Result<PathBuf, ScreenshotError> {
    use std::os::windows::ffi::OsStringExt;
    #[repr(C)]
    struct Guid {
        a: u32,
        b: u16,
        c: u16,
        d: [u8; 8],
    }
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn SHGetKnownFolderPath(
            id: *const Guid,
            flags: u32,
            token: *mut std::ffi::c_void,
            path: *mut *mut u16,
        ) -> i32;
    }
    #[link(name = "ole32")]
    unsafe extern "system" {
        fn CoTaskMemFree(memory: *mut std::ffi::c_void);
    }
    let id = Guid {
        a: 0x33e28130,
        b: 0x4e1e,
        c: 0x4676,
        d: [0x83, 0x5a, 0x98, 0x39, 0x5c, 0x3b, 0xc3, 0xbb],
    };
    let mut raw = std::ptr::null_mut();
    if unsafe { SHGetKnownFolderPath(&id, 0, std::ptr::null_mut(), &mut raw) } >= 0
        && !raw.is_null()
    {
        let mut len = 0;
        unsafe {
            while *raw.add(len) != 0 {
                len += 1;
            }
        }
        let path = PathBuf::from(std::ffi::OsString::from_wide(unsafe {
            std::slice::from_raw_parts(raw, len)
        }));
        unsafe {
            CoTaskMemFree(raw.cast());
        }
        return Ok(path);
    }
    std::env::var_os("USERPROFILE")
        .map(|home| PathBuf::from(home).join("Pictures"))
        .ok_or(ScreenshotError::SaveFailed)
}

#[cfg(target_os = "android")]
fn temporary_base() -> Result<PathBuf, ScreenshotError> {
    android::cache_directory()
}
#[cfg(target_os = "android")]
fn publish(image: &Path) -> Result<ScreenshotSaved, ScreenshotError> {
    android::publish(image)
}

#[cfg(target_os = "android")]
mod android {
    use super::*;
    use jni::objects::{JObjectArray, JString};
    use jni::refs::Global;
    use jni::{JavaVM, bind_java_type};
    use std::os::fd::FromRawFd;

    impl From<jni::errors::Error> for ScreenshotError {
        fn from(_: jni::errors::Error) -> Self {
            Self::SaveFailed
        }
    }

    fn vm() -> Result<JavaVM, ScreenshotError> {
        let pointer = crate::external::java_vm_ptr();
        if pointer.is_null() {
            return Err(ScreenshotError::SaveFailed);
        }
        Ok(JavaVM::singleton().unwrap_or_else(|_| unsafe { JavaVM::from_raw(pointer.cast()) }))
    }

    pub(super) fn cache_directory() -> Result<PathBuf, ScreenshotError> {
        vm()?.attach_current_thread(|env| {
            let raw: jni::sys::jobject = crate::external::android_activity_ptr().cast();
            if raw.is_null() {
                return Err(ScreenshotError::SaveFailed);
            }
            // The host owns the global activity reference throughout this attachment.
            let activity = unsafe { env.as_cast_raw::<Global<ScreenshotActivity>>(&raw)? };
            let directory = activity.cache_dir(env)?;
            if directory.is_null() {
                return Err(ScreenshotError::SaveFailed);
            }
            Ok(PathBuf::from(directory.path(env)?.to_string()))
        })
    }

    pub(super) fn publish(image: &Path) -> Result<ScreenshotSaved, ScreenshotError> {
        vm()?.attach_current_thread(|env| {
            let raw: jni::sys::jobject = crate::external::android_activity_ptr().cast();
            if raw.is_null() {
                return Err(ScreenshotError::SaveFailed);
            }
            // The host owns the global activity reference throughout this attachment.
            let activity = unsafe { env.as_cast_raw::<Global<ScreenshotActivity>>(&raw)? };
            if ScreenshotVersion::sdk(env)? < 29 {
                // API 26–28 cannot write the public album without storage permission.
                // The external app-scoped Pictures directory needs no new permission.
                let pictures = JString::from_str(env, "Pictures")?;
                let directory = activity.external_files_dir(env, &pictures)?;
                if directory.is_null() {
                    return Err(ScreenshotError::SaveFailed);
                }
                let path = PathBuf::from(directory.path(env)?.to_string()).join("Nova");
                return publish_file(image, &path);
            }
            let resolver = activity.resolver(env)?;
            let values = ScreenshotValues::new(env)?;
            let name = filename();
            for (key, value) in [
                ("_display_name", name.as_str()),
                ("mime_type", "image/png"),
                ("relative_path", "Pictures/Nova/"),
                ("is_pending", "1"),
            ] {
                let key = JString::from_str(env, key)?;
                let value = JString::from_str(env, value)?;
                values.put(env, &key, &value)?;
            }
            let collection_name = JString::from_str(env, "content://media/external/images/media")?;
            let collection = ScreenshotUri::parse(env, &collection_name)?;
            let uri = resolver.insert(env, &collection, &values)?;
            if uri.is_null() {
                return Err(ScreenshotError::SaveFailed);
            }
            let result = (|| -> Result<(), ScreenshotError> {
                let mode = JString::from_str(env, "w")?;
                let descriptor = resolver.open_fd(env, &uri, &mode)?;
                if descriptor.is_null() {
                    return Err(ScreenshotError::SaveFailed);
                }
                let fd = descriptor.detach_fd(env)?;
                if fd < 0 {
                    return Err(ScreenshotError::SaveFailed);
                }
                // detachFd transfers ownership, so File closes it even if copying fails.
                let mut output = unsafe { File::from_raw_fd(fd) };
                let mut input = File::open(image).map_err(|_| ScreenshotError::SaveFailed)?;
                let bytes = std::io::copy(&mut input, &mut output)
                    .map_err(|_| ScreenshotError::SaveFailed)?;
                output.sync_all().map_err(|_| ScreenshotError::SaveFailed)?;
                if bytes == 0
                    || output
                        .metadata()
                        .map_err(|_| ScreenshotError::SaveFailed)?
                        .len()
                        != bytes
                {
                    return Err(ScreenshotError::SaveFailed);
                }
                drop(output);
                let complete = ScreenshotValues::new(env)?;
                let pending_key = JString::from_str(env, "is_pending")?;
                let pending_value = JString::from_str(env, "0")?;
                complete.put(env, &pending_key, &pending_value)?;
                if resolver.update(
                    env,
                    &uri,
                    &complete,
                    &JString::null(),
                    &JObjectArray::<JString>::null(),
                )? != 1
                {
                    return Err(ScreenshotError::SaveFailed);
                }
                Ok(())
            })();
            if let Err(err) = result {
                // Clear a failed Java operation before trying to remove its pending row.
                let _ = env.exception_clear();
                let _ = resolver.delete(
                    env,
                    &uri,
                    &JString::null(),
                    &JObjectArray::<JString>::null(),
                );
                return Err(err);
            }
            Ok(ScreenshotSaved {
                display_path: format!("Pictures/Nova/{name}"),
            })
        })
    }

    bind_java_type! {
        ScreenshotActivity => "android.app.Activity",
        type_map = { ScreenshotFile => "java.io.File", ScreenshotResolver => "android.content.ContentResolver" },
        methods {
            fn cache_dir { name = "getCacheDir", sig = () -> ScreenshotFile },
            fn external_files_dir { name = "getExternalFilesDir", sig = (kind: JString) -> ScreenshotFile },
            fn resolver { name = "getContentResolver", sig = () -> ScreenshotResolver },
        }
    }
    bind_java_type! {
        ScreenshotFile => "java.io.File",
        methods { fn path { name = "getAbsolutePath", sig = () -> JString }, }
    }
    bind_java_type! {
        ScreenshotVersion => "android.os.Build$VERSION",
        fields { static sdk { name = "SDK_INT", sig = jint, get = sdk }, }
    }
    bind_java_type! {
        ScreenshotValues => "android.content.ContentValues",
        constructors { fn new(), },
        methods { fn put { sig = (key: JString, value: JString) }, }
    }
    bind_java_type! {
        ScreenshotUri => "android.net.Uri",
        methods { static fn parse { sig = (text: JString) -> ScreenshotUri }, }
    }
    bind_java_type! {
        ScreenshotDescriptor => "android.os.ParcelFileDescriptor",
        methods { fn detach_fd { name = "detachFd", sig = () -> jint }, }
    }
    bind_java_type! {
        ScreenshotResolver => "android.content.ContentResolver",
        type_map = {
            ScreenshotUri => "android.net.Uri",
            ScreenshotValues => "android.content.ContentValues",
            ScreenshotDescriptor => "android.os.ParcelFileDescriptor",
        },
        methods {
            fn insert { sig = (uri: ScreenshotUri, values: ScreenshotValues) -> ScreenshotUri },
            fn open_fd { name = "openFileDescriptor", sig = (uri: ScreenshotUri, mode: JString) -> ScreenshotDescriptor },
            fn update { sig = (uri: ScreenshotUri, values: ScreenshotValues, selection: JString, args: JString[]) -> jint },
            fn delete { sig = (uri: ScreenshotUri, selection: JString, args: JString[]) -> jint },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_capture_saves_original_dimensions_while_paused() {
        let temporary = Temporary::new(&std::env::temp_dir()).unwrap();
        let source = temporary.0.join("paused.y4m");
        let mut video = b"YUV4MPEG2 W32 H18 F30:1 Ip A1:1 C420jpeg\n".to_vec();
        for _ in 0..90 {
            video.extend(b"FRAME\n");
            video.extend([128u8; 32 * 18 * 3 / 2]);
        }
        fs::write(&source, video).unwrap();
        let mpv = libmpv2::Mpv::with_initializer(|init| {
            init.set_property("config", false)?;
            init.set_property("vo", "null")?;
            init.set_property("ao", "null")
        })
        .unwrap();
        mpv.set_property("pause", true).unwrap();
        crate::mpv_load::load_file_at(&mpv, source.to_str().unwrap(), 0.5).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !mpv
            .get_property::<f64>("time-pos")
            .is_ok_and(|position| (position - 0.5).abs() < 0.1)
        {
            assert!(Instant::now() < deadline, "paused fixture did not load");
            std::thread::sleep(Duration::from_millis(20));
        }
        let position = mpv.get_property::<f64>("time-pos").unwrap();
        let image = temporary.0.join("capture.png");
        let client = CaptureClient::enqueue(&mpv, &image).unwrap();
        client.wait(|| false).unwrap();
        drop(client);
        verify_image(&image).unwrap();
        let png = fs::read(&image).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 32);
        assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), 18);
        assert!(mpv.get_property::<bool>("pause").unwrap());
        assert!((mpv.get_property::<f64>("time-pos").unwrap() - position).abs() < 0.05);
    }

    #[test]
    fn replaced_and_closed_sessions_cannot_capture() {
        assert_eq!(check_session(2, 2, true), Ok(()));
        assert_eq!(
            check_session(2, 3, true),
            Err(ScreenshotError::StaleSession)
        );
        assert_eq!(
            check_session(2, 3, false),
            Err(ScreenshotError::StaleSession)
        );
        assert_eq!(
            check_session(3, 3, false),
            Err(ScreenshotError::Unavailable)
        );
    }
    #[test]
    fn filenames_are_short_ascii_and_unique() {
        let a = filename();
        let b = filename();
        assert_ne!(a, b);
        assert!(a.len() < 100);
        assert!(
            a.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.'))
        );
        assert!(a.ends_with(".png"));
    }
    #[test]
    fn publish_preserves_existing_files_and_rejects_empty_images() {
        let temporary = Temporary::new(&std::env::temp_dir()).unwrap();
        let image = temporary.0.join("frame.png");
        File::create(&image).unwrap();
        assert_eq!(verify_image(&image), Err(ScreenshotError::SaveFailed));
        fs::write(&image, b"test-image").unwrap();
        let output = temporary.0.join("Pictures");
        let first = publish_file(&image, &output).unwrap();
        let second = publish_file(&image, &output).unwrap();
        assert_ne!(first.display_path, second.display_path);
        assert_eq!(fs::read(first.display_path).unwrap(), b"test-image");
        assert_eq!(fs::read(second.display_path).unwrap(), b"test-image");
    }
    #[test]
    fn public_errors_do_not_include_backend_details() {
        for error in [
            ScreenshotError::Unavailable,
            ScreenshotError::StaleSession,
            ScreenshotError::CaptureFailed,
            ScreenshotError::TimedOut,
            ScreenshotError::SaveFailed,
        ] {
            let message = error.to_string();
            assert!(!message.contains("http"));
            assert!(!message.contains("header"));
        }
    }
    #[cfg(not(any(target_os = "android", target_os = "windows")))]
    #[test]
    fn xdg_pictures_is_data_not_shell_code() {
        let home = Path::new("/home/test");
        assert_eq!(
            xdg_pictures("XDG_PICTURES_DIR=\"$HOME/Photos\"", home),
            Some(home.join("Photos"))
        );
        assert_eq!(
            xdg_pictures("XDG_PICTURES_DIR=\"/media/Pictures\"", home),
            Some(PathBuf::from("/media/Pictures"))
        );
        assert_eq!(
            xdg_pictures("XDG_PICTURES_DIR=\"$(secret)/Photos\"", home),
            None
        );
        assert_eq!(xdg_pictures("XDG_PICTURES_DIR=\"relative\"", home), None);
    }
}
