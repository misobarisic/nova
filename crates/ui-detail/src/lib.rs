// This page visual tree compiles independently of AppWindow.
slint::include_modules!();

mod subpages;
pub use subpages::{install_subpages, prepare_subpages};
