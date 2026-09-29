//! Small shared helpers: KV JSON IO, atomic file writes, hashing,
//! and human-readable formatting.
use super::*;

/// Deserialize a JSON string from the KV store.
pub(crate) fn read_json<T: serde::de::DeserializeOwned>(key: &str) -> Option<T> {
    let s = storage::get_str(key)?;
    serde_json::from_str(&s).ok()
}
/// Serialize a value to JSON and write it to the KV store.
pub(crate) fn write_json(key: &str, value: &(impl Serialize + ?Sized)) {
    match serde_json::to_string(value) {
        Ok(s) => storage::set_str(key, &s),
        Err(e) => eprintln!("nova: serialize {key}: {e}"),
    }
}
/// Human-readable disk usage for the Settings page, e.g.
/// `"128.4 MB · 1,203 files"`. Pure (cross-platform for tests).
pub(crate) fn format_disk_usage(bytes: u64, files: usize) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let b = bytes as f64;
    let size = if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    };
    format!("{size} · {}", text::files_label(files))
}
/// Human-readable transfer rate for the player status line, e.g.
/// `"1.2 MB"` for 1_258_291 bytes/second.
pub(crate) fn format_rate(bytes_per_sec: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let b = bytes_per_sec as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes_per_sec} B")
    }
}
/// Thousands-grouped count (`1203` → `"1,203"`) for the usage readout.
pub(crate) fn grouped_count(n: usize) -> String {
    let digits: Vec<char> = n.to_string().chars().collect();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.iter().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*ch);
    }
    out
}
/// Write `contents` to `path` via a temp file + rename, so a crash mid-write
/// never leaves a truncated file behind.
#[allow(dead_code)]
#[cfg(feature = "desktop")]
pub(crate) fn atomic_write(path: &Path, contents: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, path)
}
/// FNV-1a 64-bit; used only to derive stable, filesystem-safe cache file
/// names (manifests, poster originals) from their URLs.
pub(crate) fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
