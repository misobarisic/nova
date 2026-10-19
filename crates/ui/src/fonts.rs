//! Packaged emoji fallback, independent of the host's font inventory.
use i_slint_core::textlayout::sharedparley::{FontContext, parley::fontique::GenericFamily};

const EMOJI: &[u8] = include_bytes!("../../../assets/fonts/NotoColorEmoji.ttf");

/// Call on the UI thread before showing the window or publishing text models.
pub fn initialize_fonts(window: &slint::Window) {
    let context = i_slint_core::window::WindowInner::from_pub(window).context();
    add_emoji_fallback(&mut context.font_context().borrow_mut());
}

fn add_emoji_fallback(context: &mut FontContext) {
    context.register_static_font(EMOJI);
    let collection = &mut context.inner.collection;
    let emoji = collection
        .family_id("Noto Color Emoji")
        .expect("bundled emoji font has its expected family");
    // Registering an imported font only makes its name available. Slint's
    // text shaper queries Roboto followed by these generic families; append
    // emoji explicitly, retaining the host's other script fallback fonts.
    for generic in [GenericFamily::SansSerif, GenericFamily::SystemUi] {
        let mut families: Vec<_> = collection.generic_families(generic).collect();
        if !families.contains(&emoji) {
            // Fontique checks application overrides before system families.
            // Appending only emoji makes it the first generic font, so popup
            // labels take their spaces from its full-width space glyph.
            families.push(emoji);
            collection.set_generic_families(generic, families.into_iter());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use i_slint_core::textlayout::sharedparley::parley::{self, fontique};
    use std::{borrow::Cow, sync::Arc};

    #[test]
    fn generic_text_keeps_system_families_before_emoji() {
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
        assert!(
            after.starts_with(&before),
            "generic popup text must keep the system text fonts ahead of emoji: {before:?} -> {after:?}"
        );
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
        fonts.collection.register_fonts(
            fontique::Blob::new(Arc::new(include_bytes!(
                "../../../assets/fonts/Roboto-Regular.ttf"
            ))),
            None,
        );
        add_emoji_fallback(&mut fonts);
        add_emoji_fallback(&mut fonts);
        let families: Vec<_> = fonts
            .collection
            .generic_families(GenericFamily::SansSerif)
            .collect();
        assert_eq!(families.len(), 1, "repeated initialization is idempotent");
        let families = [
            parley::style::FontFamilyName::named("Roboto"),
            parley::style::FontFamilyName::Generic(GenericFamily::SansSerif),
            parley::style::FontFamilyName::Generic(GenericFamily::SystemUi),
        ];
        let emoji_family = [parley::style::FontFamilyName::named("Noto Color Emoji")];
        let mut layout_context = parley::LayoutContext::<()>::new();
        let mut shape = |text: &str, emoji_only: bool| {
            let families = if emoji_only {
                &emoji_family[..]
            } else {
                &families[..]
            };
            let mut builder = layout_context.ranged_builder(&mut fonts.inner, text, 1.0, true);
            builder.push_default(parley::StyleProperty::FontFamily(
                parley::style::FontFamily::List(Cow::Borrowed(families)),
            ));
            let mut layout = builder.build(text);
            layout.break_all_lines(None);
            let glyphs: Vec<_> = layout
                .lines()
                .flat_map(|line| line.items())
                .filter_map(|item| match item {
                    parley::PositionedLayoutItem::GlyphRun(run) => Some(run),
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
            // England's subdivision flag uses tag characters, not regional indicators.
            "\u{1f3f4}\u{e0067}\u{e0062}\u{e0065}\u{e006e}\u{e0067}\u{e007f}",
        ] {
            shape(text, false);
        }
        // Test the complete font repertoire with its family selected directly.
        // Roboto itself covers ASCII digits, so Parley's current Roboto-first
        // fallback splits keycaps across fonts with either the old bitmap or
        // the new vector face. That existing policy is separate from whether
        // the bundled face retains and shapes the full baseline repertoire.
        shape("1️⃣", true);
        let decode = |sequence: &str| -> String {
            sequence
                .split_whitespace()
                .map(|cp| char::from_u32(u32::from_str_radix(cp, 16).unwrap()).unwrap())
                .collect()
        };
        // The fixture comes from every GSUB ligature input in the old full
        // bitmap font, including its legacy reversed aliases. Keep those
        // aliases equivalent to the corresponding canonical color glyph.
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
            let glyph = shape(&text, true);
            sequences += 1;
            if !equivalent.is_empty() {
                assert_eq!(glyph, shape(&decode(equivalent), true), "alias {line}");
                aliases += 1;
            }
        }
        assert_eq!(sequences, 4176);
        assert_eq!(aliases, 1612);
    }
}
