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
        if !collection.generic_families(generic).any(|id| id == emoji) {
            collection.append_generic_families(generic, std::iter::once(emoji));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use i_slint_core::textlayout::sharedparley::parley::{self, fontique};
    use std::{borrow::Cow, sync::Arc};

    #[test]
    fn flags_and_joined_emoji_shape_without_system_fonts() {
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
        let mut layout_context = parley::LayoutContext::<()>::new();
        for text in ["🇬🇧", "🇩🇪", "🇫🇷", "🇪🇸", "🇭🇷", "👩‍💻", "🏳️‍🌈", "💾", "⚙️", "👤"]
        {
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
                    parley::PositionedLayoutItem::GlyphRun(run) => Some(run),
                    _ => None,
                })
                .flat_map(|run| run.glyphs().collect::<Vec<_>>())
                .collect();
            assert_eq!(glyphs.len(), 1, "{text} must shape as one emoji");
            assert_ne!(glyphs[0].id, 0, "{text} must not use a missing-glyph box");
        }
    }
}
