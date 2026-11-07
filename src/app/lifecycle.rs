//! Android activity lifecycle events and the work each one owes.
//!
//! The listener in `lib.rs` only translates `android-activity` events into
//! [`Event`] and carries out the returned [`Plan`], so the decision itself can
//! be tested on the host.
#[cfg(target_os = "android")]
use super::{bridge::with_global_bridge, persistence};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    Pause,
    Resume,
    Stop,
    SaveState,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Plan {
    /// New foreground state for the sync engine.
    pub sync_foreground: Option<bool>,
    /// Leave the foreground: pause playback unless picture-in-picture keeps it.
    pub pause_playback: bool,
    /// Return to the foreground: resume playback and cache maintenance.
    pub resume_playback: bool,
    /// Persist pending state before the process can be killed.
    pub flush: bool,
}

pub(crate) fn plan(event: Event) -> Plan {
    match event {
        Event::Pause => Plan {
            sync_foreground: Some(false),
            pause_playback: true,
            ..Plan::default()
        },
        Event::Resume => Plan {
            sync_foreground: Some(true),
            resume_playback: true,
            ..Plan::default()
        },
        // The process can be killed after Stop or SaveState without Destroy;
        // persist the debounced settings save and the playback position first.
        Event::Stop | Event::SaveState => Plan {
            flush: true,
            ..Plan::default()
        },
    }
}

/// Persist what the Pause checkpoint does not reach: a debounced settings save
/// and the playback position, then wait for the writer to drain. Runs on the
/// event-loop thread, like the Pause checkpoint.
#[cfg(target_os = "android")]
pub(crate) fn flush() {
    // Clone under the registry lock so persistence runs after it is released.
    if let Some(bridge) = with_global_bridge(Clone::clone) {
        bridge.note_player_progress_from_ui();
        bridge.flush_pending_settings();
        bridge.persist_playback_checkpoint(false);
    }
    persistence::flush_report();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pause_and_resume_switch_the_foreground_state() {
        assert_eq!(
            plan(Event::Pause),
            Plan {
                sync_foreground: Some(false),
                pause_playback: true,
                ..Plan::default()
            }
        );
        assert_eq!(
            plan(Event::Resume),
            Plan {
                sync_foreground: Some(true),
                resume_playback: true,
                ..Plan::default()
            }
        );
    }

    #[test]
    fn stop_and_save_state_flush_pending_state() {
        // The process can be killed after Stop or SaveState without Destroy.
        for event in [Event::Stop, Event::SaveState] {
            let plan = plan(event);
            assert!(plan.flush, "{event:?} must persist pending state");
            assert_eq!(plan.sync_foreground, None);
            assert!(!plan.pause_playback && !plan.resume_playback);
        }
    }

    #[test]
    fn pause_and_resume_do_not_flush_through_the_lifecycle_barrier() {
        // Pause persists through the playback checkpoint, which already waits
        // for the writer; Resume has nothing to persist.
        assert!(!plan(Event::Pause).flush);
        assert!(!plan(Event::Resume).flush);
    }
}
