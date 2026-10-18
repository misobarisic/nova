//! Tracking regressions: one executable, isolated cases.

#[path = "support/isolation.rs"]
mod isolation;

#[path = "tracking/ui.rs"]
mod ui;

#[path = "tracking/detail_tab.rs"]
mod detail_tab;

#[path = "tracking/completion_rating.rs"]
mod completion_rating;

#[path = "tracking/services_scope.rs"]
mod services_scope;

#[path = "tracking/library_mode.rs"]
mod library_mode;
