//! COLRv1 rasterization for the existing glyph atlas. Layout stays with the
//! caller; Skrifa interprets the paint graph and tiny-skia paints one glyph.
use skrifa::{
    FontRef, GlyphId, MetadataProvider,
    color::{Brush, ColorGlyphFormat, ColorPainter, ColorStop, CompositeMode, Extend},
    instance::{LocationRef, NormalizedCoord, Size},
    outline::{DrawSettings, OutlinePen},
    raw::types::BoundingBox,
};
use tiny_skia::{
    BlendMode, Color, FillRule, GradientStop, Mask, Paint, Path, PathBuilder, Pixmap, PixmapPaint,
    Point, Rect, Shader, SpreadMode, Transform,
};

// The atlas is 512px square, with two pixels of padding on each side. Bound
// both graph work and simultaneous clip/layer storage, including bad fonts.
const MAX_SIDE: u32 = 508;
const MAX_DEPTH: usize = 64;
const MAX_OPERATIONS: usize = 32_768;
const MAX_STORAGE: usize = 32 * 1024 * 1024;

pub(super) struct Bitmap {
    pub width: u32,
    pub height: u32,
    pub left: i32,
    pub top: i32,
    /// Straight RGBA, matching the existing (non-premultiplied) atlas texture.
    pub pixels: Vec<u8>,
}

pub(super) fn has_glyph(data: &[u8], index: u32, glyph: u16) -> bool {
    FontRef::from_index(data, index).ok().is_some_and(|font| {
        font.color_glyphs()
            .get_with_format(GlyphId::new(glyph as u32), ColorGlyphFormat::ColrV1)
            .is_some()
    })
}

pub(super) fn rasterize(
    data: &[u8],
    index: u32,
    glyph: u16,
    size: f32,
    subpixel_x: f32,
    coords: &[i16],
    foreground: crate::Color,
) -> Option<Bitmap> {
    if !size.is_finite() || size <= 0.0 || !subpixel_x.is_finite() || coords.len() > MAX_DEPTH {
        return None;
    }
    let font = FontRef::from_index(data, index).ok()?;
    let glyph = font
        .color_glyphs()
        .get_with_format(GlyphId::new(glyph as u32), ColorGlyphFormat::ColrV1)?;
    let coords: Vec<_> = coords
        .iter()
        .copied()
        .map(NormalizedCoord::from_bits)
        .collect();
    let location = LocationRef::new(&coords);
    let upem = font.metrics(Size::unscaled(), location).units_per_em;
    if upem == 0 {
        return None;
    }
    let scale = size / f32::from(upem);
    let transform = Transform::from_row(scale, 0.0, 0.0, -scale, subpixel_x, 0.0);

    // Do not assume a v1 ClipBox exists. A bounds-only traversal also covers
    // glyphs whose only geometry comes from transformed component outlines.
    let mut measure = Measure::new(font.clone(), &coords, transform);
    glyph.paint(location, &mut measure).ok()?;
    let bounds = measure.bounds()?;
    let left = bounds.left().floor();
    let top = bounds.top().floor();
    let width = bounds.right().ceil() - left;
    let height = bounds.bottom().ceil() - top;
    if width <= 0.0 || height <= 0.0 || width > MAX_SIDE as f32 || height > MAX_SIDE as f32 {
        return None;
    }
    // Also keep conversions and translation exact for pathological metrics.
    if left.abs() > 1_000_000.0 || top.abs() > 1_000_000.0 {
        return None;
    }
    let pixmap = Pixmap::new(width as u32, height as u32)?;
    let transform = transform.post_translate(-left, -top);
    let mut painter = Painter::new(font, &coords, transform, foreground, pixmap);
    glyph.paint(location, &mut painter).ok()?;
    if painter.failed
        || painter.layers.len() != 1
        || painter.clips.len() != 1
        || painter.transforms.len() != 1
    {
        return None;
    }
    Some(Bitmap {
        width: width as u32,
        height: height as u32,
        left: left as i32,
        top: -(top as i32),
        pixels: painter.layers.pop()?.pixmap.take_demultiplied(),
    })
}

struct Pen(PathBuilder);
impl OutlinePen for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.move_to(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to(x, y);
    }
    fn quad_to(&mut self, x0: f32, y0: f32, x1: f32, y1: f32) {
        self.0.quad_to(x0, y0, x1, y1);
    }
    fn curve_to(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, x2: f32, y2: f32) {
        self.0.cubic_to(x0, y0, x1, y1, x2, y2);
    }
    fn close(&mut self) {
        self.0.close();
    }
}

