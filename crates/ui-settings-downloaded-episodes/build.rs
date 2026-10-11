include!("../ui/build-support.rs");

fn main() {
    compile_page_library(
        "downloaded-episodes-module.slint",
        "NovaSettingsDownloadedEpisodes",
    );
}
