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
