//! Opt-in, payload-free UI timing through the existing tracing subscriber.
use std::sync::LazyLock;
use std::time::Instant;

static RUN_ID: LazyLock<String> = LazyLock::new(|| {
    std::env::var("NOVA_PERF_RUN_ID")
        .ok()
        .filter(|id| {
            !id.is_empty()
                && id.len() <= 64
                && id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        })
        .unwrap_or_else(|| "unassigned".into())
});

pub(super) fn enabled() -> bool {
    tracing::enabled!(target: "nova_perf", tracing::Level::DEBUG)
}

pub(super) struct Timer {
    operation: &'static str,
    start: Option<Instant>,
}

impl Timer {
    pub(super) fn new(operation: &'static str) -> Self {
        Self::with_clock(operation, enabled(), Instant::now)
    }

    fn with_clock(operation: &'static str, enabled: bool, now: impl FnOnce() -> Instant) -> Self {
        Self {
            operation,
            start: enabled.then(now),
        }
    }

    fn elapsed_us(&self) -> Option<u64> {
        self.start.map(|start| micros(start.elapsed()))
    }
}

fn micros(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

impl Drop for Timer {
    fn drop(&mut self) {
        if let Some(elapsed_us) = self.elapsed_us() {
            tracing::debug!(target: "nova_perf", run_id = RUN_ID.as_str(),
                operation = self.operation, elapsed_us, "perf sample");
        }
    }
}

/// One instance per AppWindow; render completion is not display presentation.
#[derive(Default)]
pub(super) struct WindowStartup {
    entry: Option<Instant>,
    last_render: Option<Instant>,
    first_render: bool,
    useful_rows: bool,
}

impl WindowStartup {
    pub(super) fn app_entry() -> Self {
        let entry = enabled().then(Instant::now);
        if entry.is_some() {
            tracing::debug!(target: "nova_perf", run_id = RUN_ID.as_str(),
                operation = "startup.app_entry", "perf marker");
        }
        Self {
            entry,
            ..Self::default()
        }
    }

    pub(super) fn after_render(&mut self) {
        let Some(entry) = self.entry else { return };
        let now = Instant::now();
        if !std::mem::replace(&mut self.first_render, true) {
            tracing::debug!(target: "nova_perf", run_id = RUN_ID.as_str(),
                operation = "startup.first_completed_render", elapsed_us = micros(now.duration_since(entry)),
                "perf marker");
        }
        if let Some(previous) = self.last_render.replace(now) {
            tracing::debug!(target: "nova_perf", run_id = RUN_ID.as_str(),
                operation = "render.completion_interval", elapsed_us = micros(now.duration_since(previous)),
                "perf sample");
        }
    }

    pub(super) fn needs_rows(&self) -> bool {
        self.entry.is_some() && !self.useful_rows
    }

    pub(super) fn observe_rows(&mut self, rows: usize) {
        if rows > 0 && self.needs_rows() {
            self.useful_rows = true;
            tracing::debug!(target: "nova_perf", run_id = RUN_ID.as_str(),
                operation = "startup.first_useful_rows_observed", rows,
                elapsed_us = micros(self.entry.unwrap().elapsed()), "perf marker");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_does_not_read_clock() {
        let timer = Timer::with_clock("test.disabled", false, || panic!("disabled clock read"));
        assert!(timer.start.is_none());
        assert!(timer.elapsed_us().is_none());
        tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), || {
            assert!(Timer::new("test.disabled_subscriber").start.is_none());
            assert!(WindowStartup::app_entry().entry.is_none());
        });
        let mut startup = WindowStartup::default();
        startup.after_render();
        startup.observe_rows(10);
        assert!(!startup.first_render);
        assert!(!startup.useful_rows);
    }

    #[test]
    fn deliberately_blocked_operation_has_measurable_duration() {
        let timer = Timer::with_clock("test.blocked", true, Instant::now);
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(timer.elapsed_us().unwrap() >= 5_000);
    }

    #[derive(Clone)]
    struct Buffer(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn enabled_events_group_by_operation_and_run_and_render_once() {
        let bytes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let writer = Buffer(bytes.clone());
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter("nova_perf=debug")
            .with_ansi(false)
            .without_time()
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let mut startup = WindowStartup::app_entry();
            startup.after_render();
            startup.after_render();
            startup.observe_rows(0);
            startup.observe_rows(3);
            startup.observe_rows(4);
            let _timer = Timer::new("test.enabled");
            std::thread::sleep(std::time::Duration::from_millis(5));
        });
        let output = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        assert_eq!(output.matches("startup.app_entry").count(), 1);
        assert_eq!(output.matches("startup.first_completed_render").count(), 1);
        assert_eq!(output.matches("render.completion_interval").count(), 1);
        assert_eq!(
            output.matches("startup.first_useful_rows_observed").count(),
            1
        );
        assert!(output.contains("test.enabled"));
        assert!(output.lines().all(|line| line.contains("run_id=")));
    }

    #[test]
    fn startup_markers_are_once_per_window_and_ignore_empty_rows() {
        let mut startup = WindowStartup {
            entry: Some(Instant::now()),
            ..Default::default()
        };
        startup.after_render();
        let first = startup.last_render;
        startup.after_render();
        assert!(startup.first_render);
        assert!(startup.last_render >= first);
        startup.observe_rows(0);
        assert!(startup.needs_rows());
        startup.observe_rows(3);
        assert!(!startup.needs_rows());
        startup.observe_rows(4);
        assert!(startup.useful_rows);
        assert!(!WindowStartup::default().first_render);
    }
}
