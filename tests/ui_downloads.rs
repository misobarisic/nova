//! Downloads regressions: one executable, isolated cases.

#[path = "support/isolation.rs"]
mod isolation;

#[path = "downloads/calendar_downloads_navigation.rs"]
mod calendar_downloads_navigation;

#[path = "downloads/landscape.rs"]
mod landscape;

#[path = "downloads/page.rs"]
mod page;

#[path = "downloads/page_artwork_visibility.rs"]
mod page_artwork_visibility;

#[path = "downloads/tv_navigation.rs"]
mod tv_navigation;
