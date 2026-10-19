//! Addons regressions: one executable, isolated cases.

#[path = "support/isolation.rs"]
mod isolation;

#[path = "support/destinations.rs"]
mod destinations;

#[path = "support/settings.rs"]
mod settings;

#[path = "addons/capability_config.rs"]
mod capability_config;

#[path = "addons/zip_install_ui.rs"]
mod zip_install_ui;

#[path = "addons/collection_config.rs"]
mod collection_config;

#[path = "addons/schema_config.rs"]
mod schema_config;

#[path = "addons/sort_config.rs"]
mod sort_config;

#[path = "addons/addon_filters.rs"]
mod addon_filters;

#[path = "addons/browser.rs"]
mod browser;

#[path = "addons/addon_row_fit.rs"]
mod addon_row_fit;
