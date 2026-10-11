// Settings visuals compile once here, independently of the main window.
slint::include_modules!();

mod submenus;
pub use submenus::{install_submenus, prepare_submenus};
