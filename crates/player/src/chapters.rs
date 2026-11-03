//! Embedded chapter metadata, owned by the loaded source rather than its name.
use libmpv2::Mpv;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Chapter {
    pub position: f32,
    pub title: String,
}

pub(super) fn read(mpv: &Mpv, duration: f64, chapter_label: &str) -> Vec<Chapter> {
    mpv.get_property::<String>("chapter-list")
        .ok()
        .map(|raw| parse_with_label(&raw, duration, chapter_label))
        .unwrap_or_default()
}

#[cfg(test)]
fn parse(raw: &str, duration: f64) -> Vec<Chapter> {
    parse_with_label(raw, duration, nova_ui::backend_text::tr("Chapter"))
}

fn parse_with_label(raw: &str, duration: f64, chapter_label: &str) -> Vec<Chapter> {
    if !duration.is_finite() || duration <= 0.0 {
        return Vec::new();
    }
    let Ok(serde_json::Value::Array(rows)) = serde_json::from_str(raw) else {
        return Vec::new();
    };
    let mut chapters: Vec<_> = rows
        .iter()
        .take(256)
        .enumerate()
        .filter_map(|(index, row)| {
            let position = row["time"].as_f64()?;
            if !position.is_finite()
                || position < 0.0
                || position >= duration
                || position > f32::MAX as f64
            {
                return None;
            }
            let title = row["title"].as_str().unwrap_or_default().trim();
            Some(Chapter {
                position: position as f32,
                title: if title.is_empty() {
                    format!("{} {}", chapter_label, index + 1)
                } else {
                    title.chars().take(256).collect()
                },
            })
        })
        .collect();
    chapters.sort_by(|a, b| a.position.total_cmp(&b.position));
    chapters.dedup_by(|a, b| a.position == b.position);
    chapters
}

