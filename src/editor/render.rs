//! Draws a document. The same code renders the editor canvas and exports,
//! so what you see is what you save.
//!
//! All drawing happens in canvas points with a top-left origin, into the
//! current (flipped) `NSGraphicsContext`.

use std::f64::consts::PI;

use objc2::AllocAnyThread;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSAffineTransformNSAppKitAdditions, NSBezierPath, NSColor, NSCompositingOperation, NSFont,
    NSGradient, NSGradientDrawingOptions, NSGraphicsContext, NSImage, NSImageInterpolation,
    NSLineCapStyle, NSLineJoinStyle, NSShadow, NSWindingRule,
};
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{CGBitmapContextCreateImage, CGContext, CGImage};
use objc2_foundation::{NSAffineTransform, NSPoint, NSRect, NSSize};

use super::model::{
    Annotation, ArrowStyle, CanvasLayout, Color, Document, Fill, MEASURE_TICK, Orientation, Shape,
    TextStyle, curve_point, rect_center,
};
use crate::capture::geometry::{Point, Rect};
use crate::capture::image;
use crate::ui::draw::{self, Weight};

/// The captured image being annotated, as turned by the document's
/// orientation.
pub struct BaseImage {
    pub cg: CFRetained<CGImage>,
    pub ns: Retained<NSImage>,
    /// Pixels per point.
    pub scale: f64,
    /// Size in points.
    pub width: f64,
    pub height: f64,
    /// The capture before any turn (what projects store).
    pub original: CFRetained<CGImage>,
    pub orientation: Orientation,
    /// Margins found in the pixels, worked out when first needed.
    trims: std::cell::OnceCell<Trims>,
}

/// Parts of the captured image (points) a background can leave out.
#[derive(Clone, Copy, Debug)]
struct Trims {
    /// A window capture's window, inside its transparent shadow margin.
    body: Option<Rect>,
    /// What is left without margins of one plain color.
    plain: Rect,
}

impl BaseImage {
    pub fn new(cg: CFRetained<CGImage>, scale: f64) -> Self {
        Self::turned(cg.clone(), cg, scale, Orientation::default())
    }

    fn turned(
        original: CFRetained<CGImage>,
        cg: CFRetained<CGImage>,
        scale: f64,
        orientation: Orientation,
    ) -> Self {
        let (w, h) = image::pixel_size(&cg);
        let (width, height) = (w as f64 / scale, h as f64 / scale);
        let ns = NSImage::initWithCGImage_size(NSImage::alloc(), &cg, NSSize::new(width, height));
        Self {
            cg,
            ns,
            scale,
            width,
            height,
            original,
            orientation,
            trims: std::cell::OnceCell::new(),
        }
    }

    /// The same capture in `orientation`.
    pub fn with_orientation(&self, orientation: Orientation) -> Self {
        let cg = super::picture::orient_image(&self.original, orientation)
            .unwrap_or_else(|| self.original.clone());
        Self::turned(self.original.clone(), cg, self.scale, orientation)
    }

    /// The original capture's size in points.
    pub fn original_size(&self) -> (f64, f64) {
        let (w, h) = image::pixel_size(&self.original);
        (w as f64 / self.scale, h as f64 / self.scale)
    }

    /// Where everything goes. With a background, a window capture's
    /// transparent shadow margin is left out (the background draws its own
    /// shadow), and so are plain margins with auto-balance.
    pub fn layout(&self, doc: &Document) -> CanvasLayout {
        match &doc.background {
            Some(background) => doc.layout_in(self.shown(background.balance)),
            None => doc.layout(self.width, self.height),
        }
    }

    /// The part of the image shown on a background.
    fn shown(&self, balance: bool) -> Rect {
        let trims = self.trims.get_or_init(|| self.find_trims());
        match (trims.body, balance) {
            (Some(body), _) => body,
            (None, true) => trims.plain,
            (None, false) => Rect::new(0.0, 0.0, self.width, self.height),
        }
    }

    fn find_trims(&self) -> Trims {
        let full = Rect::new(0.0, 0.0, self.width, self.height);
        let Some((w, h, pixels)) = image::to_pixels(&self.cg) else {
            return Trims {
                body: None,
                plain: full,
            };
        };
        let rect = |(l, t, r, b): super::trim::Bounds| {
            Rect::new(
                l as f64 / self.scale,
                t as f64 / self.scale,
                (r + 1 - l) as f64 / self.scale,
                (b + 1 - t) as f64 / self.scale,
            )
        };
        Trims {
            body: super::trim::opaque_body(&pixels, w, h).map(rect),
            plain: rect(super::trim::plain_margins(&pixels, w, h, 6)),
        }
    }
}

pub fn ns_color(c: Color) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(c.r, c.g, c.b, c.a)
}

fn np(p: Point) -> NSPoint {
    NSPoint::new(p.x, p.y)
}

/// Pixels per point of the exported image: the capture's, times Resize.
pub fn export_scale(doc: &Document, base: &BaseImage) -> f64 {
    base.scale * doc.output_scale.unwrap_or(1.0)
}

