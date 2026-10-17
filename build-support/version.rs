//! Shared by the build script and app unit tests; no build-time network calls.
use std::process::Command;

// Keep dirty detection and Cargo invalidation scoped to the same build inputs.
pub const SOURCE_PATHS: &[&str] = &[
    "src",
    "crates",
    "android",
    "assets",
    "vendor",
    ".github",
    ".cargo",
    "scripts",
    "tests",
    "Makefile",
    "Cargo.toml",
    "Cargo.lock",
    "build.rs",
    "build-support",
    "flake.nix",
    "flake.lock",
];

#[derive(Debug, PartialEq, Eq)]
pub struct Version {
    pub display: String,
    pub base: String,
    pub release: bool,
}

pub fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

pub fn resolve(
    package: &str,
    tag: Option<&str>,
    commit: Option<&str>,
    exact: Option<&str>,
    dirty: bool,
) -> Version {
    let tag = tag.filter(|tag| {
        tag.strip_prefix('v')
            .is_some_and(|v| semver::Version::parse(v).is_ok())
    });
    let base = tag.and_then(|tag| tag.strip_prefix('v')).unwrap_or(package);
    let release = !dirty && exact == tag && tag.is_some() && base == package;
    let display = if release {
        format!("v{base}")
    } else {
        format!(
            "v{base}-{}{}",
            commit.unwrap_or("unknown"),
            if dirty && commit.is_some() {
                "-dirty"
            } else {
                ""
            }
        )
    };
    Version {
        display,
        base: base.into(),
        release,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn development_keeps_the_base_tag_and_adds_the_commit() {
        let info = resolve("0.0.29", Some("v0.0.29"), Some("abc12345"), None, false);
        assert_eq!(info.display, "v0.0.29-abc12345");
        assert_eq!(info.base, "0.0.29");
        assert!(!info.release);
        assert_eq!(
            resolve(
                "0.0.29",
                Some("v0.0.29"),
                Some("abc12345"),
                Some("v0.0.29"),
                true
            )
            .display,
            "v0.0.29-abc12345-dirty"
        );
    }

    #[test]
    fn only_clean_matching_tags_are_releases() {
        assert!(
            resolve(
                "0.0.29",
                Some("v0.0.29"),
                Some("abc12345"),
                Some("v0.0.29"),
                false
            )
            .release
        );
        assert!(
            !resolve(
                "0.1.0",
                Some("v0.0.29"),
                Some("abc12345"),
                Some("v0.0.29"),
                false
            )
            .release
        );
        assert!(!resolve("0.0.29", None, None, None, false).release);
        assert_eq!(
            resolve("0.0.29", Some("invalid"), None, None, false).display,
            "v0.0.29-unknown"
        );
    }
}
