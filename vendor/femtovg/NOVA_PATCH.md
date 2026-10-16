# Nova patches to FemtoVG 0.27.0

Source: <https://crates.io/crates/femtovg/0.27.0>, MIT OR Apache-2.0.
The original registry manifest is retained as `Cargo.toml.orig` (with the added
optional dependencies/feature documented alongside the normalized manifest).

Nova enables the optional `colrv1` feature only for desktop. It adds a private
Skrifa 0.44 / tiny-skia 0.12 `ColorPainter` in `src/text/colr.rs`. The existing
Slint/Parley glyph IDs and positions flow through the existing color-glyph
atlas, including its ephemeral atlas for transformed text. Other glyphs still
use Swash. Temporary atlases clone the existing rasterizer resource handle
rather than borrowing the already-borrowed text context during transformed
color-text drawing. No public API or desktop rendering backend changes are required.

The adapter resolves the default CPAL palette, foreground colors, gradients,
transforms, clips, and isolated composite layers. A bounds pass handles fonts
without precomputed COLRv1 clip boxes, including bounded composites with
unbounded intermediate fills. Gradient interpolation matches native Skia
color-font rendering in sRGB. Outlines and paint graphs use the same
variation coordinates. Raster allocations, graph operations, stack depth and
glyph dimensions are bounded. The output is demultiplied before uploading to
the existing straight-RGBA color atlas; the glyph cache includes foreground
color for COLRv1 glyphs.

The sibling tiny-skia patch fixes independent scale/bias normalization for
partial sweeps. CPU tests cover that regression and the painter, with independent Skia
reference images; the opt-in
`ui_emoji_render` integration test exercises the actual desktop Slint renderer.

Keep this patch isolated when updating Slint/FemtoVG. Upstream COLRv1 support
could replace it once it preserves these rendering and cache invariants.
