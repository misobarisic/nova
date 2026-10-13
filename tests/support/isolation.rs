//! Share linked UI suites while giving every case a fresh Slint backend.
use std::{
    ffi::OsStr,
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

const CHILD_CASE: &str = "NOVA_UI_TEST_CASE";
const CHILD_ROOT: &str = "NOVA_UI_TEST_ROOT";
static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

struct CaseRoot(PathBuf);

impl CaseRoot {
    fn new() -> Self {
        loop {
            let root = std::env::temp_dir().join(format!(
                "nova-ui-test-{}-{}",
                std::process::id(),
                NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&root) {
                Ok(()) => {
                    let root = Self(root);
                    for directory in ["data", "cache", "config", "tmp"] {
                        fs::create_dir(root.0.join(directory)).unwrap();
                    }
                    return root;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("cannot create UI test root: {error}"),
            }
        }
    }
}

impl Drop for CaseRoot {
    fn drop(&mut self) {
        // The child has exited, so Windows file handles are closed as well.
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Returns true only inside the subprocess that owns this particular case.
/// Call before initializing Slint, changing the locale or touching app storage.
pub fn run(full_name: &str) -> bool {
    // Rust's test harness omits the crate name from its module-qualified names.
    let (_, name) = full_name.split_once("::").unwrap();
    if let Some(selected) = std::env::var_os(CHILD_CASE) {
        assert_eq!(
            selected.as_os_str(),
            OsStr::new(name),
            "child must run exactly one case"
        );
        let root = PathBuf::from(std::env::var_os(CHILD_ROOT).unwrap());
        fs::write(root.join("case-started"), name).unwrap();
        return true;
    }

    let root = CaseRoot::new();
    let output = Command::new(std::env::current_exe().unwrap())
        .env(CHILD_CASE, name)
        .env(CHILD_ROOT, &root.0)
        .env("XDG_DATA_HOME", root.0.join("data"))
        .env("XDG_CACHE_HOME", root.0.join("cache"))
        .env("XDG_CONFIG_HOME", root.0.join("config"))
        .env("APPDATA", root.0.join("data"))
        .env("LOCALAPPDATA", root.0.join("cache"))
        .env("TMPDIR", root.0.join("tmp"))
        .env("TMP", root.0.join("tmp"))
        .env("TEMP", root.0.join("tmp"))
        // Include ignored cases here: the parent harness still owns whether a
        // native-display case is selected, while the child runs that exact case.
        .args([
            "--exact",
            name,
            "--nocapture",
            "--include-ignored",
            "--test-threads=1",
        ])
        .output()
        .unwrap_or_else(|error| panic!("cannot launch UI case {name}: {error}"));
    assert!(
        output.status.success(),
        "UI case {name} failed ({}):\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // A mistyped --exact filter exits successfully with zero tests. Require
    // a receipt from the selected body instead of trusting that exit alone.
    assert_eq!(
        fs::read_to_string(root.0.join("case-started"))
            .ok()
            .as_deref(),
        Some(name),
        "child filter must execute the selected UI case"
    );
    false
}