fn outline(font: &FontRef<'_>, coords: &[NormalizedCoord], glyph: GlyphId) -> Option<Path> {
    let glyph = font.outline_glyphs().get(glyph)?;
    let mut pen = Pen(PathBuilder::new());
    glyph
        .draw(DrawSettings::unhinted(Size::unscaled(), coords), &mut pen)
        .ok()?;
    pen.0.finish()
}

fn convert(t: skrifa::color::Transform) -> Transform {
    Transform::from_row(t.xx, t.yx, t.xy, t.yy, t.dx, t.dy)
}

fn rect(b: BoundingBox<f32>) -> Option<Rect> {
    Rect::from_ltrb(b.x_min, b.y_min, b.x_max, b.y_max)
}

#[derive(Clone, Copy)]
enum Coverage {
    Unbounded,
    Empty,
    Bounded(Rect),
}
impl Coverage {
    fn intersect(self, other: Option<Rect>) -> Self {
        self.intersection(other.map_or(Self::Empty, Self::Bounded))
    }
    fn intersection(self, other: Self) -> Self {
        match (self, other) {
            (Self::Unbounded, r) | (r, Self::Unbounded) => r,
            (Self::Bounded(a), Self::Bounded(b)) => {
                a.intersect(&b).map_or(Self::Empty, Self::Bounded)
            }
            _ => Self::Empty,
        }
    }
    fn union(self, other: Self) -> Self {
        match (self, other) {
            (Self::Unbounded, _) | (_, Self::Unbounded) => Self::Unbounded,
            (Self::Empty, r) | (r, Self::Empty) => r,
            (Self::Bounded(a), Self::Bounded(b)) => Self::Bounded(
                Rect::from_ltrb(
                    a.left().min(b.left()),
                    a.top().min(b.top()),
                    a.right().max(b.right()),
                    a.bottom().max(b.bottom()),
                )
                .unwrap(),
            ),
        }
    }
}

struct Measure<'a> {
    font: FontRef<'a>,
    coords: &'a [NormalizedCoord],
    transforms: Vec<Transform>,
    clips: Vec<Coverage>,
    layers: Vec<(Coverage, CompositeMode)>,
    operations: usize,
    failed: bool,
}
impl<'a> Measure<'a> {
    fn new(font: FontRef<'a>, coords: &'a [NormalizedCoord], transform: Transform) -> Self {
        Self {
            font,
            coords,
            transforms: vec![transform],
            clips: vec![Coverage::Unbounded],
            layers: vec![(Coverage::Empty, CompositeMode::SrcOver)],
            operations: 0,
            failed: false,
        }
    }
    fn step(&mut self) -> bool {
        self.operations += 1;
        self.failed |= self.operations > MAX_OPERATIONS;
        !self.failed
    }
    fn bounds(&self) -> Option<Rect> {
        if self.failed
            || self.layers.len() != 1
            || self.clips.len() != 1
            || self.transforms.len() != 1
        {
            return None;
        }
        match self.layers[0].0 {
            Coverage::Bounded(r) => Some(r),
            _ => None,
        }
    }
    fn clip(&mut self, bounds: Option<Rect>) {
        if self.step() && self.clips.len() < MAX_DEPTH {
            let parent = *self.clips.last().unwrap();
            self.clips.push(parent.intersect(bounds));
        } else {
            self.failed = true;
        }
    }
}
impl ColorPainter for Measure<'_> {
    fn push_transform(&mut self, t: skrifa::color::Transform) {
        if self.step() && self.transforms.len() < MAX_DEPTH {
            self.transforms
                .push(self.transforms.last().unwrap().pre_concat(convert(t)));
        } else {
            self.failed = true;
        }
    }
    fn pop_transform(&mut self) {
        if self.transforms.len() > 1 {
            self.transforms.pop();
        } else {
            self.failed = true;
        }
    }
    fn push_clip_glyph(&mut self, glyph: GlyphId) {
        let bounds = outline(&self.font, self.coords, glyph)
            .and_then(|p| p.bounds().transform(*self.transforms.last().unwrap()));
        self.clip(bounds);
    }
    fn push_clip_box(&mut self, b: BoundingBox<f32>) {
        self.clip(rect(b).and_then(|r| r.transform(*self.transforms.last().unwrap())));
    }
    fn pop_clip(&mut self) {
        if self.clips.len() > 1 {
            self.clips.pop();
        } else {
            self.failed = true;
        }
    }
    fn fill(&mut self, _: Brush<'_>) {
        if !self.step() {
            return;
        }
        let layer = &mut self.layers.last_mut().unwrap().0;
        *layer = layer.union(*self.clips.last().unwrap());
    }
    fn push_layer(&mut self, mode: CompositeMode) {
        if self.step() && self.layers.len() < MAX_DEPTH && mode != CompositeMode::Unknown {
            self.layers.push((Coverage::Empty, mode));
        } else {
            self.failed = true;
        }
    }
    fn pop_layer(&mut self) {
        if self.layers.len() <= 1 {
            self.failed = true;
            return;
        }
        let (source, mode) = self.layers.pop().unwrap();
        let dest = &mut self.layers.last_mut().unwrap().0;
        // Intermediate paints may be unbounded while the final composite is
        // bounded (for example, an infinite solid source inside SRC_IN).
        *dest = match mode {
            CompositeMode::Clear => Coverage::Empty,
            CompositeMode::Src | CompositeMode::SrcOut | CompositeMode::DestAtop => source,
            CompositeMode::Dest | CompositeMode::DestOut | CompositeMode::SrcAtop => *dest,
            CompositeMode::SrcIn | CompositeMode::DestIn => dest.intersection(source),
            _ => dest.union(source),
        };
    }
}

