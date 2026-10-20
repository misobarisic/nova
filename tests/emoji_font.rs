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

#[test]
fn roboto_text_faces_leave_emoji_to_noto_and_retain_ordinary_coverage() {
    use skrifa::{FontRef, MetadataProvider};
    let emoji = FontRef::new(include_bytes!("../assets/fonts/NotoColorEmoji.ttf")).unwrap();
    let removed = [0x00a9u32, 0x00ae, 0x203c, 0x2049, 0x20e3, 0x2122];
    for bytes in [
        include_bytes!("../assets/fonts/Roboto-Regular.ttf").as_slice(),
        include_bytes!("../assets/fonts/Roboto-Bold.ttf").as_slice(),
    ] {
        let font = FontRef::new(bytes).unwrap();
        let charmap = font.charmap();
        assert_eq!(charmap.mappings().count(), 2766);
        for cp in removed {
            assert!(charmap.map(cp).is_none(), "Roboto still maps U+{cp:04X}");
            assert!(
                emoji.charmap().map(cp).is_some(),
                "Noto must replace U+{cp:04X}"
            );
        }
        for character in " 0123456789#*Hello ČĆĐŠŽ čćđšž Привет Ελληνικά".chars()
        {
            assert!(
                charmap
                    .map(character)
                    .is_some_and(|glyph| glyph.to_u32() != 0),
                "ordinary text lost {character:?}"
            );
        }
        // Formatting/joining coverage is deliberately retained too.
        for cp in [0u32, 13, 0x200d] {
            assert!(charmap.map(cp).is_some(), "formatting U+{cp:04X}");
        }
    }
}
