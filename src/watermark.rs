//! Watermarks: a logo or a line of text in a corner of screenshots and
//! recordings, as TV stations do. A watermark's size and margin are shares
//! of the frame, so it looks the same on a small capture, a Retina capture
//! and a 4K video.
//!
//! The mark (the logo, or the text drawn once) is rendered to an image on
//! the main thread; placing and drawing it with its opacity is plain
//! CoreGraphics and works on any thread.

use std::cell::RefCell;
use std::sync::Mutex;

use objc2_app_kit::{NSColor, NSFont, NSGraphicsContext, NSShadow};
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGBitmapContextCreateImage, CGContext, CGImage, CGInterpolationQuality};
use objc2_foundation::NSSize;
use serde::{Deserialize, Serialize};

use crate::capture::geometry::{Point, Rect};
use crate::capture::image;
use crate::ui::draw;

/// Where in the frame the watermark sits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Position {
    TopLeft,
    Top,
    TopRight,
    Left,
    Center,
    Right,
    BottomLeft,
    Bottom,
    #[default]
    BottomRight,
}

impl Position {
    pub const ALL: [Position; 9] = [
        Position::TopLeft,
        Position::Top,
        Position::TopRight,
        Position::Left,
        Position::Center,
        Position::Right,
        Position::BottomLeft,
        Position::Bottom,
        Position::BottomRight,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Position::TopLeft => "Top Left",
            Position::Top => "Top",
            Position::TopRight => "Top Right",
            Position::Left => "Left",
            Position::Center => "Center",
            Position::Right => "Right",
            Position::BottomLeft => "Bottom Left",
            Position::Bottom => "Bottom",
            Position::BottomRight => "Bottom Right",
        }
    }

    /// 0 (left or top), 0.5 (middle) or 1 (right or bottom), each way.
    fn fractions(self) -> (f64, f64) {
        let column = |p| match p {
            Position::TopLeft | Position::Left | Position::BottomLeft => 0.0,
            Position::Top | Position::Center | Position::Bottom => 0.5,
            _ => 1.0,
        };
        let row = |p| match p {
            Position::TopLeft | Position::Top | Position::TopRight => 0.0,
            Position::Left | Position::Center | Position::Right => 0.5,
            _ => 1.0,
        };
        (column(self), row(self))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    #[default]
    Text,
    Image,
}

/// What a watermark shows and how.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Style {
    pub kind: Kind,
    /// For text watermarks: one line, such as "© Acme".
    pub text: String,
    /// For image watermarks: a file, best a PNG with transparency.
    pub image: String,
    /// The text's color, `#RRGGBB`.
    pub color: String,
    /// A dark rounded background behind the text.
    pub background: bool,
    /// 0 (invisible) to 1.
    pub opacity: f64,
    /// Its width, in percent of the frame's width.
    pub size: f64,
    pub position: Position,
    /// Its distance from the edges, in percent of the frame's shorter side.
    pub margin: f64,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            kind: Kind::Text,
            text: String::new(),
            image: String::new(),
            color: "#FFFFFF".into(),
            background: false,
            opacity: 0.7,
            size: 15.0,
            position: Position::BottomRight,
            margin: 3.0,
        }
    }
}

/// The `[watermark]` settings: the default watermark and what gets it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Put the watermark on new screenshots.
    pub screenshots: bool,
    /// Put it on new recordings, and on new Studio recordings' exports.
    pub recordings: bool,
    #[serde(flatten)]
    pub style: Style,
}

static SETTINGS: Mutex<Option<Settings>> = Mutex::new(None);

/// Applies settings; callable from any thread.
pub fn configure(settings: Settings) {
    if let Ok(mut s) = SETTINGS.lock() {
        *s = Some(settings);
    }
}

pub fn settings() -> Settings {
    SETTINGS
        .lock()
        .ok()
        .and_then(|s| s.clone())
        .unwrap_or_default()
}

