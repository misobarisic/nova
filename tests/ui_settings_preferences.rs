//! Settings preferences regressions: one executable, isolated cases.

#[path = "support/isolation.rs"]
mod isolation;

#[path = "support/destinations.rs"]
mod destinations;

#[path = "support/settings.rs"]
mod settings_support;

#[path = "settings_preferences/support.rs"]
mod preference_support;

#[path = "settings_preferences/downloads.rs"]
mod downloads;

#[path = "settings_preferences/home_catalogs.rs"]
mod home_catalogs;

#[path = "settings_preferences/image_resolution.rs"]
mod image_resolution;

#[path = "settings_preferences/playback_speed.rs"]
mod playback_speed;

#[path = "settings_preferences/screenshot_format.rs"]
mod screenshot_format;

#[path = "settings_preferences/intro_skip.rs"]
mod intro_skip;

#[path = "settings_preferences/playback_preferences_ui.rs"]
mod playback_preferences_ui;

#[path = "settings_preferences/subtitle_style.rs"]
mod subtitle_style;

#[path = "settings_preferences/interface_mode.rs"]
mod interface_mode;

#[path = "settings_preferences/stremio_flow.rs"]
mod stremio_flow;

#[path = "settings_preferences/stremio_motion.rs"]
mod stremio_motion;

#[path = "settings_preferences/catalog_grouping.rs"]
mod catalog_grouping;

#[path = "settings_preferences/paused_controls.rs"]
mod paused_controls;

#[path = "settings_preferences/casting_remux_buffer.rs"]
mod casting_remux_buffer;

#[path = "settings_preferences/loading_text.rs"]
mod loading_text;

#[path = "settings_preferences/listening.rs"]
mod listening;

#[path = "settings_preferences/source_recovery.rs"]
mod source_recovery;

#[path = "settings_preferences/spoiler.rs"]
mod spoiler;

#[path = "settings_preferences/torrent_ram.rs"]
mod torrent_ram;
