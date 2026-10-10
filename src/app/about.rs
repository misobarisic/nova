//! Standalone app information and license navigation.
use super::*;

impl Bridge {
    pub(super) fn show_about_page(&self) {
        if let Some(app) = self.app() {
            app.set_show_home(false);
            app.set_show_library(false);
            app.set_show_settings(false);
            app.set_show_about(true);
        }
    }
}
