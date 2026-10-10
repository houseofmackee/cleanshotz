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

use super::model::{Annotation, ArrowStyle, CanvasLayout, Color, Document, Fill, Shape, TextStyle};
use crate::capture::geometry::{Point, Rect};
use crate::capture::image;
use crate::ui::draw::{self, Weight};

/// The captured image being annotated.
pub struct BaseImage {
    pub cg: CFRetained<CGImage>,
    pub ns: Retained<NSImage>,
    /// Pixels per point.
    pub scale: f64,
    /// Size in points.
    pub width: f64,
    pub height: f64,
}

impl BaseImage {
    pub fn new(cg: CFRetained<CGImage>, scale: f64) -> Self {
        let (w, h) = image::pixel_size(&cg);
        let (width, height) = (w as f64 / scale, h as f64 / scale);
        let ns = NSImage::initWithCGImage_size(NSImage::alloc(), &cg, NSSize::new(width, height));
        Self {
            cg,
            ns,
            scale,
            width,
            height,
        }
    }

    pub fn layout(&self, doc: &Document) -> CanvasLayout {
        doc.layout(self.width, self.height)
    }
}

pub fn ns_color(c: Color) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(c.r, c.g, c.b, c.a)
}

fn np(p: Point) -> NSPoint {
    NSPoint::new(p.x, p.y)
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
                draw_picture(picture, cover(picture.size(), canvas));
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
        if Some(annotation.id) != skip {
            draw_annotation(annotation, base, &layout, &context);
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
    match &a.shape {
        Shape::Arrow { start, end, style } => draw_arrow(*start, *end, *style, a.width, &color),
        Shape::Line { start, end } => {
            let path = NSBezierPath::bezierPath();
            path.moveToPoint(np(*start));
            path.lineToPoint(np(*end));
            stroke(&path, a.width, &color);
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
        Shape::Image { rect, picture } => draw_picture(picture, *rect),
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
fn draw_picture(picture: &super::picture::Picture, rect: Rect) {
    let (w, h) = picture.size();
    let ns = NSImage::initWithCGImage_size(NSImage::alloc(), picture.cg(), NSSize::new(w, h));
    // SAFETY: drawing inside a current graphics context.
    unsafe {
        ns.drawInRect_fromRect_operation_fraction_respectFlipped_hints(
            draw::ns_rect(rect),
            NSRect::ZERO,
            NSCompositingOperation::SourceOver,
            1.0,
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

fn draw_arrow(start: Point, end: Point, style: ArrowStyle, width: f64, color: &NSColor) {
    let (dx, dy) = (end.x - start.x, end.y - start.y);
    let length = dx.hypot(dy);
    if length < 1.0 {
        return;
    }
    let (ux, uy) = (dx / length, dy / length);
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

    match style {
        ArrowStyle::Filled | ArrowStyle::Double => {
            // Stop the shaft inside the head so its round cap does not poke out.
            let shaft_end = Point::new(end.x - ux * head * 0.6, end.y - uy * head * 0.6);
            let shaft_start = if style == ArrowStyle::Double {
                Point::new(start.x + ux * head * 0.6, start.y + uy * head * 0.6)
            } else {
                start
            };
            let path = NSBezierPath::bezierPath();
            path.moveToPoint(np(shaft_start));
            path.lineToPoint(np(shaft_end));
            stroke(&path, width, color);
            filled_head(end, ux, uy);
            if style == ArrowStyle::Double {
                filled_head(start, -ux, -uy);
            }
        }
        ArrowStyle::Open => {
            let path = NSBezierPath::bezierPath();
            path.moveToPoint(np(start));
            path.lineToPoint(np(end));
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
    let layout = base.layout(doc);
    let width = (layout.width * base.scale).round() as usize;
    let height = (layout.height * base.scale).round() as usize;
    let cg = image::bitmap_context(width, height, None)?;
    // Top-left origin in points.
    CGContext::translate_ctm(Some(&cg), 0.0, height as f64);
    CGContext::scale_ctm(Some(&cg), base.scale, -base.scale);
    let context = NSGraphicsContext::graphicsContextWithCGContext_flipped(&cg, true);
    let previous = NSGraphicsContext::currentContext();
    NSGraphicsContext::setCurrentContext(Some(&context));
    render(doc, base, None);
    context.flushGraphics();
    NSGraphicsContext::setCurrentContext(previous.as_deref());
    CGBitmapContextCreateImage(Some(&cg))
}