/// Where a watermark of `mark`'s proportions (width, height) goes in
/// `frame`: anchored at its position, or with its center at `free`
/// (fractions of the frame, 0 to 1 each way). It never grows past the
/// frame's margins.
pub fn place(frame: Rect, mark: (f64, f64), style: &Style, free: Option<Point>) -> Rect {
    let (mw, mh) = (mark.0.max(1.0), mark.1.max(1.0));
    let margin = frame.width.min(frame.height) * style.margin.clamp(0.0, 25.0) / 100.0;
    let mut width = frame.width * style.size.clamp(1.0, 100.0) / 100.0;
    let mut height = width * mh / mw;
    let fit = ((frame.width - 2.0 * margin).max(1.0) / width)
        .min((frame.height - 2.0 * margin).max(1.0) / height)
        .min(1.0);
    width *= fit;
    height *= fit;
    let (x, y) = match free {
        Some(center) => (
            (frame.x + center.x * frame.width - width / 2.0)
                .clamp(frame.x, (frame.max_x() - width).max(frame.x)),
            (frame.y + center.y * frame.height - height / 2.0)
                .clamp(frame.y, (frame.max_y() - height).max(frame.y)),
        ),
        None => {
            let (fx, fy) = style.position.fractions();
            (
                frame.x + margin + fx * (frame.width - 2.0 * margin - width),
                frame.y + margin + fy * (frame.height - 2.0 * margin - height),
            )
        }
    };
    Rect::new(x, y, width, height)
}

/// The center of `rect` as fractions of `frame`, for a dragged watermark.
pub fn center_in(rect: Rect, frame: Rect) -> Point {
    Point::new(
        ((rect.x + rect.width / 2.0 - frame.x) / frame.width.max(f64::MIN_POSITIVE))
            .clamp(0.0, 1.0),
        ((rect.y + rect.height / 2.0 - frame.y) / frame.height.max(f64::MIN_POSITIVE))
            .clamp(0.0, 1.0),
    )
}

/// `#RRGGBB` (or `#RRGGBBAA`) as red, green, blue and alpha from 0 to 1;
/// white when it is not a color.
pub fn parse_color(hex: &str) -> (f64, f64, f64, f64) {
    let digits = hex.trim().trim_start_matches('#');
    let channel = |i: usize| {
        digits
            .get(i..i + 2)
            .and_then(|d| u8::from_str_radix(d, 16).ok())
            .map(|v| f64::from(v) / 255.0)
    };
    match (digits.len(), channel(0), channel(2), channel(4)) {
        (6 | 8, Some(r), Some(g), Some(b)) => (r, g, b, channel(6).unwrap_or(1.0)),
        _ => (1.0, 1.0, 1.0, 1.0),
    }
}

/// Renders the mark for `style`: the image file, or the text drawn large.
/// Main thread (text uses AppKit).
pub fn render(style: &Style) -> Result<CFRetained<CGImage>, String> {
    match style.kind {
        Kind::Image => {
            let path = crate::capture::naming::expand_home(style.image.trim());
            if style.image.trim().is_empty() {
                return Err(t!("No watermark image is chosen"));
            }
            image::load(&path).ok_or_else(|| {
                t!(
                    "The watermark image {path} could not be opened",
                    path = path.display()
                )
            })
        }
        Kind::Text => {
            let text = style.text.lines().next().unwrap_or("").trim();
            if text.is_empty() {
                return Err(t!("The watermark has no text"));
            }
            render_text(text, style).ok_or_else(|| t!("Could not draw the watermark text"))
        }
    }
}

