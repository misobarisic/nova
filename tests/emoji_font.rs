//! Coverage of Nova's bundled font; private painter tests live in the FemtoVG fork.
#![cfg(feature = "desktop")]

#[test]
fn bundled_font_preserves_every_baseline_codepoint() {
    use skrifa::{FontRef, MetadataProvider};
    let font = FontRef::new(include_bytes!("../assets/fonts/NotoColorEmoji.ttf")).unwrap();
    let charmap = font.charmap();
    let mut count = 0;
    for line in include_str!("../assets/fonts/emoji-coverage.txt").lines() {
        if line.starts_with('#') || line.split_whitespace().count() != 1 {
            continue;
        }
        let codepoint = u32::from_str_radix(line, 16).unwrap();
        assert!(
            charmap
                .map(codepoint)
                .is_some_and(|glyph| glyph.to_u32() != 0),
            "missing baseline codepoint U+{line}"
        );
        count += 1;
    }
    assert_eq!(count, 1510);
}