/// Cached publication receipts; Slint owns the last bounded chapter/badge
/// values. An unchanged hit emits no update, avoiding a clone/model scan too.
#[derive(Default)]
pub(super) struct OsdMetadata {
    source: Option<SourceKey>,
    observer: Option<Observer>,
    receipt: Receipt,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SourceKey {
    session: u64,
    epoch: u64,
    core: usize,
}
const FALLBACK_REFRESH: std::time::Duration = std::time::Duration::from_secs(5);
const EVENT_BUDGET: usize = 64;

#[derive(Default)]
struct Receipt {
    duration: Option<u64>,
    chapter_label: String,
    chapters_at: Option<std::time::Instant>,
    badges_at: Option<std::time::Instant>,
}
#[derive(Clone, Copy, Default)]
struct Dirty {
    chapters: bool,
    badges: bool,
}
impl Dirty {
    fn all() -> Self {
        Self {
            chapters: true,
            badges: true,
        }
    }
}
pub(super) struct Update {
    pub chapters: Option<Vec<Chapter>>,
    pub badges: Option<Vec<slint::SharedString>>,
}
impl Receipt {
    fn refresh(
        &self,
        duration: f64,
        chapter_label: &str,
        now: std::time::Instant,
        dirty: Dirty,
    ) -> Dirty {
        // Observer failures use the same five-second safety refresh. This is
        // at most 5s + one eligible 250ms tick of foreground display staleness;
        // event-loop suspension/blocking can extend wall-clock latency.
        let due = |at: Option<std::time::Instant>| {
            at.is_none_or(|at| now.saturating_duration_since(at) >= FALLBACK_REFRESH)
        };
        Dirty {
            chapters: dirty.chapters
                || self.duration != Some(duration.to_bits())
                || self.chapter_label != chapter_label
                || due(self.chapters_at),
            badges: dirty.badges || due(self.badges_at),
        }
    }
}
impl OsdMetadata {
    /// Called only under State.mpv, then State.osd_metadata, in that order.
    /// Lifecycle callbacks only advance an atomic epoch, never take this lock.
    pub fn poll(
        &mut self,
        mpv: &Mpv,
        session: u64,
        epoch: u64,
        duration: f64,
        chapter_label: &str,
        now: std::time::Instant,
    ) -> Update {
        let key = SourceKey {
            session,
            epoch,
            core: mpv.ctx.as_ptr() as usize,
        };
        if self.source != Some(key) {
            // Recreate even for the same URL/core: old client events cannot
            // authorize a new source, renderer generation or recovery reload.
            self.observer = Observer::new(mpv);
            self.source = Some(key);
            self.receipt = Receipt::default();
        }
        let dirty = self
            .observer
            .as_mut()
            .map(Observer::drain)
            .unwrap_or_default();
        let refresh = self.receipt.refresh(duration, chapter_label, now, dirty);
        let chapters = refresh.chapters.then(|| {
            let chapters = read(mpv, duration, chapter_label);
            self.receipt.duration = Some(duration.to_bits());
            self.receipt.chapter_label = chapter_label.to_owned();
            self.receipt.chapters_at = Some(now);
            chapters
        });
        let badges = refresh.badges.then(|| {
            let badges = super::media_badges::read(mpv);
            self.receipt.badges_at = Some(now);
            badges
        });
        Update { chapters, badges }
    }
}

/// A separately owned native client: never consume the main mpv event queue
/// used by loading/screenshots. NONE notifications avoid copying property data.
struct Observer(std::ptr::NonNull<libmpv2_sys::mpv_handle>);
// SAFETY: mpv clients can move between threads. This client is exclusively
// accessed under State.osd_metadata; only that lock's owner drains its events.
unsafe impl Send for Observer {}
impl Observer {
    fn new(mpv: &Mpv) -> Option<Self> {
        let raw = unsafe {
            libmpv2_sys::mpv_create_client(mpv.ctx.as_ptr(), c"nova_osd_metadata".as_ptr())
        };
        let observer = Self(std::ptr::NonNull::new(raw)?);
        // This client needs metadata changes, never per-frame legacy ticks.
        // Disabling them is private to this client and avoids a high-rate
        // playback stream needlessly exhausting the bounded drain budget.
        unsafe {
            libmpv2_sys::mpv_request_event(
                observer.0.as_ptr(),
                libmpv2_sys::mpv_event_id_MPV_EVENT_TICK,
                0,
            );
        }
        for (index, property) in [
            c"chapter-list",
            c"duration",
            c"video-params",
            c"video-format",
            c"current-tracks/audio",
            c"audio-params",
            c"track-list",
            c"aid",
            c"vid",
        ]
        .iter()
        .enumerate()
        {
            let result = unsafe {
                libmpv2_sys::mpv_observe_property(
                    observer.0.as_ptr(),
                    index as u64 + 1,
                    property.as_ptr(),
                    libmpv2::mpv_format::None,
                )
            };
            if result < 0 {
                return None;
            }
        }
        Some(observer)
    }
    fn drain(&mut self) -> Dirty {
        drain_events(|| {
            // Zero timeout: never wait for an event while holding the UI/mpv lock.
            let event = unsafe { &*libmpv2_sys::mpv_wait_event(self.0.as_ptr(), 0.0) };
            (event.event_id, event.reply_userdata)
        })
    }
}

fn drain_events(mut next: impl FnMut() -> (libmpv2_sys::mpv_event_id, u64)) -> Dirty {
    let mut dirty = Dirty::default();
    for _ in 0..EVENT_BUDGET {
        let (event, reply) = next();
        match event {
            libmpv2_sys::mpv_event_id_MPV_EVENT_NONE => return dirty,
            libmpv2_sys::mpv_event_id_MPV_EVENT_PROPERTY_CHANGE => match reply {
                1 | 2 => dirty.chapters = true,
                3..=9 => dirty.badges = true,
                _ => dirty = Dirty::all(),
            },
            libmpv2_sys::mpv_event_id_MPV_EVENT_START_FILE
            | libmpv2_sys::mpv_event_id_MPV_EVENT_FILE_LOADED
            | libmpv2_sys::mpv_event_id_MPV_EVENT_END_FILE
            | libmpv2_sys::mpv_event_id_MPV_EVENT_VIDEO_RECONFIG
            | libmpv2_sys::mpv_event_id_MPV_EVENT_AUDIO_RECONFIG
            | libmpv2_sys::mpv_event_id_MPV_EVENT_QUEUE_OVERFLOW
            | libmpv2_sys::mpv_event_id_MPV_EVENT_SHUTDOWN => dirty = Dirty::all(),
            _ => {}
        }
    }
    // Queue overflow or a saturated budget cannot leave a stale receipt.
    Dirty::all()
}

impl Drop for Observer {
    fn drop(&mut self) {
        // Destroy only this client, never terminate the shared playback core.
        unsafe {
            libmpv2_sys::mpv_destroy(self.0.as_ptr());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warm_receipt_needs_no_update_until_dirty_duration_language_or_fallback() {
        let now = std::time::Instant::now();
        let receipt = Receipt {
            duration: Some(60f64.to_bits()),
            chapter_label: "Chapter".into(),
            chapters_at: Some(now),
            badges_at: Some(now),
        };
        let warm = receipt.refresh(60.0, "Chapter", now, Dirty::default());
        assert!(!warm.chapters && !warm.badges);
        let before_deadline = receipt.refresh(
            60.0,
            "Chapter",
            now + FALLBACK_REFRESH - std::time::Duration::from_nanos(1),
            Dirty::default(),
        );
        assert!(!before_deadline.chapters && !before_deadline.badges);
        let expired = receipt.refresh(60.0, "Chapter", now + FALLBACK_REFRESH, Dirty::default());
        assert!(expired.chapters && expired.badges);
        let duration = receipt.refresh(61.0, "Chapter", now, Dirty::default());
        assert!(duration.chapters && !duration.badges);
        let language = receipt.refresh(60.0, "Poglavlje", now, Dirty::default());
        assert!(language.chapters && !language.badges);
        let audio = receipt.refresh(
            60.0,
            "Chapter",
            now,
            Dirty {
                chapters: false,
                badges: true,
            },
        );
        assert!(!audio.chapters && audio.badges);
    }

    #[test]
    fn observer_budget_and_overflow_dirty_both_receipts_without_waiting() {
        use libmpv2_sys::*;
        let mut calls = 0;
        let flooded = drain_events(|| {
            calls += 1;
            (mpv_event_id_MPV_EVENT_PROPERTY_CHANGE, 1)
        });
        assert_eq!(calls, EVENT_BUDGET);
        assert!(flooded.chapters && flooded.badges);
        let mut events = [
            (mpv_event_id_MPV_EVENT_QUEUE_OVERFLOW, 0),
            (mpv_event_id_MPV_EVENT_NONE, 0),
        ]
        .into_iter();
        let overflow = drain_events(|| events.next().unwrap());
        assert!(overflow.chapters && overflow.badges);
        let mut events = [
            (mpv_event_id_MPV_EVENT_PROPERTY_CHANGE, 5),
            (mpv_event_id_MPV_EVENT_NONE, 0),
        ]
        .into_iter();
        let audio = drain_events(|| events.next().unwrap());
        assert!(!audio.chapters && audio.badges);
        let empty = drain_events(|| (mpv_event_id_MPV_EVENT_NONE, 0));
        assert!(!empty.chapters && !empty.badges);
    }

    #[test]
    fn embedded_chapters_are_sorted_bounded_and_not_invented() {
        let chapters = parse(
            r#"[{"time":12,"title":"Second"},{"time":0,"title":"Opening"},{"time":12,"title":"Duplicate"},{"time":-1},{"time":60},{"title":"Missing"}]"#,
            60.0,
        );
        assert_eq!(chapters.len(), 2);
        assert_eq!(
            (chapters[0].position, chapters[0].title.as_str()),
            (0.0, "Opening")
        );
        assert_eq!(
            (chapters[1].position, chapters[1].title.as_str()),
            (12.0, "Second")
        );
        assert!(parse("[]", 60.0).is_empty());
        assert!(parse("invalid", 60.0).is_empty());
        assert!(parse(r#"[{"time":0}]"#, f64::NAN).is_empty());
        assert_eq!(parse(r#"[{"time":0}]"#, 60.0)[0].title, "Chapter 1");
    }
}
