//! Android back inputs regressions: one executable, isolated cases.

#[path = "support/isolation.rs"]
mod isolation;

#[path = "android_back_inputs/addon_actions.rs"]
mod addon_actions;

#[path = "android_back_inputs/transient_controls.rs"]
mod transient_controls;

#[path = "android_back_inputs/detail_input.rs"]
mod detail_input;

#[path = "android_back_inputs/settings_input.rs"]
mod settings_input;

#[path = "android_back_inputs/tracking.rs"]
mod tracking;