/// The text at a large size (so it stays sharp when scaled), with a soft
/// shadow, or on a dark rounded background.
fn render_text(text: &str, style: &Style) -> Option<CFRetained<CGImage>> {
    const FONT: f64 = 160.0;
    let font = NSFont::boldSystemFontOfSize(FONT);
    let size = draw::text_size_with_font(text, &font);
    let (pad_x, pad_y) = if style.background {
        (FONT * 0.4, FONT * 0.2)
    } else {
        (FONT * 0.12, FONT * 0.12)
    };
    let (w, h) = (
        (size.width + 2.0 * pad_x).ceil(),
        (size.height + 2.0 * pad_y).ceil(),
    );
    let context = image::bitmap_context(w as usize, h as usize, None)?;
    CGContext::translate_ctm(Some(&context), 0.0, h);
    CGContext::scale_ctm(Some(&context), 1.0, -1.0);
    let graphics = NSGraphicsContext::graphicsContextWithCGContext_flipped(&context, true);
    let previous = NSGraphicsContext::currentContext();
    NSGraphicsContext::setCurrentContext(Some(&graphics));
    if style.background {
        draw::fill_rounded(
            Rect::new(0.0, 0.0, w, h),
            h * 0.3,
            &NSColor::colorWithWhite_alpha(0.0, 0.55),
        );
    } else {
        graphics.saveGraphicsState();
        let shadow = NSShadow::new();
        shadow.setShadowBlurRadius(FONT * 0.06);
        // Shadow offsets are not flipped: negative y is downward.
        shadow.setShadowOffset(NSSize::new(0.0, -FONT * 0.015));
        shadow.setShadowColor(Some(&NSColor::colorWithWhite_alpha(0.0, 0.5)));
        shadow.set();
    }
    let (r, g, b, a) = parse_color(&style.color);
    draw::draw_text_with_font(
        text,
        Point::new(pad_x, pad_y),
        &font,
        &NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, a),
    );
    if !style.background {
        graphics.restoreGraphicsState();
    }
    graphics.flushGraphics();
    NSGraphicsContext::setCurrentContext(previous.as_deref());
    CGBitmapContextCreateImage(Some(&context))
}

thread_local! {
    /// The last rendered mark, by style.
    static MARK: RefCell<Option<(Style, Option<CFRetained<CGImage>>)>> = const { RefCell::new(None) };
}

/// The mark for `style`, rendered once per style. Main thread.
pub fn mark(style: &Style) -> Result<CFRetained<CGImage>, String> {
    if let Some(Some(cached)) = MARK.with(|m| {
        m.borrow()
            .as_ref()
            .filter(|(s, _)| s == style)
            .map(|(_, image)| image.clone())
    }) {
        return Ok(cached);
    }
    let rendered = render(style);
    MARK.with(|m| *m.borrow_mut() = Some((style.clone(), rendered.as_ref().ok().cloned())));
    rendered
}

/// A watermark ready to draw on any thread: the mark and how to place it.
#[derive(Clone)]
pub struct Overlay {
    pub mark: CFRetained<CGImage>,
    pub style: Style,
    /// A dragged watermark's center, as fractions of the frame.
    pub free: Option<Point>,
}

// SAFETY: CGImages are immutable and thread-safe; the rest is plain data.
unsafe impl Send for Overlay {}
unsafe impl Sync for Overlay {}

impl Overlay {
    /// The watermark from Settings > Watermark. Main thread.
    pub fn from_settings() -> Result<Overlay, String> {
        let style = settings().style;
        Ok(Overlay {
            mark: mark(&style)?,
            style,
            free: None,
        })
    }

    /// The default watermark from the settings, if it renders. Main thread.
    pub fn default_for_new(recording: bool) -> Option<Overlay> {
        let settings = settings();
        if !(if recording {
            settings.recordings
        } else {
            settings.screenshots
        }) {
            return None;
        }
        match mark(&settings.style) {
            Ok(mark) => Some(Overlay {
                mark,
                style: settings.style,
                free: None,
            }),
            Err(e) => {
                tracing::warn!(error = %e, "watermark left out");
                None
            }
        }
    }

    /// Where it goes in a frame of `size` (pixels or points; the result is
    /// in the same unit), top-left origin.
    pub fn rect_in(&self, size: (f64, f64)) -> Rect {
        let (mw, mh) = image::pixel_size(&self.mark);
        place(
            Rect::new(0.0, 0.0, size.0, size.1),
            (mw as f64, mh as f64),
            &self.style,
            self.free,
        )
    }

    /// Draws it into `context`, a bitmap of `size` pixels with
    /// CoreGraphics's usual bottom-left origin.
    pub fn draw(&self, context: &CGContext, size: (f64, f64)) {
        let r = self.rect_in(size);
        CGContext::save_g_state(Some(context));
        CGContext::set_alpha(Some(context), self.style.opacity.clamp(0.0, 1.0));
        CGContext::set_interpolation_quality(Some(context), CGInterpolationQuality::High);
        CGContext::draw_image(
            Some(context),
            CGRect::new(
                CGPoint::new(r.x, size.1 - r.y - r.height),
                CGSize::new(r.width, r.height),
            ),
            Some(&self.mark),
        );
        CGContext::restore_g_state(Some(context));
    }