/// The exported image's size in pixels.
pub fn export_size(doc: &Document, base: &BaseImage) -> (usize, usize) {
    let layout = base.layout(doc);
    let scale = export_scale(doc, base);
    (
        ((layout.width * scale).round() as usize).max(1),
        ((layout.height * scale).round() as usize).max(1),
    )
}

/// Draws the whole document. `skip` leaves out one annotation (the one
/// being edited as text).
pub fn render(doc: &Document, base: &BaseImage, skip: Option<u64>) {
    let Some(context) = NSGraphicsContext::currentContext() else {
        return;
    };
    let layout = base.layout(doc);
    let radius = doc.background.as_ref().map_or(0.0, |b| b.corner_radius);

    if let Some(bg) = &doc.background {
        let canvas = Rect::new(0.0, 0.0, layout.width, layout.height);
        match &bg.fill {
            Fill::Solid { color } => draw::fill_rect(canvas, &ns_color(*color)),
            Fill::Image { picture } => {
                context.saveGraphicsState();
                NSBezierPath::clipRect(draw::ns_rect(canvas));
                draw_picture(picture, cover(picture.size(), canvas), 1.0);
                context.restoreGraphicsState();
            }
            Fill::Gradient { from, to } => {
                // The gradient extends past its end points; keep it on the canvas.
                context.saveGraphicsState();
                NSBezierPath::clipRect(draw::ns_rect(canvas));
                if let Some(gradient) = NSGradient::initWithStartingColor_endingColor(
                    NSGradient::alloc(),
                    &ns_color(*from),
                    &ns_color(*to),
                ) {
                    gradient.drawFromPoint_toPoint_options(
                        NSPoint::new(0.0, 0.0),
                        NSPoint::new(layout.width, layout.height),
                        NSGradientDrawingOptions::DrawsBeforeStartingLocation
                            | NSGradientDrawingOptions::DrawsAfterEndingLocation,
                    );
                }
                context.restoreGraphicsState();
            }
        }
        if bg.shadow {
            context.saveGraphicsState();
            let shadow = NSShadow::new();
            shadow.setShadowBlurRadius(30.0);
            // Shadow offsets are not flipped: negative y is downward.
            shadow.setShadowOffset(NSSize::new(0.0, -10.0));
            shadow.setShadowColor(Some(&NSColor::colorWithWhite_alpha(0.0, 0.45)));
            shadow.set();
            // Cast by what is shown, not the whole content rect: combined
            // images leave parts of it empty.
            for piece in shown_pieces(doc, base, &layout) {
                draw::fill_rounded(piece, radius, &NSColor::blackColor());
            }
            context.restoreGraphicsState();
        }
    }

    context.saveGraphicsState();
    NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
        draw::ns_rect(layout.content),
        radius,
        radius,
    )
    .addClip();
    // From here on, image coordinates: the captured image sits at the
    // origin, and the visible region `s` maps onto the content rect.
    let s = layout.source;
    let transform = NSAffineTransform::transform();
    transform.translateXBy_yBy(layout.content.x - s.x, layout.content.y - s.y);
    transform.concat();
    context.setImageInterpolation(NSImageInterpolation::High);
    // SAFETY: drawing inside a current graphics context.
    unsafe {
        base.ns
            .drawInRect_fromRect_operation_fraction_respectFlipped_hints(
                NSRect::new(NSPoint::ZERO, NSSize::new(base.width, base.height)),
                NSRect::ZERO,
                NSCompositingOperation::SourceOver,
                1.0,
                true,
                None,
            );
    }

    for annotation in &doc.annotations {
        if Some(annotation.id) != skip && !matches!(annotation.shape, Shape::Watermark { .. }) {
            draw_annotation(annotation, base, &layout, &context);
        }
    }
    // Watermarks last, placed in the visible picture as it is now.
    for annotation in &doc.annotations {
        if let Shape::Watermark {
            style,
            picture,
            free,
            ..
        } = &annotation.shape
        {
            let rect = crate::watermark::place(layout.source, picture.size(), style, *free);
            draw_picture(picture, rect, style.opacity);
        }
    }
    context.restoreGraphicsState();
}

