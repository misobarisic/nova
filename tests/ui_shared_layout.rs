//! Shared layout regressions: one executable, isolated cases.

#[path = "support/isolation.rs"]
mod isolation;

#[path = "shared_layout/animation_feedback.rs"]
mod animation_feedback;

#[path = "shared_layout/control_polish.rs"]
mod control_polish;

#[path = "shared_layout/automatic_next_episode.rs"]
mod automatic_next_episode;

#[path = "shared_layout/desktop_action_menus.rs"]
mod desktop_action_menus;

#[path = "shared_layout/next_episode_banner.rs"]
mod next_episode_banner;

#[path = "shared_layout/page_layout_fit.rs"]
mod page_layout_fit;

#[path = "shared_layout/searchfield_overflow.rs"]
mod searchfield_overflow;
