include!("../build-support.rs");

fn main() {
    // ComponentContainer is separately gated from library imports in Slint
    // 1.18. Keep that gate local to this isolated build script.
    // SAFETY: this build script starts no threads before configuring Slint.
    unsafe { std::env::set_var("SLINT_ENABLE_EXPERIMENTAL_FEATURES", "1") };
    compile(None);
}
