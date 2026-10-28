//! Player integration pilot: one linked suite, a fresh subprocess per case.

#[path = "support/isolation.rs"]
mod isolation;

#[path = "player/double_tap.rs"]
mod double_tap;

#[path = "player/gestures.rs"]
mod gestures;

#[path = "player/hold_motion.rs"]
mod hold_motion;

#[path = "player/osd_header.rs"]
mod osd_header;

#[path = "player/osd_touch.rs"]
mod osd_touch;

#[path = "player/shortcuts.rs"]
mod shortcuts;

#[path = "player/buffered_progress.rs"]
mod buffered_progress;

#[path = "player/chapters.rs"]
mod chapters;

#[path = "player/subtitle_lift.rs"]
mod subtitle_lift;

#[path = "player/end_menu.rs"]
mod end_menu;

#[path = "player/intro_skip.rs"]
mod intro_skip;

#[path = "player/remote_controls.rs"]
mod remote_controls;

#[path = "player/settings_scroll.rs"]
mod settings_scroll;

#[path = "player/tv_controls.rs"]
mod tv_controls;

#[path = "player/party.rs"]
mod party;

#[path = "player/tools.rs"]
mod tools;

#[path = "player/paused_controls.rs"]
mod paused_controls;

#[path = "player/loading.rs"]
mod loading;

#[path = "player/stats.rs"]
mod stats;

#[path = "player/next_episode_control.rs"]
mod next_episode_control;
