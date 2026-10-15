//! Stremio Settings integration: each case owns an isolated Slint backend.
#[path = "support/isolation.rs"]
mod isolation;

#[path = "ui_settings_stremio/flow.rs"]
mod flow;
