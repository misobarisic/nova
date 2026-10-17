//! Release identity is independent of optimization/debug profile.
pub const DISPLAY: &str = env!("NOVA_BUILD_VERSION");
pub const BASE: &str = env!("NOVA_BASE_VERSION");

pub fn is_release() -> bool {
    !cfg!(debug_assertions) && env!("NOVA_BUILD_RELEASE") == "true"
}

// The same resolver is compiled into build.rs, whose unit tests Cargo does not run.
#[cfg(test)]
#[allow(dead_code)]
#[path = "../build-support/version.rs"]
mod build_version;
