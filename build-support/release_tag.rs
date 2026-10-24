//! Select an explicit dispatched release identity, retaining tag-push fallback.
pub fn select<'a>(
    package: &str,
    explicit: Option<&'a str>,
    ref_type: Option<&str>,
    ref_name: Option<&'a str>,
) -> Result<Option<&'a str>, String> {
    let tag = explicit
        .filter(|tag| !tag.is_empty())
        .or(if ref_type == Some("tag") {
            ref_name
        } else {
            None
        });
    if ref_type == Some("tag") && tag.is_none() {
        return Err("missing release tag".into());
    }
    if let Some(tag) = tag
        && tag != format!("v{package}")
    {
        return Err("release tag must match Cargo.toml".into());
    }
    Ok(tag)
}

#[cfg(test)]
mod tests {
    use super::select;

    #[test]
    fn dispatch_uses_the_explicit_tag_instead_of_main() {
        assert_eq!(
            select("1.2.3", Some("v1.2.3"), Some("branch"), Some("main")),
            Ok(Some("v1.2.3"))
        );
    }

    #[test]
    fn local_and_tag_push_behavior_is_retained() {
        assert_eq!(select("1.2.3", None, None, None), Ok(None));
        assert_eq!(
            select("1.2.3", None, Some("tag"), Some("v1.2.3")),
            Ok(Some("v1.2.3"))
        );
        assert!(select("1.2.3", None, Some("tag"), None).is_err());
    }

    #[test]
    fn mismatch_is_rejected() {
        assert!(select("1.2.3", Some("main"), Some("branch"), Some("main")).is_err());
        assert!(select("1.2.3", None, Some("tag"), Some("v1.2.4")).is_err());
    }
}
