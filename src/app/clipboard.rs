//! Best-effort clipboard writes shared by identity, invite and addon actions.

pub(crate) fn copy_to_clipboard(text: &str) {
    if let Err(error) = prepare_copy().and_then(|target| target.copy(text)) {
        eprintln!("nova: clipboard: {error}");
    }
}

/// Capture thread-affine ownership on the UI thread before a background copy.
/// The prepared target contains no platform object or borrowed window.
pub(crate) struct CopyTarget {
    #[cfg(target_os = "windows")]
    window: isize,
}

pub(crate) fn prepare_copy() -> Result<CopyTarget, String> {
    #[cfg(target_os = "windows")]
    {
        #[link(name = "user32")]
        unsafe extern "system" {
            fn GetActiveWindow() -> *mut std::ffi::c_void;
        }
        let window = unsafe { GetActiveWindow() } as isize;
        if window == 0 {
            return Err("Clipboard window unavailable".into());
        }
        Ok(CopyTarget { window })
    }
    #[cfg(not(target_os = "windows"))]
    Ok(CopyTarget {})
}

impl CopyTarget {
    /// May wait for a platform helper; callers doing UI work should run this
    /// on a bounded worker and show success only after this result is Ok.
    pub(crate) fn copy(self, text: &str) -> Result<(), String> {
        #[cfg(target_os = "android")]
        {
            crate::player::set_clipboard(text)
        }
        #[cfg(target_os = "windows")]
        {
            windows_clipboard(text, self.window).map_err(|error| error.to_string())
        }
        #[cfg(not(any(target_os = "android", target_os = "windows")))]
        {
            #[cfg(target_os = "macos")]
            let candidates: &[(&str, &[&str])] = &[("pbcopy", &[])];
            #[cfg(not(target_os = "macos"))]
            let candidates: &[(&str, &[&str])] = &[
                ("wl-copy", &[]),
                ("xclip", &["-selection", "clipboard"]),
                ("xsel", &["--clipboard", "--input"]),
            ];
            if copy_with_helpers(text, candidates) {
                Ok(())
            } else {
                Err("Clipboard helper unavailable or failed".into())
            }
        }
    }
}

#[cfg(not(any(target_os = "android", target_os = "windows")))]
fn copy_with_helpers(text: &str, candidates: &[(&str, &[&str])]) -> bool {
    use std::io::Write;
    for (program, args) in candidates {
        let Ok(mut child) = std::process::Command::new(program)
            .args(*args)
            .stdin(std::process::Stdio::piped())
            .spawn()
        else {
            continue;
        };
        // Close stdin before waiting: clipboard helpers may wait for EOF.
        let written = child
            .stdin
            .take()
            .is_some_and(|mut stdin| stdin.write_all(text.as_bytes()).is_ok());
        let succeeded = child.wait().is_ok_and(|status| status.success());
        if written && succeeded {
            return true;
        }
    }
    false
}

#[cfg(target_os = "windows")]
fn windows_clipboard(text: &str, window: isize) -> std::io::Result<()> {
    use std::ffi::c_void;
    type Handle = *mut c_void;
    #[link(name = "user32")]
    unsafe extern "system" {
        fn OpenClipboard(window: Handle) -> i32;
        fn CloseClipboard() -> i32;
        fn EmptyClipboard() -> i32;
        fn SetClipboardData(format: u32, memory: Handle) -> Handle;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalAlloc(flags: u32, bytes: usize) -> Handle;
        fn GlobalLock(memory: Handle) -> Handle;
        fn GlobalUnlock(memory: Handle) -> i32;
        fn GlobalFree(memory: Handle) -> Handle;
    }
    // Win32 requires movable global memory and transfers ownership only after
    // SetClipboardData succeeds. RAII covers every earlier failure path.
    struct Memory(Handle);
    impl Drop for Memory {
        fn drop(&mut self) {
            unsafe {
                GlobalFree(self.0);
            }
        }
    }
    struct Clipboard;
    impl Drop for Clipboard {
        fn drop(&mut self) {
            unsafe {
                CloseClipboard();
            }
        }
    }
    let utf16: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    // SAFETY: allocation is sized for utf16, locking yields writable memory,
    // and all handles remain owned until the documented clipboard transfer.
    unsafe {
        let memory = Memory(GlobalAlloc(0x0002, std::mem::size_of_val(utf16.as_slice())));
        if memory.0.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let buffer = GlobalLock(memory.0).cast::<u16>();
        if buffer.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        std::ptr::copy_nonoverlapping(utf16.as_ptr(), buffer, utf16.len());
        GlobalUnlock(memory.0);
        // Supply the application's active HWND: EmptyClipboard makes it the
        // owner, which SetClipboardData requires for delayed ownership rules.
        if OpenClipboard(window as Handle) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let _clipboard = Clipboard;
        if EmptyClipboard() == 0 || SetClipboardData(13, memory.0).is_null() {
            return Err(std::io::Error::last_os_error());
        }
        std::mem::forget(memory);
    }
    Ok(())
}

#[cfg(all(test, unix, not(target_os = "android")))]
mod tests {
    use super::*;

    #[test]
    fn helper_failure_falls_through_and_stdin_is_closed() {
        let root = std::env::temp_dir().join(format!("nova-clipboard-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let output = root.join("copied");
        // Pass the path as a shell argument, never as executable text.
        let args = ["-c", "cat > \"$1\"", "clipboard", output.to_str().unwrap()];
        assert!(copy_with_helpers(
            "Unicode: č 🎬",
            &[("sh", &["-c", "cat >/dev/null; exit 1"]), ("sh", &args),]
        ));
        assert_eq!(std::fs::read_to_string(output).unwrap(), "Unicode: č 🎬");
        assert!(!copy_with_helpers("x", &[("sh", &["-c", "exit 1"])]));
        std::fs::remove_dir_all(root).unwrap();
    }
}
