//! Discover regressions: one linked suite, a fresh subprocess per case.

#[path = "support/isolation.rs"]
mod isolation;

#[path = "discover/retry.rs"]
mod retry;

#[path = "discover/reveal_and_filters.rs"]
mod reveal_and_filters;

#[path = "discover/search_history_ui.rs"]
mod search_history_ui;

#[path = "discover/search_isolation.rs"]
mod search_isolation;

#[path = "discover/search_ui.rs"]
mod search_ui;

#[path = "discover/styling.rs"]
mod styling;
