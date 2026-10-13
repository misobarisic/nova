//! Streams motion regressions: one executable, isolated cases.

#[path = "support/isolation.rs"]
mod isolation;

#[path = "streams_motion/pill_axis_lock.rs"]
mod pill_axis_lock;

#[path = "streams_motion/pill_scroll.rs"]
mod pill_scroll;

#[path = "streams_motion/selector_transition.rs"]
mod selector_transition;