pub fn draw_annotation(
    a: &Annotation,
    base: &BaseImage,
    layout: &CanvasLayout,
    context: &NSGraphicsContext,
) {
    let color = ns_color(a.color);
    let sketch = a.sketchy.then_some(a.id);
    match &a.shape {
        Shape::Arrow {
            start,
            end,
            style,
            bend,
        } => draw_arrow(*start, *end, *bend, *style, a.width, &color, sketch),
        Shape::Line { start, end } => match sketch {
            Some(seed) => draw_sketch(&[*start, *end], false, seed, a.width, &color),
            None => stroke(&polyline(&[*start, *end]), a.width, &color),
        },
        Shape::Rectangle {
            rect,
            filled: false,
        } if a.sketchy => {
            let corners = [
                Point::new(rect.x, rect.y),
                Point::new(rect.max_x(), rect.y),
                Point::new(rect.max_x(), rect.max_y()),
                Point::new(rect.x, rect.max_y()),
            ];
            draw_sketch(&corners, true, a.id, a.width, &color);
        }
        Shape::Ellipse {
            rect,
            filled: false,
        } if a.sketchy => {
            // One pass around, with a little overlap where it closes.
            let mut points = ellipse_points(*rect);
            points.extend(points.clone().into_iter().take(3));
            draw_sketch(&points, false, a.id, a.width, &color);
        }
        Shape::Magnify { source, callout } => {
            draw_magnify(base, *source, *callout, a.width, &color)
        }
        Shape::Measure { start, end } => draw_measure(*start, *end, base, a.width, &color),
        Shape::TextHighlight { rects } => {
            let marker = ns_color(Color { a: 0.4, ..a.color });
            for r in rects {
                draw::fill_rounded(*r, (r.height * 0.15).min(4.0), &marker);
            }
        }
        Shape::Rectangle { rect, filled } => {
            let path = NSBezierPath::bezierPathWithRect(draw::ns_rect(*rect));
            if *filled {
                color.setFill();
                path.fill();
            } else {
                path.setLineJoinStyle(NSLineJoinStyle::Round);
                stroke(&path, a.width, &color);
            }
        }
        Shape::Ellipse { rect, filled } => {
            let path = NSBezierPath::bezierPathWithOvalInRect(draw::ns_rect(*rect));
            if *filled {
                color.setFill();
                path.fill();
            } else {
                stroke(&path, a.width, &color);
            }
        }
        Shape::Pen { points } => stroke(&smooth_path(points), a.width, &color),
        Shape::Highlighter { points } => {
            // Translucent paint rather than multiply blending, which would
            // disappear on dark backgrounds.
            let translucent = ns_color(Color { a: 0.4, ..a.color });
            let path = smooth_path(points);
            path.setLineCapStyle(NSLineCapStyle::Square);
            stroke(&path, a.highlighter_width(), &translucent);
        }
        Shape::Text {
            origin,
            text,
            size,
            style,
            ..
        } => draw_text(text, *origin, *size, *style, a.color),
        Shape::Counter { center, number } => {
            let r = a.counter_radius();
            let circle = Rect::new(center.x - r, center.y - r, 2.0 * r, 2.0 * r);
            draw::fill_rounded(circle, r, &color);
            NSColor::whiteColor().setStroke();
            let ring =
                NSBezierPath::bezierPathWithOvalInRect(draw::ns_rect(draw::inset(circle, 1.0)));
            ring.setLineWidth(2.0);
            ring.stroke();
            draw::draw_text_centered(
                &number.to_string(),
                circle,
                r * 1.05,
                Weight::Semibold,
                &NSColor::whiteColor(),
            );
        }
        Shape::Pixelate { rect } => {
            draw_resampled(base, *rect, 10.0, NSImageInterpolation::None, context)
        }
        Shape::Blur { rect } => {
            draw_resampled(base, *rect, 14.0, NSImageInterpolation::High, context)
        }
        Shape::Image {
            rect,
            picture,
            opacity,
        } => draw_picture(picture, *rect, *opacity),
        Shape::Watermark {
            rect,
            picture,
            style,
            ..
        } => draw_picture(picture, *rect, style.opacity),
        Shape::Spotlight { rect } => {
            // Dim everything visible except the spotlight.
            let path = NSBezierPath::bezierPathWithRect(draw::ns_rect(layout.source));
            path.appendBezierPath(&NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                draw::ns_rect(*rect),
                8.0,
                8.0,
            ));
            path.setWindingRule(NSWindingRule::EvenOdd);
            NSColor::colorWithWhite_alpha(0.0, 0.55).setFill();
            path.fill();
        }
    }
}

/// The captured image and the image layers, in canvas points, clipped to
/// the visible region.
fn shown_pieces(doc: &Document, base: &BaseImage, layout: &CanvasLayout) -> Vec<Rect> {
    let to_canvas = |r: Rect| {
        let r = r.intersection(&layout.source);
        let o = layout.to_canvas(Point::new(r.x, r.y));
        Rect::new(o.x, o.y, r.width, r.height)
    };
    std::iter::once(Rect::new(0.0, 0.0, base.width, base.height))
        .chain(doc.annotations.iter().filter_map(|a| match &a.shape {
            Shape::Image { rect, .. } => Some(*rect),
            _ => None,
        }))
        .map(to_canvas)
        .filter(|r| r.width > 0.0 && r.height > 0.0)
        .collect()
}

/// Draws `picture` scaled into `rect` (canvas points, flipped context).
fn draw_picture(picture: &super::picture::Picture, rect: Rect, opacity: f64) {
    let (w, h) = picture.size();
    let ns = NSImage::initWithCGImage_size(NSImage::alloc(), picture.cg(), NSSize::new(w, h));
    // SAFETY: drawing inside a current graphics context.
    unsafe {
        ns.drawInRect_fromRect_operation_fraction_respectFlipped_hints(
            draw::ns_rect(rect),
            NSRect::ZERO,
            NSCompositingOperation::SourceOver,
            opacity.clamp(0.0, 1.0),
            true,
            None,
        );
    }
}

