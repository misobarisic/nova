//! My Library regressions: one linked suite, a fresh subprocess per case.

#[path = "support/isolation.rs"]
mod isolation;

#[path = "library/cards.rs"]
mod cards;

#[path = "library/category_filters.rs"]
mod category_filters;

#[path = "library/duplicates.rs"]
mod duplicates;

#[path = "library/header_scroll.rs"]
mod header_scroll;

#[path = "library/scroll_restore.rs"]
mod scroll_restore;

#[path = "library/search_animation.rs"]
mod search_animation;
