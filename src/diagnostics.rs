//! Process-wide tracing setup shared by foreground and Android job-only starts.
//! Leaf crates emit events but never install a subscriber themselves.
use std::sync::OnceLock;
use tracing_subscriber::EnvFilter;

fn filter(value: &str) -> EnvFilter {
    // Environment filters are operator configuration, not payload matchers.
    EnvFilter::builder().with_regex(false).parse_lossy(value)
}

pub(crate) fn init() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        let value = std::env::var("RUST_LOG").unwrap_or_else(|_| "warn,nova_sync=info".into());
        #[cfg(not(target_os = "android"))]
        let writer = std::io::stderr;
        #[cfg(target_os = "android")]
        let writer = android::Logcat;
        // Respect an embedding application's existing global subscriber.
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter(&value))
            .with_ansi(false)
            .with_writer(writer)
            .try_init();
    });
}

#[cfg(target_os = "android")]
mod android {
    use std::ffi::{CString, c_char, c_int};
    use std::io::{self, Write};
    use tracing::{Level, Metadata};
    use tracing_subscriber::fmt::MakeWriter;

    #[link(name = "log")]
    unsafe extern "C" {
        fn __android_log_write(priority: c_int, tag: *const c_char, text: *const c_char) -> c_int;
    }

    pub(super) struct Logcat;
    pub(super) struct Event {
        priority: c_int,
        bytes: Vec<u8>,
    }
    impl<'a> MakeWriter<'a> for Logcat {
        type Writer = Event;
        fn make_writer(&'a self) -> Event {
            Event {
                priority: 4,
                bytes: Vec::new(),
            }
        }
        fn make_writer_for(&'a self, metadata: &Metadata<'_>) -> Event {
            let priority = match *metadata.level() {
                Level::ERROR => 6,
                Level::WARN => 5,
                Level::INFO => 4,
                Level::DEBUG => 3,
                Level::TRACE => 2,
            };
            Event {
                priority,
                bytes: Vec::new(),
            }
        }
    }
    impl Write for Event {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl Drop for Event {
        fn drop(&mut self) {
            // No Activity/stdout redirection is needed in a job-only process.
            // Keep each line below logcat's limit, including on UTF-8 text.
            let text = String::from_utf8_lossy(&self.bytes);
            for line in text.lines() {
                let mut end = line.len().min(3500);
                while !line.is_char_boundary(end) {
                    end -= 1;
                }
                let line = CString::new(line[..end].replace('\0', "\\0")).expect("NUL escaped");
                // SAFETY: both strings remain NUL-terminated and live for the
                // synchronous liblog call; no pointer is retained by Android.
                unsafe {
                    __android_log_write(self.priority, c"Nova".as_ptr(), line.as_ptr());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);
    impl Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn rust_log_filters_sync_events_and_preserves_structured_fields() {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let writer = Buffer(bytes.clone());
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(filter("warn,nova_sync=debug"))
            .with_ansi(false)
            .without_time()
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            tracing::debug!(target: "nova_sync", trigger = "explicit", record_count = 3, "sync event");
            tracing::debug!(target: "unrelated", "hidden debug event");
            tracing::trace!(target: "nova_sync", "hidden trace event");
            tracing::warn!(target: "unrelated", "visible warning");
        });
        let output = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        assert!(output.contains("trigger=\"explicit\""));
        assert!(output.contains("record_count=3"));
        assert!(output.contains("visible warning"));
        assert!(!output.contains("hidden"));
    }
}
