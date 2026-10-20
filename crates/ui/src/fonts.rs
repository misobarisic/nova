//! Packaged emoji fallback, independent of the host's font inventory.
use i_slint_core::textlayout::sharedparley::{FontContext, parley::fontique::GenericFamily};

const EMOJI: &[u8] = include_bytes!("../../../assets/fonts/NotoColorEmoji.ttf");
const REGULAR: &[u8] = include_bytes!("../../../assets/fonts/Roboto-Regular.ttf");
const BOLD: &[u8] = include_bytes!("../../../assets/fonts/Roboto-Bold.ttf");

/// Call on the UI thread before showing the window or publishing text models.
pub fn initialize_fonts(window: &slint::Window) {
    let context = i_slint_core::window::WindowInner::from_pub(window).context();
    add_emoji_fallback(&mut context.font_context().borrow_mut());
}

fn add_emoji_fallback(context: &mut FontContext) {
    context.register_static_font(REGULAR);
    context.register_static_font(BOLD);
    context.register_static_font(EMOJI);
    let collection = &mut context.inner.collection;
    let text = collection.family_id("Roboto").expect("bundled text family");
    let emoji = collection
        .family_id("Noto Color Emoji")
        .expect("bundled emoji font has its expected family");
    // Parley selects the first face covering the whole grapheme. The bundled
    // text faces omit only six overlapping emoji mappings (including U+20E3),
    // so keycaps fall back as a whole while ordinary digits/#/* stay Roboto.
    // Put Noto ahead of host fonts that may also cover emoji, but keep Roboto
    // first for generic popup text: Noto's space has a full emoji-cell advance.
    for generic in [GenericFamily::SansSerif, GenericFamily::SystemUi] {
        let previous: Vec<_> = collection.generic_families(generic).collect();
        if previous.starts_with(&[text, emoji]) {
            continue;
        }
        let mut families = vec![text, emoji];
        for family in previous {
            if !families.contains(&family) {
                families.push(family);
            }
        }
        collection.set_generic_families(generic, families.into_iter());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use i_slint_core::textlayout::sharedparley::parley::{self, fontique};
    use std::borrow::Cow;

    #[test]
    fn generic_text_keeps_roboto_then_noto_and_other_system_families() {
        let mut fonts = FontContext::new(parley::FontContext::default());
        let before: Vec<_> = fonts
            .collection
            .generic_families(GenericFamily::SansSerif)
            .collect();
        add_emoji_fallback(&mut fonts);
        let after: Vec<_> = fonts
            .collection
            .generic_families(GenericFamily::SansSerif)
            .collect();
        let text = fonts.collection.family_id("Roboto").unwrap();
        let emoji = fonts.collection.family_id("Noto Color Emoji").unwrap();
        assert!(after.starts_with(&[text, emoji]));
        assert!(before.iter().all(|family| after.contains(family)));
        add_emoji_fallback(&mut fonts);
        assert_eq!(
            fonts
                .collection
                .generic_families(GenericFamily::SansSerif)
                .collect::<Vec<_>>(),
            after
        );
    }

    #[test]
    fn full_baseline_emoji_sequences_shape_without_system_fonts() {
        let mut fonts = FontContext::new(parley::FontContext {
            collection: fontique::Collection::new(fontique::CollectionOptions {
                system_fonts: false,
                ..Default::default()
            }),
            source_cache: Default::default(),
        });
        add_emoji_fallback(&mut fonts);
        add_emoji_fallback(&mut fonts);
        let families: Vec<_> = fonts
            .collection
            .generic_families(GenericFamily::SansSerif)
            .collect();
        assert_eq!(families.len(), 2, "repeated initialization is idempotent");
        let mut layout_context = parley::LayoutContext::<()>::new();
        let mut shape = |text: &str, requested_family: &str| {
            let families = [
                parley::style::FontFamilyName::named(requested_family),
                parley::style::FontFamilyName::Generic(GenericFamily::SansSerif),
                parley::style::FontFamilyName::Generic(GenericFamily::SystemUi),
            ];
            let mut builder = layout_context.ranged_builder(&mut fonts.inner, text, 1.0, true);
            builder.push_default(parley::StyleProperty::FontFamily(
                parley::style::FontFamily::List(Cow::Borrowed(&families)),
            ));
            let mut layout = builder.build(text);
            layout.break_all_lines(None);
            let glyphs: Vec<_> = layout
                .lines()
                .flat_map(|line| line.items())
                .filter_map(|item| match item {
                    parley::PositionedLayoutItem::GlyphRun(run) => {
                        let segment = &text[run.run().text_range()];
                        // Legacy reversed aliases can begin with a tag, ZWJ or
                        // variation selector outside the visible grapheme.
                        // Harfrust hides these default ignorables using a
                        // zero-advance space glyph, which may belong to Roboto.
                        // Check their invisibility, then require Noto for every
                        // run that contains any visible part of the emoji.
                        if segment.chars().all(|ch| {
                            matches!(ch, '\u{200d}' | '\u{fe0e}' | '\u{fe0f}' | '\u{e0020}'..='\u{e007f}')
                        }) {
                            assert_eq!(run.advance(), 0.0, "format-only segment {segment:?}");
                            assert!(
                                run.glyphs().all(|glyph| glyph.advance == 0.0),
                                "{segment:?} in {text:?} must remain invisible"
                            );
                            return None;
                        }
                        let source = run.run().font().data.as_ref();
                        assert!(
                            std::ptr::eq(source, EMOJI) || source == EMOJI,
                            "{segment:?} in {text:?} must select the packaged Noto face, not a monochrome glyph"
                        );
                        Some(run)
                    }
                    _ => None,
                })
                .flat_map(|run| run.glyphs().collect::<Vec<_>>())
                .collect();
            assert_eq!(glyphs.len(), 1, "{text:?} must shape as one emoji");
            assert_ne!(glyphs[0].id, 0, "{text:?} must not use a missing-glyph box");
            glyphs[0].id
        };
        for text in [
            "🇬🇧",
            "🇩🇪",
            "🇫🇷",
            "🇪🇸",
            "🇭🇷",
            "👩‍💻",
            "🏳️‍🌈",
            "💾",
            "⚙️",
            "👤",
            "❤️",
            "👨🏿‍🚀",
            "©",
            "©️",
            "®",
            "®️",
            "‼",
            "‼️",
            "⁉",
            "⁉️",
            "™",
            "™️",
            "#️⃣",
            "*️⃣",
            "0️⃣",
            "1️⃣",
            "9️⃣",
            "1\u{20e3}",
            "🎬",
            "🎥",
            "🎞️",
            "🎧",
            "🔈",
            "🔉",
            "🔊",
            "🌍",
            "🌎",
            "🌏",
            "🌐",
            "📁",
            "📂",
            "💾",
            // England's subdivision flag uses tag characters, not regional indicators.
            "\u{1f3f4}\u{e0067}\u{e0062}\u{e0065}\u{e006e}\u{e0067}\u{e007f}",
        ] {
            shape(text, "Roboto");
        }
        let decode = |sequence: &str| -> String {
            sequence
                .split_whitespace()
                .map(|cp| char::from_u32(u32::from_str_radix(cp, 16).unwrap()).unwrap())
                .collect()
        };
        // The fixture comes from every GSUB ligature input in the old full
        // bitmap font, including its legacy reversed aliases. Every input must
        // still produce one visible Noto glyph with production Roboto fallback.
        // Check alias glyph identity separately with explicit Noto: malformed
        // aliases such as CANCEL TAG + black flag cross grapheme boundaries,
        // so production fallback can place the leading invisible tag in Roboto
        // and shape the visible flag independently of the font's GSUB alias.
        // The generator separately checks all baseline cmap entries, including
        // controls and default ignorables that do not produce layout glyphs.
        let mut sequences = 0;
        let mut aliases = 0;
        for line in include_str!("../../../assets/fonts/emoji-coverage.txt").lines() {
            if line.starts_with('#') {
                continue;
            }
            let (input, equivalent) = line.split_once(';').unwrap_or((line, ""));
            let text = decode(input);
            if text.chars().count() < 2 {
                continue;
            }
            shape(&text, "Roboto");
            sequences += 1;
            if !equivalent.is_empty() {
                assert_eq!(
                    shape(&text, "Noto Color Emoji"),
                    shape(&decode(equivalent), "Noto Color Emoji"),
                    "font-level alias {line}"
                );
                aliases += 1;
            }
        }
        assert_eq!(sequences, 4176);
        assert_eq!(aliases, 1612);
    }

    #[test]
    fn mixed_text_keeps_roboto_metrics_weights_and_whole_noto_graphemes() {
        let mut fonts = FontContext::new(parley::FontContext {
            collection: fontique::Collection::new(fontique::CollectionOptions {
                system_fonts: false,
                ..Default::default()
            }),
            source_cache: Default::default(),
        });
        add_emoji_fallback(&mut fonts);
        let generic = [
            parley::style::FontFamilyName::Generic(GenericFamily::SansSerif),
            parley::style::FontFamilyName::Generic(GenericFamily::SystemUi),
        ];
        let production = [
            parley::style::FontFamilyName::named("Roboto"),
            generic[0].clone(),
            generic[1].clone(),
        ];
        let mut layout_context = parley::LayoutContext::<()>::new();
        for bold in [false, true] {
            for families in [&production[..], &generic[..]] {
                for text in [
                    " 0123456789 # * + - . ! ? / ",
                    "Čarolija, Život, čćđšž — Привет — Ελληνικά",
                    "Film 123 # * 🎬 ©️ ‼️ 1️⃣ 🇭🇷 👩🏽‍💻",
                ] {
                    let mut builder =
                        layout_context.ranged_builder(&mut fonts.inner, text, 1.0, true);
                    builder.push_default(parley::StyleProperty::FontFamily(
                        parley::style::FontFamily::List(Cow::Borrowed(families)),
                    ));
                    if bold {
                        builder.push_default(parley::StyleProperty::FontWeight(
                            parley::style::FontWeight::BOLD,
                        ));
                    }
                    let mut layout = builder.build(text);
                    layout.break_all_lines(None);
                    let mut sources = Vec::new();
                    for item in layout.lines().flat_map(|line| line.items()) {
                        let parley::PositionedLayoutItem::GlyphRun(run) = item else {
                            continue;
                        };
                        let segment = &text[run.run().text_range()];
                        let is_emoji = text.starts_with("Film") && !segment.is_ascii();
                        let expected = if is_emoji {
                            EMOJI
                        } else if bold {
                            BOLD
                        } else {
                            REGULAR
                        };
                        let source = run.run().font().data.as_ref();
                        assert!(
                            std::ptr::eq(source, expected) || source == expected,
                            "wrong source for {segment:?}, bold={bold}"
                        );
                        assert!(run.glyphs().all(|glyph| glyph.id != 0), "{segment:?}");
                        sources.push(is_emoji);
                    }
                    assert!(sources.contains(&false));
                    if text.starts_with("Film") {
                        assert!(sources.contains(&true));
                    }
                }
            }
        }
    }
}
