//! Android back navigation regressions: one executable, isolated cases.

#[path = "support/isolation.rs"]
mod isolation;

#[path = "android_back_navigation/focus.rs"]
mod focus;

#[path = "android_back_navigation/nav.rs"]
mod nav;

#[path = "android_back_navigation/resize.rs"]
mod resize;

#[path = "android_back_navigation/selector_entry.rs"]
mod selector_entry;

#[path = "android_back_navigation/stream_selector.rs"]
mod stream_selector;
