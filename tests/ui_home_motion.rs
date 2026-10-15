//! Home motion regressions: one executable, isolated cases.

#[path = "support/isolation.rs"]
mod isolation;

#[path = "home_motion/carousel_flick.rs"]
mod carousel_flick;

#[path = "home_motion/catalog_layout.rs"]
mod catalog_layout;

#[path = "home_motion/showcase_rotation.rs"]
mod showcase_rotation;

#[path = "home_motion/showcase_swipe.rs"]
mod showcase_swipe;

#[path = "home_motion/hero_title_logo.rs"]
mod hero_title_logo;

#[path = "home_motion/remote_devices.rs"]
mod remote_devices;