    /// Draws it onto a BGRA video frame in place.
    pub fn draw_on_frame(&self, sample: &objc2_core_media::CMSampleBuffer) {
        use objc2_core_video::{
            CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight,
            CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
            CVPixelBufferUnlockBaseAddress,
        };
        // SAFETY: a video sample from a reader asked for BGRA frames; the
        // pixels stay locked while the context draws on them.
        unsafe {
            let Some(pixels) = sample.image_buffer() else {
                return;
            };
            if CVPixelBufferLockBaseAddress(&pixels, CVPixelBufferLockFlags(0)) != 0 {
                return;
            }
            let (w, h) = (
                CVPixelBufferGetWidth(&pixels),
                CVPixelBufferGetHeight(&pixels),
            );
            if let Some(context) = image::bitmap_context_over(
                CVPixelBufferGetBaseAddress(&pixels),
                w,
                h,
                CVPixelBufferGetBytesPerRow(&pixels),
            ) {
                self.draw(&context, (w as f64, h as f64));
                CGContext::flush(Some(&context));
            }
            CVPixelBufferUnlockBaseAddress(&pixels, CVPixelBufferLockFlags(0));
        }
    }

    /// `image` with the watermark on it.
    pub fn apply(&self, image: &CGImage) -> Option<CFRetained<CGImage>> {
        let (w, h) = image::pixel_size(image);
        let space = CGImage::color_space(Some(image));
        let context = image::bitmap_context(w, h, space.as_deref())?;
        let size = (w as f64, h as f64);
        CGContext::draw_image(
            Some(&context),
            CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(size.0, size.1)),
            Some(image),
        );
        self.draw(&context, size);
        CGBitmapContextCreateImage(Some(&context))
    }
}

