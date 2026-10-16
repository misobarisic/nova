//! Exercise the vendored painter through Nova's existing dependency graph,
//! without building upstream FemtoVG's unrelated example/test dependencies.
#![cfg(feature = "desktop")]
pub use femtovg::Color;

#[path = "../vendor/femtovg/src/text/colr.rs"]
mod colr;

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
fn variable_color_glyphs_match_independent_skia_references() {
    use skrifa::{FontRef, MetadataProvider};
    const FONT: &[u8] = include_bytes!("fixtures/emoji/test_glyphs-glyf_colr_1_variable.ttf");
    let font = FontRef::new(FONT).unwrap();
    let manifest: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/emoji/skia-references.json")).unwrap();
    for case in manifest["cases"].as_array().unwrap() {
        let file = case["file"].as_str().unwrap();
        let variations = case["variations"].as_object().unwrap();
        let location = font.axes().location(
            variations
                .iter()
                .map(|(tag, value)| (tag.as_str(), value.as_f64().unwrap() as f32)),
        );
        let coords: Vec<_> = location.coords().iter().map(|c| c.to_bits()).collect();
        let color = u32::from_str_radix(case["foreground_argb"].as_str().unwrap(), 16).unwrap();
        let bitmap = colr::rasterize(
            FONT,
            0,
            case["glyph"].as_u64().unwrap() as u16,
            100.0,
            0.0,
            &coords,
            Color::rgba(
                (color >> 16) as u8,
                (color >> 8) as u8,
                color as u8,
                (color >> 24) as u8,
            ),
        )
        .unwrap_or_else(|| panic!("failed to paint {file}"));
        let mut actual = image::RgbaImage::new(128, 128);
        for y in 0..bitmap.height {
            for x in 0..bitmap.width {
                let dx = 14 + bitmap.left + x as i32;
                let dy = 114 - bitmap.top + y as i32;
                if (0..128).contains(&dx) && (0..128).contains(&dy) {
                    let offset = ((y * bitmap.width + x) * 4) as usize;
                    actual.put_pixel(
                        dx as u32,
                        dy as u32,
                        image::Rgba(bitmap.pixels[offset..offset + 4].try_into().unwrap()),
                    );
                }
            }
        }
        let expected = image::open(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/emoji")
                .join(file),
        )
        .unwrap()
        .into_rgba8();
        let mut error = 0usize;
        let mut different_pixels = 0usize;
        for (a, b) in actual.pixels().zip(expected.pixels()) {
            let mut worst = 0;
            for channel in 0..4 {
                // Ignore invisible RGB and compare the actual compositing
                // values. Independent rasterizers can differ at AA edges.
                let component = |p: &image::Rgba<u8>| -> u8 {
                    if channel == 3 {
                        p[3]
                    } else {
                        ((u16::from(p[channel]) * u16::from(p[3]) + 127) / 255) as u8
                    }
                };
                let delta = component(a).abs_diff(component(b)) as usize;
                error += delta;
                worst = worst.max(delta);
            }
            different_pixels += usize::from(worst > 32);
        }
        let mean_error = error as f64 / (128.0 * 128.0 * 4.0);
        if mean_error >= 3.0 || different_pixels >= 164 {
            let output = std::env::temp_dir().join(format!("nova-actual-{file}"));
            actual.save(&output).unwrap();
            panic!(
                "{file}: mean channel error {mean_error:.3}, {different_pixels} pixels differ >32; actual {}",
                output.display()
            );
        }
    }
}