/// The rect that covers `area` with an image of `size`, keeping its aspect
/// ratio and centering it (the overflow is clipped by the caller).
pub fn cover(size: (f64, f64), area: Rect) -> Rect {
    let (w, h) = size;
    if w <= 0.0 || h <= 0.0 {
        return area;
    }
    let factor = (area.width / w).max(area.height / h);
    let (cw, ch) = (w * factor, h * factor);
    Rect::new(
        area.x + (area.width - cw) / 2.0,
        area.y + (area.height - ch) / 2.0,
        cw,
        ch,
    )
}

fn stroke(path: &NSBezierPath, width: f64, color: &NSColor) {
    color.setStroke();
    path.setLineWidth(width);
    path.setLineCapStyle(NSLineCapStyle::Round);
    path.setLineJoinStyle(NSLineJoinStyle::Round);
    path.stroke();
}

/// A path through `points`, smoothed with curves through segment midpoints.
fn smooth_path(points: &[Point]) -> Retained<NSBezierPath> {
    let path = NSBezierPath::bezierPath();
    let Some(first) = points.first() else {
        return path;
    };
    path.moveToPoint(np(*first));
    if points.len() == 1 {
        path.lineToPoint(np(*first));
        return path;
    }
    for pair in points.windows(2).skip(1) {
        let mid = Point::new((pair[0].x + pair[1].x) / 2.0, (pair[0].y + pair[1].y) / 2.0);
        path.curveToPoint_controlPoint1_controlPoint2(np(mid), np(pair[0]), np(pair[0]));
    }
    path.lineToPoint(np(*points.last().unwrap()));
    path
}

/// The arrow's centerline: the straight segment, or points along its
/// curve.
fn arrow_path(start: Point, bend: Option<Point>, end: Point) -> Vec<Point> {
    match bend {
        Some(c) => (0..=48)
            .map(|i| curve_point(start, c, end, f64::from(i) / 48.0))
            .collect(),
        None => vec![start, end],
    }
}

/// The unit direction the arrow points in at its tip.
fn tip_direction(start: Point, bend: Option<Point>, end: Point) -> (f64, f64) {
    let from = bend.unwrap_or(start);
    let (dx, dy) = (end.x - from.x, end.y - from.y);
    let length = dx.hypot(dy).max(f64::MIN_POSITIVE);
    (dx / length, dy / length)
}

/// The part of `path` at least `cut` away (along the path) from its end.
fn trim_end(path: &[Point], cut: f64) -> Vec<Point> {
    let mut out = path.to_vec();
    let mut left = cut;
    while out.len() >= 2 && left > 0.0 {
        let n = out.len();
        let (a, b) = (out[n - 2], out[n - 1]);
        let seg = (b.x - a.x).hypot(b.y - a.y);
        if seg > left {
            let t = (seg - left) / seg;
            out[n - 1] = Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t);
            break;
        }
        left -= seg;
        out.pop();
    }
    out
}

fn path_length(path: &[Point]) -> f64 {
    path.windows(2)
        .map(|w| (w[1].x - w[0].x).hypot(w[1].y - w[0].y))
        .sum()
}

/// The outline of a tapered arrow for stroke width `width`: one side of
/// the shaft from the tail, a barb, the tip, the other barb and the other
/// side back to the tail. The shaft widens from a point; the head grows a
/// little with the arrow's length and its barbs reach back past the point
/// where the shaft joins it. Curved arrows follow their curve up to the
/// head, which points along the curve's end.
pub fn tapered_outline(start: Point, bend: Option<Point>, end: Point, width: f64) -> Vec<Point> {
    let center = arrow_path(start, bend, end);
    let length = path_length(&center).max(1.0);
    let (ux, uy) = tip_direction(start, bend, end);
    let (nx, ny) = (-uy, ux);
    // Not `clamp`: half a very short arrow is less than the minimum.
    let head = (width * 6.0 + length * 0.1).max(12.0).min(length * 0.5);
    let barb = head * 0.55;
    let neck = (width * 1.2).max(head * 0.15);
    let tail = (width * 0.12).min(1.0);
    let shaft = trim_end(&center, head * 0.7);
    let shaft_length = path_length(&shaft).max(f64::MIN_POSITIVE);
    // Each shaft point pushed out to `side`, widening toward the neck.
    let mut walked = 0.0;
    let mut sides: Vec<(Point, Point)> = Vec::with_capacity(shaft.len());
    for (i, p) in shaft.iter().enumerate() {
        if i > 0 {
            walked += (p.x - shaft[i - 1].x).hypot(p.y - shaft[i - 1].y);
        }
        let (a, b) = (
            shaft[i.saturating_sub(1)],
            shaft[(i + 1).min(shaft.len() - 1)],
        );
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let d = dx.hypot(dy).max(f64::MIN_POSITIVE);
        let (px, py) = if i + 1 == shaft.len() {
            (nx, ny)
        } else {
            (-dy / d, dx / d)
        };
        let half = tail + (neck - tail) * (walked / shaft_length);
        sides.push((
            Point::new(p.x - px * half, p.y - py * half),
            Point::new(p.x + px * half, p.y + py * half),
        ));
    }
    let barb_base = Point::new(end.x - ux * head, end.y - uy * head);
    let mut outline: Vec<Point> = sides.iter().map(|s| s.0).collect();
    outline.push(Point::new(barb_base.x - nx * barb, barb_base.y - ny * barb));
    outline.push(end);
    outline.push(Point::new(barb_base.x + nx * barb, barb_base.y + ny * barb));
    outline.extend(sides.iter().rev().map(|s| s.1));
    outline
}