/// `style` on a sample 16:9 frame (a gradient with a window on it), for
/// Settings. Main thread.
pub fn preview(style: &Style) -> Result<CFRetained<CGImage>, String> {
    let (w, h) = (640.0, 360.0);
    let context = image::bitmap_context(w as usize, h as usize, None)
        .ok_or_else(|| t!("Could not draw the preview"))?;
    CGContext::translate_ctm(Some(&context), 0.0, h);
    CGContext::scale_ctm(Some(&context), 1.0, -1.0);
    let graphics = NSGraphicsContext::graphicsContextWithCGContext_flipped(&context, true);
    let previous = NSGraphicsContext::currentContext();
    NSGraphicsContext::setCurrentContext(Some(&graphics));
    let bands = [
        (0.20, 0.36, 0.55),
        (0.27, 0.40, 0.62),
        (0.36, 0.43, 0.68),
        (0.45, 0.45, 0.72),
    ];
    for (i, (r, g, b)) in bands.iter().enumerate() {
        draw::fill_rect(
            Rect::new(0.0, h / 4.0 * i as f64, w, h / 4.0),
            &NSColor::colorWithSRGBRed_green_blue_alpha(*r, *g, *b, 1.0),
        );
    }
    let window = Rect::new(80.0, 50.0, 480.0, 260.0);
    draw::fill_rounded(window, 12.0, &NSColor::colorWithWhite_alpha(0.97, 1.0));
    draw::fill_rounded(
        Rect::new(window.x, window.y, window.width, 34.0),
        12.0,
        &NSColor::colorWithWhite_alpha(0.9, 1.0),
    );
    for (i, width) in [300.0, 380.0, 240.0, 340.0].iter().enumerate() {
        draw::fill_rounded(
            Rect::new(
                window.x + 30.0,
                window.y + 70.0 + 40.0 * i as f64,
                *width,
                14.0,
            ),
            7.0,
            &NSColor::colorWithWhite_alpha(0.82, 1.0),
        );
    }
    graphics.flushGraphics();
    NSGraphicsContext::setCurrentContext(previous.as_deref());
    let sample = CGBitmapContextCreateImage(Some(&context))
        .ok_or_else(|| t!("Could not draw the preview"))?;
    let overlay = Overlay {
        mark: mark(style)?,
        style: style.clone(),
        free: None,
    };
    overlay
        .apply(&sample)
        .ok_or_else(|| t!("Could not draw the preview"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(position: Position) -> Style {
        Style {
            position,
            size: 20.0,
            margin: 5.0,
            ..Style::default()
        }
    }

    #[test]
    fn anchors_in_each_position_with_a_margin() {
        let frame = Rect::new(0.0, 0.0, 1000.0, 500.0);
        // 20% of 1000 wide, a 4:1 mark: 200 × 50; margin 5% of 500 = 25.
        let at = |p| place(frame, (400.0, 100.0), &style(p), None);
        assert_eq!(
            at(Position::BottomRight),
            Rect::new(775.0, 425.0, 200.0, 50.0)
        );
        assert_eq!(at(Position::TopLeft), Rect::new(25.0, 25.0, 200.0, 50.0));
        assert_eq!(at(Position::Center), Rect::new(400.0, 225.0, 200.0, 50.0));
        assert_eq!(at(Position::Bottom), Rect::new(400.0, 425.0, 200.0, 50.0));
    }

    #[test]
    fn scales_with_the_frame() {
        let small = place(
            Rect::new(0.0, 0.0, 800.0, 600.0),
            (2.0, 1.0),
            &style(Position::BottomRight),
            None,
        );
        let large = place(
            Rect::new(0.0, 0.0, 3200.0, 2400.0),
            (2.0, 1.0),
            &style(Position::BottomRight),
            None,
        );
        assert_eq!(large.width, small.width * 4.0);
        assert_eq!(large.x, small.x * 4.0);
        // Offsets carry over.
        let shifted = place(
            Rect::new(100.0, 50.0, 800.0, 600.0),
            (2.0, 1.0),
            &style(Position::BottomRight),
            None,
        );
        assert_eq!(shifted.offset(-100.0, -50.0), small);
    }

    #[test]
    fn never_outgrows_the_frame() {
        // A tall mark at 90% of a wide, short frame: limited by the height.
        let r = place(
            Rect::new(0.0, 0.0, 1000.0, 100.0),
            (1.0, 2.0),
            &Style {
                size: 90.0,
                margin: 10.0,
                ..Style::default()
            },
            None,
        );
        assert!((r.height - 80.0).abs() < 1e-9 && (r.width - 40.0).abs() < 1e-9);
        assert!(r.y >= 10.0 && r.max_y() <= 90.0 + 1e-9);
    }

    #[test]
    fn places_and_finds_free_positions() {
        let frame = Rect::new(0.0, 0.0, 1000.0, 500.0);
        let s = style(Position::BottomRight);
        let r = place(frame, (4.0, 1.0), &s, Some(Point::new(0.5, 0.5)));
        assert_eq!(r, Rect::new(400.0, 225.0, 200.0, 50.0));
        assert_eq!(center_in(r, frame), Point::new(0.5, 0.5));
        // Dragged past the edge: kept inside.
        let edge = place(frame, (4.0, 1.0), &s, Some(Point::new(1.0, 0.0)));
        assert_eq!(edge, Rect::new(800.0, 0.0, 200.0, 50.0));
    }

    #[test]
    fn parses_colors() {
        assert_eq!(parse_color("#FF0000"), (1.0, 0.0, 0.0, 1.0));
        assert_eq!(parse_color("00ff0080").3, 128.0 / 255.0);
        assert_eq!(parse_color("nope"), (1.0, 1.0, 1.0, 1.0));
    }

    #[test]
    fn reads_flat_settings() {
        let settings: Settings = toml::from_str(
            "screenshots = true\nkind = \"image\"\nimage = \"~/logo.png\"\nposition = \"top_left\"\nopacity = 0.5\n",
        )
        .unwrap();
        assert!(settings.screenshots && !settings.recordings);
        assert_eq!(settings.style.kind, Kind::Image);
        assert_eq!(settings.style.position, Position::TopLeft);
        assert_eq!(settings.style.opacity, 0.5);
        assert_eq!(settings.style.size, 15.0);
        let written = toml::to_string(&settings).unwrap();
        assert_eq!(toml::from_str::<Settings>(&written).unwrap(), settings);
    }
}
