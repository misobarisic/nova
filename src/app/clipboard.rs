//! Best-effort writes to the system clipboard.
//!
//! Shared by the app's copy actions: Settings → Sync ("Copy identity", "Copy
//! invite code") and Settings → Addons ("Copy link", which is where an addon's
//! install URL lives now that the row no longer prints it).

/// Copy `text` to the system clipboard. Best effort: a missing helper tool (or
/// a JNI failure on Android) is logged and otherwise ignored — the copy is a
/// convenience, never a step the caller must succeed at.
pub(crate) fn copy_to_clipboard(text: &str) {
    #[cfg(target_os = "android")]
    {
        if let Err(e) = crate::player::set_clipboard(text) {
            eprintln!("nova: {e}");
        }
    }
    #[cfg(not(target_os = "android"))]
    {
        use std::io::Write;
        let candidates: &[(&str, &[&str])] = &[
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ];
        for (program, args) in candidates {
            if let Ok(mut child) = std::process::Command::new(program)
                .args(*args)
                .stdin(std::process::Stdio::piped())
                .spawn()
            {
                if let Some(stdin) = child.stdin.as_mut() {
                    let _ = stdin.write_all(text.as_bytes());
                }
                let _ = child.wait();
                return;
            }
        }
        let _ = text;
    }
}
