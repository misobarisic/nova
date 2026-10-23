//! Settings navigation regressions: one executable, isolated cases.

#[path = "support/isolation.rs"]
mod isolation;

#[path = "support/destinations.rs"]
mod destinations;

#[path = "settings_navigation/about.rs"]
mod about;

#[path = "settings_navigation/updates.rs"]
mod updates;

#[path = "settings_navigation/layout.rs"]
mod layout;

#[path = "settings_navigation/navigation.rs"]
mod navigation;

#[path = "settings_navigation/search.rs"]
mod search;

#[path = "settings_navigation/overflow.rs"]
mod overflow;

#[path = "settings_navigation/responsive_rows.rs"]
mod responsive_rows;

#[path = "settings_navigation/row_height.rs"]
mod row_height;

#[path = "settings_navigation/sticky_controls.rs"]
mod sticky_controls;