#[derive(Clone, Copy)]
struct Stop {
    position: f32,
    color: Color,
}
fn gradient_stops(stops: Vec<Stop>) -> Vec<GradientStop> {
    stops
        .into_iter()
        .map(|s| GradientStop::new(s.position, s.color))
        .collect()
}

struct Layer {
    pixmap: Pixmap,
    mode: BlendMode,
}
struct Painter<'a> {
    font: FontRef<'a>,
    coords: &'a [NormalizedCoord],
    palette: Vec<Color>,
    foreground: Color,
    transforms: Vec<Transform>,
    clips: Vec<Option<Mask>>,
    layers: Vec<Layer>,
    width: u32,
    height: u32,
    storage: usize,
    operations: usize,
    failed: bool,
}
impl<'a> Painter<'a> {
    fn new(
        font: FontRef<'a>,
        coords: &'a [NormalizedCoord],
        transform: Transform,
        foreground: crate::Color,
        pixmap: Pixmap,
    ) -> Self {
        let palette = font.color_palettes().get(0).map_or_else(Vec::new, |p| {
            p.colors()
                .iter()
                .map(|c| Color::from_rgba8(c.red, c.green, c.blue, c.alpha))
                .collect()
        });
        let width = pixmap.width();
        let height = pixmap.height();
        Self {
            font,
            coords,
            palette,
            foreground: Color::from_rgba(foreground.r, foreground.g, foreground.b, foreground.a)
                .unwrap_or(Color::BLACK),
            transforms: vec![transform],
            clips: vec![None],
            layers: vec![Layer {
                pixmap,
                mode: BlendMode::SourceOver,
            }],
            width,
            height,
            storage: width as usize * height as usize * 4,
            operations: 0,
            failed: false,
        }
    }
    fn step(&mut self) -> bool {
        self.operations += 1;
        self.failed |= self.operations > MAX_OPERATIONS;
        !self.failed
    }
    fn reserve(&mut self, bytes: usize) -> bool {
        if self.storage + bytes > MAX_STORAGE {
            self.failed = true;
            return false;
        }
        self.storage += bytes;
        true
    }
    fn color(&self, index: u16, alpha: f32) -> Color {
        let mut color = if index == 0xffff {
            self.foreground
        } else {
            self.palette
                .get(index as usize)
                .copied()
                .unwrap_or(Color::TRANSPARENT)
        };
        color.set_alpha((color.alpha() * alpha).clamp(0.0, 1.0));
        color
    }
    fn stops(&self, stops: &[ColorStop]) -> Vec<Stop> {
        stops
            .iter()
            .map(|s| Stop {
                position: s.offset,
                color: self.color(s.palette_index, s.alpha),
            })
            .collect()
    }
    fn clip(&mut self, path: Option<Path>) {
        let bytes = self.width as usize * self.height as usize;
        if !self.step() || self.clips.len() >= MAX_DEPTH || !self.reserve(bytes) {
            self.failed = true;
            return;
        }
        let Some(mut mask) = self.clips.last().and_then(|m| m.clone()).or_else(|| {
            let mut mask = Mask::new(self.width, self.height)?;
            mask.data_mut().fill(255);
            Some(mask)
        }) else {
            self.failed = true;
            return;
        };
        if let Some(path) = path {
            mask.intersect_path(
                &path,
                FillRule::Winding,
                true,
                *self.transforms.last().unwrap(),
            );
        } else {
            mask.data_mut().fill(0);
        }
        self.clips.push(Some(mask));
    }
    fn shader(&self, brush: Brush<'_>) -> Option<Shader<'static>> {
        let transform = *self.transforms.last().unwrap();
        match brush {
            Brush::Solid {
                palette_index,
                alpha,
            } => Some(Shader::SolidColor(self.color(palette_index, alpha))),
            Brush::LinearGradient {
                p0,
                p1,
                color_stops,
                extend,
            } => tiny_skia::LinearGradient::new(
                Point::from_xy(p0.x, p0.y),
                Point::from_xy(p1.x, p1.y),
                gradient_stops(self.stops(color_stops)),
                spread(extend),
                transform,
            ),
            Brush::RadialGradient {
                c0,
                r0,
                c1,
                r1,
                color_stops,
                extend,
            } => radial(
                Point::from_xy(c0.x, c0.y),
                r0,
                Point::from_xy(c1.x, c1.y),
                r1,
                self.stops(color_stops),
                spread(extend),
                transform,
            ),
            Brush::SweepGradient {
                c0,
                start_angle,
                end_angle,
                color_stops,
                extend,
            } => tiny_skia::SweepGradient::new(
                Point::from_xy(c0.x, c0.y),
                start_angle,
                end_angle,
                gradient_stops(self.stops(color_stops)),
                spread(extend),
                // Skrifa already converts COLR's counterclockwise angles to
                // clockwise. tiny-skia's atan2 follows positive local Y; reflect
                // only the shader about its center, since glyph geometry still
                // uses the font's Y-up coordinate system.
                transform.pre_concat(Transform::from_row(1.0, 0.0, 0.0, -1.0, 0.0, 2.0 * c0.y)),
            ),
        }
    }
}
impl ColorPainter for Painter<'_> {
    fn push_transform(&mut self, t: skrifa::color::Transform) {
        if self.step() && self.transforms.len() < MAX_DEPTH {
            self.transforms
                .push(self.transforms.last().unwrap().pre_concat(convert(t)));
        } else {
            self.failed = true;
        }
    }
    fn pop_transform(&mut self) {
        if self.transforms.len() > 1 {
            self.transforms.pop();
        } else {
            self.failed = true;
        }
    }
    fn push_clip_glyph(&mut self, glyph: GlyphId) {
        self.clip(outline(&self.font, self.coords, glyph));
    }
    fn push_clip_box(&mut self, b: BoundingBox<f32>) {
        self.clip(rect(b).map(PathBuilder::from_rect));
    }
    fn pop_clip(&mut self) {
        if self.clips.len() > 1 {
            self.clips.pop();
            self.storage -= self.width as usize * self.height as usize;
        } else {
            self.failed = true;
        }
    }
    fn fill(&mut self, brush: Brush<'_>) {
        if !self.step() {
            return;
        }
        if let Some(shader) = self.shader(brush) {
            // The shader maps font coordinates into the device. Fill the whole
            // device clip, without transforming its rectangle a second time.
            let paint = Paint {
                shader,
                anti_alias: true,
                ..Paint::default()
            };
            let bounds = Rect::from_xywh(0.0, 0.0, self.width as f32, self.height as f32).unwrap();
            self.layers.last_mut().unwrap().pixmap.fill_rect(
                bounds,
                &paint,
                Transform::identity(),
                self.clips.last().unwrap().as_ref(),
            );
        }
    }
    fn push_layer(&mut self, mode: CompositeMode) {
        let bytes = self.width as usize * self.height as usize * 4;
        if !self.step() || self.layers.len() >= MAX_DEPTH || !self.reserve(bytes) {
            self.failed = true;
            return;
        }
        if let Some(pixmap) = Pixmap::new(self.width, self.height) {
            self.layers.push(Layer {
                pixmap,
                mode: blend(mode),
            });
        } else {
            self.failed = true;
        }
    }
    fn pop_layer(&mut self) {
        if self.layers.len() <= 1 {
            self.failed = true;
            return;
        }
        let layer = self.layers.pop().unwrap();
        let paint = PixmapPaint {
            blend_mode: layer.mode,
            ..PixmapPaint::default()
        };
        // Clip masks already affected each source fill. Applying them again
        // while merging would square antialiased edge coverage.
        self.layers.last_mut().unwrap().pixmap.draw_pixmap(
            0,
            0,
            layer.pixmap.as_ref(),
            &paint,
            Transform::identity(),
            None,
        );
        self.storage -= self.width as usize * self.height as usize * 4;
    }
}

