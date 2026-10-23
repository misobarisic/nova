//! Settings controls regressions: one executable, isolated cases.

#[path = "support/isolation.rs"]
mod isolation;

#[path = "settings_controls/language.rs"]
mod language;

#[path = "settings_controls/selectors.rs"]
mod selectors;

#[path = "settings_controls/slider_drag.rs"]
mod slider_drag;

#[path = "settings_controls/state_motion.rs"]
mod state_motion;

#[path = "settings_controls/theme.rs"]
mod theme;