#[allow(clippy::too_many_arguments)]
fn draw_arrow(
    start: Point,
    end: Point,
    bend: Option<Point>,
    style: ArrowStyle,
    width: f64,
    color: &NSColor,
    sketch: Option<u64>,
) {
    let center = arrow_path(start, bend, end);
    let length = path_length(&center);
    if length < 1.0 {
        return;
    }
    let (ux, uy) = tip_direction(start, bend, end);
    let head = (width * 4.5).max(16.0).min(length * 0.6);
    let half = head * 0.55;

    // A filled head with its tip at `tip`, pointing along (dir_x, dir_y).
    let filled_head = |tip: Point, dir_x: f64, dir_y: f64| {
        let base = Point::new(tip.x - dir_x * head, tip.y - dir_y * head);
        let path = NSBezierPath::bezierPath();
        path.moveToPoint(np(tip));
        path.lineToPoint(NSPoint::new(base.x - dir_y * half, base.y + dir_x * half));
        path.lineToPoint(NSPoint::new(base.x + dir_y * half, base.y - dir_x * half));
        path.closePath();
        path.setLineJoinStyle(NSLineJoinStyle::Round);
        color.setFill();
        color.setStroke();
        path.setLineWidth(width.min(4.0));
        path.fill();
        path.stroke();
    };
    let shaft = |points: &[Point]| match sketch {
        Some(seed) => draw_sketch(points, false, seed, width, color),
        None => stroke(&polyline(points), width, color),
    };

    match style {
        ArrowStyle::Filled | ArrowStyle::Double => {
            // Stop the shaft inside the head so its round cap does not poke out.
            let mut points = trim_end(&center, head * 0.6);
            if style == ArrowStyle::Double {
                points.reverse();
                points = trim_end(&points, head * 0.6);
                points.reverse();
            }
            shaft(&points);
            filled_head(end, ux, uy);
            if style == ArrowStyle::Double {
                let (sx, sy) = tip_direction(end, bend, start);
                filled_head(start, sx, sy);
            }
        }
        ArrowStyle::Tapered => {
            let outline = tapered_outline(start, bend, end, width);
            let path = polyline(&outline);
            path.closePath();
            color.setFill();
            path.fill();
            // A hairline in the same color rounds the corners slightly.
            path.setLineJoinStyle(NSLineJoinStyle::Round);
            path.setLineWidth(1.0);
            color.setStroke();
            path.stroke();
        }
        ArrowStyle::Open => {
            shaft(&center);
            let path = NSBezierPath::bezierPath();
            let angle = uy.atan2(ux);
            for side in [-1.0, 1.0] {
                let a = angle + PI + side * PI / 6.0;
                path.moveToPoint(np(end));
                path.lineToPoint(NSPoint::new(end.x + head * a.cos(), end.y + head * a.sin()));
            }
            stroke(&path, width, color);
        }
    }
}

fn polyline(points: &[Point]) -> Retained<NSBezierPath> {
    let path = NSBezierPath::bezierPath();
    if let Some(first) = points.first() {
        path.moveToPoint(np(*first));
        for p in &points[1..] {
            path.lineToPoint(np(*p));
        }
    }
    path
}

/// A small deterministic random number generator, so a hand-drawn shape
/// looks the same on every redraw and export.
struct Wobble(u64);

impl Wobble {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    /// A number from -1 to 1.
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 52) as f64 - 1.0
    }
}

/// Two slightly different hand-drawn passes over the polyline through
/// `points` (closed: back to the first point), seeded by `seed`.
pub fn sketch_passes(points: &[Point], closed: bool, seed: u64, width: f64) -> Vec<Vec<Point>> {
    let mut rng = Wobble::new(seed);
    let mut corners = points.to_vec();
    if closed && let Some(first) = points.first() {
        corners.push(*first);
    }
    let wobble = (width * 0.5).clamp(1.0, 3.0);
    (0..2)
        .map(|_| {
            let mut out = Vec::new();
            for w in corners.windows(2) {
                let (a, b) = (w[0], w[1]);
                let (dx, dy) = (b.x - a.x, b.y - a.y);
                let len = dx.hypot(dy);
                if len < f64::EPSILON {
                    continue;
                }
                let (nx, ny) = (-dy / len, dx / len);
                // A gentle bow plus jitter at the ends; corners overshoot a
                // little, as a quick pen stroke does.
                let bow = rng.next() * (len * 0.012 + wobble).min(6.0);
                let over = wobble * 0.8 * rng.next().abs();
                let steps = ((len / 12.0).ceil() as usize).clamp(2, 40);
                for i in 0..=steps {
                    let t = i as f64 / steps as f64;
                    let along = -over + t * (len + 2.0 * over);
                    let side = bow * (PI * t).sin() + rng.next() * wobble * 0.25;
                    out.push(Point::new(
                        a.x + dx / len * along + nx * side,
                        a.y + dy / len * along + ny * side,
                    ));
                }
            }
            out
        })
        .collect()
}