fn spread(extend: Extend) -> SpreadMode {
    match extend {
        Extend::Repeat => SpreadMode::Repeat,
        Extend::Reflect => SpreadMode::Reflect,
        _ => SpreadMode::Pad,
    }
}

// Stop normalization can place one radius below zero. Truncate at radius=0,
// interpolating both center and color, rather than dropping the gradient.
#[allow(clippy::too_many_arguments)]
fn radial(
    mut c0: Point,
    mut r0: f32,
    mut c1: Point,
    mut r1: f32,
    mut stops: Vec<Stop>,
    extend: SpreadMode,
    transform: Transform,
) -> Option<Shader<'static>> {
    if r0 < 0.0 || r1 < 0.0 {
        if r1 < r0 {
            std::mem::swap(&mut c0, &mut c1);
            std::mem::swap(&mut r0, &mut r1);
            stops.reverse();
            for s in &mut stops {
                s.position = 1.0 - s.position;
            }
        }
        if r0 == r1 {
            return None;
        }
        if extend != SpreadMode::Pad {
            // Move the defining circles by whole color-line periods. Merely
            // truncating/repeating the shortened stops would change the repeat
            // period. Reflect repeats after two periods, not one.
            let period = if extend == SpreadMode::Reflect {
                2.0
            } else {
                1.0
            };
            let delta = r1 - r0;
            let shift = (-r0 / delta / period).ceil() * period;
            let dx = (c1.x - c0.x) * shift;
            let dy = (c1.y - c0.y) * shift;
            c0.x += dx;
            c0.y += dy;
            c1.x += dx;
            c1.y += dy;
            r0 = (r0 + delta * shift).max(0.0);
            r1 += delta * shift;
            return tiny_skia::RadialGradient::new(
                c0,
                r0,
                c1,
                r1,
                gradient_stops(stops),
                extend,
                transform,
            );
        }
        let t = -r0 / (r1 - r0);
        let at_zero = interpolate(&stops, t);
        let delta = Point::from_xy(c1.x - c0.x, c1.y - c0.y);
        if r1 <= 0.0 {
            // The visible cone starts beyond every stop. Pad the final color,
            // retaining the cone geometry (a solid shader would also paint
            // points outside a non-concentric cone).
            let radius_delta = r1 - r0;
            c0 = Point::from_xy(c0.x + t * delta.x, c0.y + t * delta.y);
            c1 = Point::from_xy(c0.x + delta.x, c0.y + delta.y);
            return tiny_skia::RadialGradient::new(
                c0,
                0.0,
                c1,
                radius_delta,
                vec![
                    GradientStop::new(0.0, at_zero),
                    GradientStop::new(1.0, at_zero),
                ],
                extend,
                transform,
            );
        }
        c0 = Point::from_xy(c0.x + t * (c1.x - c0.x), c0.y + t * (c1.y - c0.y));
        r0 = 0.0;
        stops.retain(|s| s.position > t);
        for s in &mut stops {
            s.position = (s.position - t) / (1.0 - t);
        }
        stops.insert(
            0,
            Stop {
                position: 0.0,
                color: at_zero,
            },
        );
    }
    tiny_skia::RadialGradient::new(c0, r0, c1, r1, gradient_stops(stops), extend, transform)
}

fn interpolate(stops: &[Stop], t: f32) -> Color {
    let Some(first) = stops.first() else {
        return Color::TRANSPARENT;
    };
    if t <= first.position {
        return first.color;
    }
    for pair in stops.windows(2) {
        if t < pair[1].position {
            let u = (t - pair[0].position) / (pair[1].position - pair[0].position);
            let a = pair[0].color;
            let b = pair[1].color;
            // Match tiny-skia's normal gradients and Skia's native color-font
            // rendering: interpolate straight sRGB, then composite premultiplied
            // pixels. The independent alpha-gradient reference locks this down.
            return Color::from_rgba(
                a.red() + u * (b.red() - a.red()),
                a.green() + u * (b.green() - a.green()),
                a.blue() + u * (b.blue() - a.blue()),
                a.alpha() + u * (b.alpha() - a.alpha()),
            )
            .unwrap_or(Color::TRANSPARENT);
        }
    }
    stops.last().unwrap().color
}

fn blend(mode: CompositeMode) -> BlendMode {
    match mode {
        CompositeMode::Clear => BlendMode::Clear,
        CompositeMode::Src => BlendMode::Source,
        CompositeMode::Dest => BlendMode::Destination,
        CompositeMode::SrcOver => BlendMode::SourceOver,
        CompositeMode::DestOver => BlendMode::DestinationOver,
        CompositeMode::SrcIn => BlendMode::SourceIn,
        CompositeMode::DestIn => BlendMode::DestinationIn,
        CompositeMode::SrcOut => BlendMode::SourceOut,
        CompositeMode::DestOut => BlendMode::DestinationOut,
        CompositeMode::SrcAtop => BlendMode::SourceAtop,
        CompositeMode::DestAtop => BlendMode::DestinationAtop,
        CompositeMode::Xor => BlendMode::Xor,
        CompositeMode::Plus => BlendMode::Plus,
        CompositeMode::Screen => BlendMode::Screen,
        CompositeMode::Overlay => BlendMode::Overlay,
        CompositeMode::Darken => BlendMode::Darken,
        CompositeMode::Lighten => BlendMode::Lighten,
        CompositeMode::ColorDodge => BlendMode::ColorDodge,
        CompositeMode::ColorBurn => BlendMode::ColorBurn,
        CompositeMode::HardLight => BlendMode::HardLight,
        CompositeMode::SoftLight => BlendMode::SoftLight,
        CompositeMode::Difference => BlendMode::Difference,
        CompositeMode::Exclusion => BlendMode::Exclusion,
        CompositeMode::Multiply => BlendMode::Multiply,
        CompositeMode::HslHue => BlendMode::Hue,
        CompositeMode::HslSaturation => BlendMode::Saturation,
        CompositeMode::HslColor => BlendMode::Color,
        CompositeMode::HslLuminosity => BlendMode::Luminosity,
        _ => BlendMode::SourceOver,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const FONT: &[u8] = include_bytes!("../../../../assets/fonts/NotoColorEmoji.ttf");
    fn painter(transform: Transform) -> Painter<'static> {
        let mut p = Painter::new(
            FontRef::new(FONT).unwrap(),
            &[],
            transform,
            crate::Color::rgba(0, 255, 0, 128),
            Pixmap::new(24, 24).unwrap(),
        );
        p.palette = vec![
            Color::from_rgba8(255, 0, 0, 255),
            Color::from_rgba8(0, 0, 255, 255),
        ];
        p
    }
    fn box_at(x0: f32, y0: f32, x1: f32, y1: f32) -> BoundingBox<f32> {
        BoundingBox {
            x_min: x0,
            y_min: y0,
            x_max: x1,
            y_max: y1,
        }
    }
    fn solid(index: u16) -> Brush<'static> {
        Brush::Solid {
            palette_index: index,
            alpha: 1.0,
        }
    }
    fn pixel(p: &Painter<'_>, x: u32, y: u32) -> tiny_skia::ColorU8 {
        p.layers[0].pixmap.pixel(x, y).unwrap().demultiply()
    }

    #[test]
    fn nested_clips_intersect_and_restore() {
        let mut p = painter(Transform::identity());
        p.push_clip_box(box_at(4.0, 4.0, 12.0, 12.0));
        p.push_clip_box(box_at(8.0, 0.0, 16.0, 16.0));
        p.fill(solid(0));
        assert_eq!(pixel(&p, 6, 6).alpha(), 0);
        assert_eq!(pixel(&p, 10, 6).red(), 255);
        assert_eq!(pixel(&p, 14, 6).alpha(), 0);
        p.pop_clip();
        p.fill(solid(1));
        assert_eq!(pixel(&p, 6, 6).blue(), 255);
    }

    #[test]
    fn transform_applies_once_to_clip_and_shader() {
        let mut p = painter(Transform::from_row(2.0, 0.0, 0.0, 2.0, 4.0, 4.0));
        p.push_clip_box(box_at(0.0, 0.0, 4.0, 4.0));
        let stops = [
            ColorStop {
                offset: 0.0,
                palette_index: 0,
                alpha: 1.0,
            },
            ColorStop {
                offset: 1.0,
                palette_index: 1,
                alpha: 1.0,
            },
        ];
        p.fill(Brush::LinearGradient {
            p0: skrifa::raw::types::Point::new(0.0, 0.0),
            p1: skrifa::raw::types::Point::new(4.0, 0.0),
            color_stops: &stops,
            extend: Extend::Pad,
        });
        assert_eq!(pixel(&p, 3, 6).alpha(), 0);
        assert_eq!(pixel(&p, 12, 6).alpha(), 0);
        assert!(pixel(&p, 5, 6).red() > pixel(&p, 5, 6).blue());
        assert!(pixel(&p, 10, 6).blue() > pixel(&p, 10, 6).red());
    }

    #[test]
    fn isolated_source_in_composites_with_backdrop() {
        let mut p = painter(Transform::identity());
        p.push_layer(CompositeMode::SrcOver);
        p.push_clip_box(box_at(2.0, 2.0, 14.0, 14.0));
        p.fill(solid(0));
        p.pop_clip();
        p.push_layer(CompositeMode::SrcIn);
        p.push_clip_box(box_at(8.0, 8.0, 20.0, 20.0));
        p.fill(solid(1));
        p.pop_clip();
        p.pop_layer();
        p.pop_layer();
        assert_eq!(pixel(&p, 4, 4).alpha(), 0);
        assert_eq!(pixel(&p, 10, 10).blue(), 255);
        assert_eq!(pixel(&p, 18, 18).alpha(), 0);
    }

    #[test]
    fn unbounded_intermediate_source_in_has_bounded_final_coverage() {
        let mut measure = Measure::new(FontRef::new(FONT).unwrap(), &[], Transform::identity());
        measure.push_layer(CompositeMode::SrcOver);
        measure.push_clip_box(box_at(2.0, 2.0, 14.0, 14.0));
        measure.fill(solid(0));
        measure.pop_clip();
        measure.push_layer(CompositeMode::SrcIn);
        measure.fill(solid(1));
        measure.pop_layer();
        measure.pop_layer();
        assert_eq!(measure.bounds(), Rect::from_ltrb(2.0, 2.0, 14.0, 14.0));
        assert!(!measure.failed);
    }

    #[test]
    fn foreground_color_and_alpha_are_resolved() {
        let mut p = painter(Transform::identity());
        p.push_clip_box(box_at(0.0, 0.0, 24.0, 24.0));
        p.fill(Brush::Solid {
            palette_index: 0xffff,
            alpha: 0.5,
        });
        let c = pixel(&p, 12, 12);
        assert_eq!(c.green(), 255);
        assert!((63..=65).contains(&c.alpha()));
    }

    #[test]
    fn negative_radial_radius_is_truncated_with_interpolated_color() {
        let stops = vec![
            Stop {
                position: 0.0,
                color: Color::from_rgba8(255, 0, 0, 255),
            },
            Stop {
                position: 1.0,
                color: Color::from_rgba8(0, 0, 255, 255),
            },
        ];
        let shader = radial(
            Point::from_xy(12.0, 12.0),
            -8.0,
            Point::from_xy(12.0, 12.0),
            8.0,
            stops,
            SpreadMode::Pad,
            Transform::identity(),
        )
        .unwrap();
        let mut pixmap = Pixmap::new(24, 24).unwrap();
        let paint = Paint {
            shader,
            ..Paint::default()
        };
        pixmap.fill_rect(
            Rect::from_xywh(0.0, 0.0, 24.0, 24.0).unwrap(),
            &paint,
            Transform::identity(),
            None,
        );
        let c = pixmap.pixel(12, 12).unwrap().demultiply();
        assert!(c.red() > 90 && c.red() < 140 && c.blue() > c.red(), "{c:?}");
        assert!(pixmap.pixel(20, 12).unwrap().blue() > 245);
    }

    #[test]
    fn radial_zero_stop_interpolation_matches_native_skia_srgb() {
        let stops = [
            Stop {
                position: 0.0,
                color: Color::from_rgba8(255, 0, 0, 0),
            },
            Stop {
                position: 1.0,
                color: Color::from_rgba8(0, 0, 255, 255),
            },
        ];
        let color = interpolate(&stops, 0.5);
        assert_eq!(color.red(), 0.5);
        assert_eq!(color.blue(), 0.5);
        assert_eq!(color.alpha(), 0.5);
    }

    #[test]
    fn negative_radial_radii_preserve_repeat_and_reflect_periods() {
        for (r0, r1) in [(-4.0, 4.0), (-12.0, -4.0)] {
            for mode in [SpreadMode::Repeat, SpreadMode::Reflect] {
                let stops = vec![
                    Stop {
                        position: 0.0,
                        color: Color::from_rgba8(255, 0, 0, 255),
                    },
                    Stop {
                        position: 1.0,
                        color: Color::from_rgba8(0, 0, 255, 255),
                    },
                ];
                let shader = radial(
                    Point::from_xy(12.0, 12.0),
                    r0,
                    Point::from_xy(12.0, 12.0),
                    r1,
                    stops,
                    mode,
                    Transform::identity(),
                )
                .unwrap();
                let paint = Paint {
                    shader,
                    ..Paint::default()
                };
                let mut pixmap = Pixmap::new(24, 24).unwrap();
                pixmap.fill_rect(
                    Rect::from_xywh(0.0, 0.0, 24.0, 24.0).unwrap(),
                    &paint,
                    Transform::identity(),
                    None,
                );
                for x in [14, 22] {
                    let radius = ((x as f32 + 0.5 - 12.0).powi(2) + 0.5f32.powi(2)).sqrt();
                    let t = (radius - r0) / (r1 - r0);
                    let t = if mode == SpreadMode::Repeat {
                        t.rem_euclid(1.0)
                    } else {
                        let t = t.rem_euclid(2.0);
                        if t > 1.0 { 2.0 - t } else { t }
                    };
                    let actual = pixmap.pixel(x, 12).unwrap().demultiply();
                    assert!(
                        actual.blue().abs_diff((t * 255.0).round() as u8) <= 3,
                        "{r0}..{r1} {mode:?} at {x}: {actual:?}, expected blue {}",
                        t * 255.0
                    );
                }
            }
        }
    }

    #[test]
    fn partial_sweep_normalizes_when_start_angle_is_zero() {
        let mut p = painter(Transform::identity());
        p.push_clip_box(box_at(0.0, 0.0, 24.0, 24.0));
        let stops = [
            ColorStop {
                offset: 0.0,
                palette_index: 0,
                alpha: 1.0,
            },
            ColorStop {
                offset: 1.0,
                palette_index: 1,
                alpha: 1.0,
            },
        ];
        p.fill(Brush::SweepGradient {
            c0: skrifa::raw::types::Point::new(12.0, 12.0),
            start_angle: 0.0,
            end_angle: 180.0,
            color_stops: &stops,
            extend: Extend::Pad,
        });
        let middle = pixel(&p, 12, 3);
        assert!(middle.red().abs_diff(middle.blue()) < 20, "{middle:?}");
        assert!(pixel(&p, 3, 12).blue() > 230);
    }

    #[test]
    fn every_bundled_color_glyph_rasterizes() {
        let font = FontRef::new(FONT).unwrap();
        let count = font
            .glyph_metrics(Size::unscaled(), LocationRef::default())
            .glyph_count();
        let mut rendered = 0;
        for id in 0..count {
            let id = u16::try_from(id).unwrap();
            if has_glyph(FONT, 0, id) {
                let bitmap = rasterize(FONT, 0, id, 32.0, 0.0, &[], crate::Color::black())
                    .unwrap_or_else(|| panic!("COLRv1 glyph {id} did not rasterize"));
                assert!(
                    bitmap.pixels.as_chunks::<4>().0.iter().any(|p| p[3] != 0),
                    "blank glyph {id}"
                );
                rendered += 1;
            }
        }
        assert!(
            rendered > 3500,
            "missing bundled color font: {rendered} glyphs"
        );
    }

    #[test]
    fn malformed_fonts_sizes_and_graph_storage_are_bounded() {
        assert!(rasterize(b"invalid", 0, 1, 32.0, 0.0, &[], crate::Color::black()).is_none());
        assert!(rasterize(FONT, 0, 1, f32::INFINITY, 0.0, &[], crate::Color::black()).is_none());
        let mut p = painter(Transform::identity());
        for _ in 0..MAX_DEPTH + 1 {
            p.push_layer(CompositeMode::SrcOver);
        }
        assert!(p.failed);
        assert!(p.layers.len() <= MAX_DEPTH);
        assert!(p.storage <= MAX_STORAGE);
    }
}