fn draw_sketch(points: &[Point], closed: bool, seed: u64, width: f64, color: &NSColor) {
    for pass in sketch_passes(points, closed, seed, width) {
        stroke(&smooth_path(&pass), width * 0.85, color);
    }
}

/// Points around the ellipse in `rect`.
fn ellipse_points(rect: Rect) -> Vec<Point> {
    let (cx, cy) = (rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
    (0..36)
        .map(|i| {
            let a = f64::from(i) / 36.0 * 2.0 * PI;
            Point::new(
                cx + rect.width / 2.0 * a.cos(),
                cy + rect.height / 2.0 * a.sin(),
            )
        })
        .collect()
}

/// The two lines that touch both circles on the outside (`c1` with radius
/// `r1`, `c2` with `r2`), as pairs of end points; none when one circle
/// contains the other.
pub fn outer_tangents(c1: Point, r1: f64, c2: Point, r2: f64) -> Vec<(Point, Point)> {
    let (dx, dy) = (c2.x - c1.x, c2.y - c1.y);
    let d = dx.hypot(dy);
    if d <= (r1 - r2).abs() || d < f64::EPSILON {
        return Vec::new();
    }
    let base = dy.atan2(dx);
    let spread = ((r1 - r2) / d).acos();
    [base + spread, base - spread]
        .into_iter()
        .map(|a| {
            let (c, s) = (a.cos(), a.sin());
            (
                Point::new(c1.x + r1 * c, c1.y + r1 * s),
                Point::new(c2.x + r2 * c, c2.y + r2 * s),
            )
        })
        .collect()
}

fn draw_magnify(base: &BaseImage, source: Rect, callout: Rect, width: f64, color: &NSColor) {
    let Some(context) = NSGraphicsContext::currentContext() else {
        return;
    };
    let (sc, cc) = (rect_center(source), rect_center(callout));
    let (sr, cr) = (source.width / 2.0, callout.width / 2.0);
    let line = (width * 0.5).max(1.5);
    let joins = NSBezierPath::bezierPath();
    for (a, b) in outer_tangents(sc, sr, cc, cr) {
        joins.moveToPoint(np(a));
        joins.lineToPoint(np(b));
    }
    stroke(&joins, line, color);
    let ring = NSBezierPath::bezierPathWithOvalInRect(draw::ns_rect(source));
    stroke(&ring, line, color);

    // The magnified pixels, clipped to the callout circle.
    let circle = NSBezierPath::bezierPathWithOvalInRect(draw::ns_rect(callout));
    context.saveGraphicsState();
    circle.addClip();
    NSColor::colorWithWhite_alpha(0.12, 1.0).setFill();
    NSBezierPath::fillRect(draw::ns_rect(callout));
    let area = source.intersection(&Rect::new(0.0, 0.0, base.width, base.height));
    if area.width >= 0.5
        && area.height >= 0.5
        && let Some(region) = image::crop(&base.cg, area, base.scale)
    {
        let zoom = callout.width / source.width.max(f64::MIN_POSITIVE);
        let dest = Rect::new(
            callout.x + (area.x - source.x) * zoom,
            callout.y + (area.y - source.y) * zoom,
            area.width * zoom,
            area.height * zoom,
        );
        let (pw, ph) = image::pixel_size(&region);
        let ns = NSImage::initWithCGImage_size(
            NSImage::alloc(),
            &region,
            NSSize::new(pw as f64, ph as f64),
        );
        context.setImageInterpolation(NSImageInterpolation::High);
        // SAFETY: drawing inside a current graphics context.
        unsafe {
            ns.drawInRect_fromRect_operation_fraction_respectFlipped_hints(
                draw::ns_rect(dest),
                NSRect::ZERO,
                NSCompositingOperation::SourceOver,
                1.0,
                true,
                None,
            );
        }
    }
    context.restoreGraphicsState();
    // A white ring keeps the edge visible on any picture.
    stroke(&circle, width + 3.0, &NSColor::whiteColor());
    stroke(&circle, width, color);
}

fn draw_measure(start: Point, end: Point, base: &BaseImage, width: f64, color: &NSColor) {
    let (dx, dy) = (end.x - start.x, end.y - start.y);
    let length = dx.hypot(dy);
    if length < 1.0 {
        return;
    }
    let (nx, ny) = (-dy / length, dx / length);
    let line = (width * 0.5).max(1.5);
    let path = NSBezierPath::bezierPath();
    path.moveToPoint(np(start));
    path.lineToPoint(np(end));
    for p in [start, end] {
        path.moveToPoint(NSPoint::new(
            p.x - nx * MEASURE_TICK,
            p.y - ny * MEASURE_TICK,
        ));
        path.lineToPoint(NSPoint::new(
            p.x + nx * MEASURE_TICK,
            p.y + ny * MEASURE_TICK,
        ));
    }
    // A dark outline keeps the line readable on light and dark pictures.
    stroke(&path, line + 2.0, &NSColor::colorWithWhite_alpha(0.0, 0.35));
    stroke(&path, line, color);
    let text = measure_label(dx, dy, base.scale);
    let size = draw::text_size(&text, 12.0, Weight::Semibold);
    let mid = Point::new((start.x + end.x) / 2.0, (start.y + end.y) / 2.0);
    // Beside the line, on the side away from it.
    let off = size.height.max(size.width * nx.abs()) / 2.0 + 10.0;
    let (ox, oy) = if ny > 0.0 { (-nx, -ny) } else { (nx, ny) };
    let center = Point::new(mid.x + ox * off, mid.y + oy * off);
    let pill = Rect::new(
        center.x - size.width / 2.0 - 6.0,
        center.y - size.height / 2.0 - 3.0,
        size.width + 12.0,
        size.height + 6.0,
    );
    draw::fill_rounded(pill, pill.height / 2.0, color);
    draw::draw_text_centered(&text, pill, 12.0, Weight::Semibold, &NSColor::whiteColor());
}

/// "120 px" for a straight measurement, "120 × 80 px" when it is
/// diagonal (width and height in image pixels).
pub fn measure_label(dx: f64, dy: f64, scale: f64) -> String {
    let (w, h) = ((dx * scale).abs().round(), (dy * scale).abs().round());
    if w < 1.0 || h < 1.0 {
        format!("{} px", w.max(h))
    } else {
        format!("{w} × {h} px")
    }
}

const TEXT_PADDING: (f64, f64) = (8.0, 4.0);

/// Size of a text annotation in points, including its background.
pub fn text_size(text: &str, size: f64, style: TextStyle) -> NSSize {
    let s = text_bounds(text, size);
    match style {
        TextStyle::Plain => s,
        TextStyle::Background => NSSize::new(
            s.width + 2.0 * TEXT_PADDING.0,
            s.height + 2.0 * TEXT_PADDING.1,
        ),
    }
}

fn text_bounds(text: &str, size: f64) -> NSSize {
    // Measure each line; an empty text still has the height of one line.
    let lines: Vec<&str> = if text.is_empty() {
        vec![" "]
    } else {
        text.lines().collect()
    };
    let mut width: f64 = 0.0;
    let mut height = 0.0;
    for line in lines {
        let s =
            draw::text_size_with_font(if line.is_empty() { " " } else { line }, &text_font(size));
        width = width.max(s.width);
        height += s.height;
    }
    NSSize::new(width.ceil(), height.ceil())
}

pub fn text_font(size: f64) -> Retained<NSFont> {
    NSFont::boldSystemFontOfSize(size)
}

fn draw_text(text: &str, origin: Point, size: f64, style: TextStyle, color: Color) {
    let font = text_font(size);
    match style {
        TextStyle::Plain => {
            draw_lines(text, origin, &font, &ns_color(color));
        }
        TextStyle::Background => {
            let s = text_size(text, size, style);
            draw::fill_rounded(
                Rect::new(origin.x, origin.y, s.width, s.height),
                (size * 0.3).min(10.0),
                &ns_color(color),
            );
            draw_lines(
                text,
                Point::new(origin.x + TEXT_PADDING.0, origin.y + TEXT_PADDING.1),
                &font,
                &NSColor::whiteColor(),
            );
        }
    }
}

fn draw_lines(text: &str, origin: Point, font: &NSFont, color: &NSColor) {
    let mut y = origin.y;
    for line in text.lines() {
        let line = if line.is_empty() { " " } else { line };
        draw::draw_text_with_font(line, Point::new(origin.x, y), font, color);
        y += draw::text_size_with_font(line, font).height;
    }
}

/// Pixelate and blur: resample the original image under `rect` down by
/// `block` points, then draw it back up. Without interpolation that gives
/// square pixels; with high-quality interpolation, a blur.
fn draw_resampled(
    base: &BaseImage,
    rect: Rect,
    block: f64,
    interpolation: NSImageInterpolation,
    context: &NSGraphicsContext,
) {
    let area = rect.intersection(&Rect::new(0.0, 0.0, base.width, base.height));
    if area.width < 1.0 || area.height < 1.0 {
        return;
    }
    let Some(region) = image::crop(&base.cg, area, base.scale) else {
        return;
    };
    let w = ((area.width / block).ceil() as usize).max(1);
    let h = ((area.height / block).ceil() as usize).max(1);
    let Some(small) = image::resize(&region, w, h) else {
        return;
    };
    let small =
        NSImage::initWithCGImage_size(NSImage::alloc(), &small, NSSize::new(w as f64, h as f64));
    context.saveGraphicsState();
    context.setImageInterpolation(interpolation);
    // SAFETY: drawing inside a current graphics context.
    unsafe {
        small.drawInRect_fromRect_operation_fraction_respectFlipped_hints(
            draw::ns_rect(area),
            NSRect::ZERO,
            NSCompositingOperation::Copy,
            1.0,
            true,
            None,
        );
    }
    context.restoreGraphicsState();
}

/// Renders the document at the image's full resolution.
pub fn export(doc: &Document, base: &BaseImage) -> Option<CFRetained<CGImage>> {
    let scale = export_scale(doc, base);
    let (width, height) = export_size(doc, base);
    let cg = image::bitmap_context(width, height, None)?;
    // Top-left origin in points.
    CGContext::translate_ctm(Some(&cg), 0.0, height as f64);
    CGContext::scale_ctm(Some(&cg), scale, -scale);
    let context = NSGraphicsContext::graphicsContextWithCGContext_flipped(&cg, true);
    let previous = NSGraphicsContext::currentContext();
    NSGraphicsContext::setCurrentContext(Some(&context));
    render(doc, base, None);
    context.flushGraphics();
    NSGraphicsContext::setCurrentContext(previous.as_deref());
    CGBitmapContextCreateImage(Some(&cg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tapered_arrows_widen_toward_a_swept_back_head() {
        let (start, end) = (Point::new(0.0, 0.0), Point::new(200.0, 0.0));
        let outline = tapered_outline(start, None, end, 4.0);
        // Tail, neck, barb, tip, barb, neck, tail.
        let [tail, neck, barb, tip, barb2, neck2, tail2] = outline[..] else {
            panic!("{outline:?}");
        };
        assert_eq!(tip, end);
        // Symmetric about the shaft.
        for (a, b) in [(tail, tail2), (neck, neck2), (barb, barb2)] {
            assert!((a.x - b.x).abs() < 1e-9 && (a.y + b.y).abs() < 1e-9);
        }
        // The shaft widens, the barbs are wider still and sit behind the
        // neck (swept back), and the tail is almost a point.
        assert!(tail.y.abs() < 1.0);
        assert!(neck.y.abs() > tail.y.abs() && barb.y.abs() > neck.y.abs() * 2.0);
        assert!(barb.x < neck.x && neck.x < tip.x);
        // Short arrows keep a head no longer than half of them.
        let short = tapered_outline(start, None, Point::new(20.0, 0.0), 8.0);
        assert!(short[2].x >= 10.0 - 1e-9);
    }

    #[test]
    fn curved_tapered_arrows_point_along_the_curve() {
        let (start, end) = (Point::new(0.0, 0.0), Point::new(200.0, 0.0));
        let outline = tapered_outline(start, Some(Point::new(100.0, -100.0)), end, 4.0);
        let tip = outline.iter().position(|p| *p == end).unwrap();
        // The head points down and to the right (the curve's end tangent):
        // both barbs are up and to the left of the tip.
        for barb in [outline[tip - 1], outline[tip + 1]] {
            assert!(barb.x < end.x);
        }
        let mid = (outline[tip - 1].y + outline[tip + 1].y) / 2.0;
        assert!(mid < 0.0, "the head's base is above the tip: {mid}");
        // The shaft follows the curve upward.
        assert!(outline.iter().any(|p| p.y < -40.0));
    }

    #[test]
    fn trims_paths_from_the_end() {
        let path = [
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(10.0, 10.0),
        ];
        assert_eq!(
            trim_end(&path, 15.0),
            vec![Point::new(0.0, 0.0), Point::new(5.0, 0.0)]
        );
        assert_eq!(path_length(&path), 20.0);
    }

    #[test]
    fn sketches_are_repeatable_and_stay_close() {
        let line = [Point::new(0.0, 0.0), Point::new(100.0, 0.0)];
        let a = sketch_passes(&line, false, 7, 4.0);
        assert_eq!(a, sketch_passes(&line, false, 7, 4.0));
        assert_ne!(a, sketch_passes(&line, false, 8, 4.0));
        assert_eq!(a.len(), 2);
        for p in a.iter().flatten() {
            assert!(p.y.abs() <= 6.0 && p.x >= -4.0 && p.x <= 104.0, "{p:?}");
        }
    }

    #[test]
    fn finds_outer_tangents() {
        let lines = outer_tangents(Point::new(0.0, 0.0), 10.0, Point::new(100.0, 0.0), 30.0);
        assert_eq!(lines.len(), 2);
        for (a, b) in lines {
            // Each end is on its circle and the line is perpendicular to
            // both radii.
            assert!((a.x.hypot(a.y) - 10.0).abs() < 1e-9);
            assert!(((b.x - 100.0).hypot(b.y) - 30.0).abs() < 1e-9);
            let (lx, ly) = (b.x - a.x, b.y - a.y);
            assert!((lx * a.x + ly * a.y).abs() < 1e-6);
        }
        assert!(outer_tangents(Point::new(0.0, 0.0), 50.0, Point::new(5.0, 0.0), 10.0).is_empty());
    }

    #[test]
    fn labels_measurements_in_pixels() {
        assert_eq!(measure_label(120.0, 0.0, 2.0), "240 px");
        assert_eq!(measure_label(0.0, -50.2, 1.0), "50 px");
        assert_eq!(measure_label(30.0, 40.0, 2.0), "60 × 80 px");
    }
}
