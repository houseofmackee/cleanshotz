//! The annotation document: what was drawn on a capture, independent of
//! how it is displayed. Coordinates are image points with a top-left
//! origin. Everything here is plain data, serializable into `.cszdoc`
//! projects.

use serde::{Deserialize, Serialize};

use super::picture::Picture;
use crate::capture::geometry::{Point, Rect};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Color {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

impl Color {
    pub const fn rgb(r: f64, g: f64, b: f64) -> Self {
        Self { r, g, b, a: 1.0 }
    }

    pub const RED: Color = Color::rgb(1.0, 0.23, 0.19);
    pub const WHITE: Color = Color::rgb(1.0, 1.0, 1.0);
    pub const BLACK: Color = Color::rgb(0.0, 0.0, 0.0);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArrowStyle {
    /// A solid triangular head.
    Filled,
    /// Two strokes forming the head.
    Open,
    /// Solid heads on both ends.
    Double,
    /// One solid shape: a shaft that widens from a point at the tail into
    /// a large head with swept-back barbs (CleanShot X's arrow).
    Tapered,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextStyle {
    /// Colored text.
    Plain,
    /// White text on a rounded background in the color.
    Background,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Shape {
    Arrow {
        start: Point,
        end: Point,
        style: ArrowStyle,
        /// The control point of a curved arrow (a quadratic curve).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bend: Option<Point>,
    },
    Line {
        start: Point,
        end: Point,
    },
    Rectangle {
        rect: Rect,
        filled: bool,
    },
    Ellipse {
        rect: Rect,
        filled: bool,
    },
    Pen {
        points: Vec<Point>,
    },
    Highlighter {
        points: Vec<Point>,
    },
    Text {
        origin: Point,
        text: String,
        size: f64,
        style: TextStyle,
        /// Measured when the text is set, for hit-testing.
        bounds: Rect,
    },
    Counter {
        center: Point,
        number: u32,
    },
    Pixelate {
        rect: Rect,
    },
    Blur {
        rect: Rect,
    },
    Spotlight {
        rect: Rect,
    },
    /// A pasted or dropped image, or a combined capture. May extend past
    /// the captured image; the canvas grows to include it.
    Image {
        rect: Rect,
        picture: Picture,
        /// 0 to 1, for lining up captures (overlay compare).
        #[serde(default = "opaque", skip_serializing_if = "is_opaque")]
        opacity: f64,
    },
    /// A zoomed copy of the circle in `source` shown in the circle in
    /// `callout` (both squares), joined by two lines.
    Magnify {
        source: Rect,
        callout: Rect,
    },
    /// A dimension line labeled with its length in image pixels.
    Measure {
        start: Point,
        end: Point,
    },
    /// Translucent markers over parts of lines of text (the highlighter's
    /// text mode).
    TextHighlight {
        rects: Vec<Rect>,
    },
    /// A logo or text in a corner of the visible picture (see
    /// [`crate::watermark`]): anchored at its style's position, or with
    /// its center at `free` (fractions of the picture) once dragged. It is
    /// always drawn on top.
    Watermark {
        style: crate::watermark::Style,
        /// The mark: the logo, or the text drawn once.
        picture: Picture,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        free: Option<Point>,
        /// Where it is now, in image points, and the visible picture it was
        /// placed in; kept current by [`Document::place_watermarks`].
        #[serde(skip)]
        rect: Rect,
        #[serde(skip)]
        frame: Rect,
    },
}

fn opaque() -> f64 {
    1.0
}

fn is_opaque(opacity: &f64) -> bool {
    *opacity >= 1.0
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Annotation {
    pub id: u64,
    pub shape: Shape,
    pub color: Color,
    /// Stroke width in points.
    pub width: f64,
    /// Drawn as if by hand (rectangles, ellipses, lines and arrows).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sketchy: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Fill {
    Solid {
        color: Color,
    },
    /// Top-left to bottom-right.
    Gradient {
        from: Color,
        to: Color,
    },
    /// Scaled to cover the canvas.
    Image {
        picture: Picture,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Aspect {
    /// Padding only.
    Auto,
    Square,
    /// 4:5
    Portrait,
    /// 9:16
    Story,
    /// 16:9
    Wide,
    /// 4:3
    Standard,
    /// 3:2
    Photo,
    /// 1.91:1, the size of link previews.
    Link,
}

impl Aspect {
    /// In menu order.
    pub const ALL: [Aspect; 8] = [
        Aspect::Auto,
        Aspect::Square,
        Aspect::Portrait,
        Aspect::Story,
        Aspect::Wide,
        Aspect::Standard,
        Aspect::Photo,
        Aspect::Link,
    ];

    pub fn ratio(self) -> Option<f64> {
        match self {
            Aspect::Auto => None,
            Aspect::Square => Some(1.0),
            Aspect::Portrait => Some(4.0 / 5.0),
            Aspect::Story => Some(9.0 / 16.0),
            Aspect::Wide => Some(16.0 / 9.0),
            Aspect::Standard => Some(4.0 / 3.0),
            Aspect::Photo => Some(3.0 / 2.0),
            Aspect::Link => Some(1.91),
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Aspect::Auto => "Auto",
            Aspect::Square => "1:1 Square (Instagram)",
            Aspect::Portrait => "4:5 Portrait (Instagram)",
            Aspect::Story => "9:16 Story (Instagram, TikTok)",
            Aspect::Wide => "16:9 Wide (YouTube)",
            Aspect::Standard => "4:3 Standard",
            Aspect::Photo => "3:2 Photo",
            Aspect::Link => "1.91:1 Link Preview (X, LinkedIn)",
        }
    }
}

/// A backdrop around the image (CleanShot's background tool).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Background {
    pub fill: Fill,
    pub padding: f64,
    pub corner_radius: f64,
    pub shadow: bool,
    pub aspect: Aspect,
    /// Trim margins of one plain color from the capture, so it sits in the
    /// middle of the background (auto-balance).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub balance: bool,
}

impl Default for Background {
    fn default() -> Self {
        Self {
            fill: Fill::Gradient {
                from: Color::rgb(0.98, 0.45, 0.42),
                to: Color::rgb(0.55, 0.36, 0.96),
            },
            padding: 64.0,
            corner_radius: 12.0,
            shadow: true,
            aspect: Aspect::Auto,
            balance: false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Document {
    pub annotations: Vec<Annotation>,
    /// Visible part of the image, in image points.
    pub crop: Option<Rect>,
    pub background: Option<Background>,
    pub next_id: u64,
    /// How the captured image is turned. Image points are those of the
    /// turned image.
    pub orientation: Orientation,
    /// Exported pixels relative to the capture's own (Resize); `None` is
    /// the original size.
    pub output_scale: Option<f64>,
}

/// The Resize factor for what was typed as the width and height, given
/// the exported size at the original scale (`native`) and the size shown
/// before (`shown`). Each field takes pixels or a percentage of the
/// original; the field that changed decides, keeping proportions. `None`
/// for the original size; `Err` when the input makes no sense.
pub fn resize_factor(
    width: &str,
    height: &str,
    native: (usize, usize),
    shown: (usize, usize),
) -> Result<Option<f64>, ()> {
    let parse = |text: &str, native: usize| -> Result<f64, ()> {
        let text = text.trim();
        let factor = match text.strip_suffix('%') {
            Some(percent) => percent.trim().parse::<f64>().map_err(|_| ())? / 100.0,
            None => {
                let px = text.trim_end_matches("px").trim();
                px.parse::<f64>().map_err(|_| ())? / native.max(1) as f64
            }
        };
        if factor.is_finite() && factor > 0.0 && factor <= 8.0 {
            Ok(factor)
        } else {
            Err(())
        }
    };
    let unchanged = |text: &str, shown: usize| text.trim() == shown.to_string();
    let factor = if !unchanged(width, shown.0) {
        parse(width, native.0)?
    } else if !unchanged(height, shown.1) {
        parse(height, native.1)?
    } else {
        // Nothing typed: keep the shown size.
        shown.0 as f64 / native.0.max(1) as f64
    };
    Ok(((factor - 1.0).abs() > 0.001).then_some(factor))
}

/// Rotating or mirroring the whole image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Turn {
    /// A quarter turn clockwise.
    Right,
    /// A quarter turn counterclockwise.
    Left,
    /// Mirror left to right.
    FlipHorizontal,
    /// Mirror top to bottom.
    FlipVertical,
}

impl Turn {
    /// Where `p` goes, in an image `w` x `h` points before the turn (top-left
    /// origin).
    pub fn map(self, p: Point, w: f64, h: f64) -> Point {
        match self {
            Turn::Right => Point::new(h - p.y, p.x),
            Turn::Left => Point::new(p.y, w - p.x),
            Turn::FlipHorizontal => Point::new(w - p.x, p.y),
            Turn::FlipVertical => Point::new(p.x, h - p.y),
        }
    }

    pub fn map_rect(self, r: Rect, w: f64, h: f64) -> Rect {
        Rect::from_corners(
            self.map(Point::new(r.x, r.y), w, h),
            self.map(Point::new(r.max_x(), r.max_y()), w, h),
        )
    }

    /// Whether width and height swap.
    pub fn is_quarter(self) -> bool {
        matches!(self, Turn::Right | Turn::Left)
    }
}

/// Mirrored left to right first (if `mirrored`), then turned clockwise by
/// `quarter_turns`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Orientation {
    pub quarter_turns: u8,
    pub mirrored: bool,
}

impl Orientation {
    /// This orientation followed by `turn`.
    pub fn then(self, turn: Turn) -> Self {
        let t = self.quarter_turns % 4;
        match turn {
            Turn::Right => Self {
                quarter_turns: (t + 1) % 4,
                ..self
            },
            Turn::Left => Self {
                quarter_turns: (t + 3) % 4,
                ..self
            },
            // Mirroring after a turn is mirroring first and turning the
            // other way.
            Turn::FlipHorizontal => Self {
                quarter_turns: (4 - t) % 4,
                mirrored: !self.mirrored,
            },
            Turn::FlipVertical => Self {
                quarter_turns: (6 - t) % 4,
                mirrored: !self.mirrored,
            },
        }
    }

    /// The turns that produce this orientation from the original image.
    pub fn turns(self) -> Vec<Turn> {
        let mut turns = Vec::new();
        if self.mirrored {
            turns.push(Turn::FlipHorizontal);
        }
        turns.extend(std::iter::repeat_n(
            Turn::Right,
            usize::from(self.quarter_turns % 4),
        ));
        turns
    }

    /// The turned size of an image `w` x `h`.
    pub fn size(self, w: f64, h: f64) -> (f64, f64) {
        if self.quarter_turns % 2 == 1 {
            (h, w)
        } else {
            (w, h)
        }
    }
}

/// Where the image sits on the output canvas.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CanvasLayout {
    /// Output size in points.
    pub width: f64,
    pub height: f64,
    /// The visible (cropped) image region, in canvas points.
    pub content: Rect,
    /// The visible region in image points (the crop, or the whole image).
    pub source: Rect,
}

impl CanvasLayout {
    /// Image points to canvas points.
    pub fn to_canvas(self, p: Point) -> Point {
        Point::new(
            p.x - self.source.x + self.content.x,
            p.y - self.source.y + self.content.y,
        )
    }

    /// Canvas points to image points.
    pub fn to_image(self, p: Point) -> Point {
        Point::new(
            p.x - self.content.x + self.source.x,
            p.y - self.content.y + self.source.y,
        )
    }
}

impl Document {
    /// Everything there is to show, in image points: the captured image
    /// plus any image layers placed beyond it.
    pub fn extent(&self, image_width: f64, image_height: f64) -> Rect {
        self.extent_in(Rect::new(0.0, 0.0, image_width, image_height))
    }

    /// Like [`Document::extent`], with `image` the part of the captured
    /// image that counts (trimmed margins left out).
    pub fn extent_in(&self, image: Rect) -> Rect {
        self.annotations
            .iter()
            .filter_map(|a| match &a.shape {
                Shape::Image { rect, .. } => Some(*rect),
                _ => None,
            })
            .fold(image, |extent, rect| extent.union(&rect))
    }

    pub fn layout(&self, image_width: f64, image_height: f64) -> CanvasLayout {
        self.layout_in(Rect::new(0.0, 0.0, image_width, image_height))
    }

    /// Like [`Document::layout`], showing only `image` of the captured
    /// image (margins or a window shadow trimmed).
    pub fn layout_in(&self, image: Rect) -> CanvasLayout {
        let full = self.extent_in(image);
        let source = self
            .crop
            .map(|c| c.intersection(&full))
            .filter(|c| c.width >= 1.0 && c.height >= 1.0)
            .unwrap_or(full);
        let Some(bg) = &self.background else {
            return CanvasLayout {
                width: source.width,
                height: source.height,
                content: Rect::new(0.0, 0.0, source.width, source.height),
                source,
            };
        };
        let mut width = source.width + 2.0 * bg.padding;
        let mut height = source.height + 2.0 * bg.padding;
        if let Some(ratio) = bg.aspect.ratio() {
            // Grow one side so the canvas has the requested ratio.
            if width / height < ratio {
                width = height * ratio;
            } else {
                height = width / ratio;
            }
        }
        let (width, height) = (width.round(), height.round());
        CanvasLayout {
            width,
            height,
            content: Rect::new(
                ((width - source.width) / 2.0).round(),
                ((height - source.height) / 2.0).round(),
                source.width,
                source.height,
            ),
            source,
        }
    }

    pub fn add(&mut self, shape: Shape, color: Color, width: f64) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.annotations.push(Annotation {
            id,
            shape,
            color,
            width,
            sketchy: false,
        });
        id
    }

    pub fn get(&self, id: u64) -> Option<&Annotation> {
        self.annotations.iter().find(|a| a.id == id)
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut Annotation> {
        self.annotations.iter_mut().find(|a| a.id == id)
    }

    pub fn remove(&mut self, id: u64) {
        self.annotations.retain(|a| a.id != id);
    }

    /// The topmost annotation at `p` (image points), within `tolerance`.
    pub fn hit(&self, p: Point, tolerance: f64) -> Option<u64> {
        self.annotations
            .iter()
            .rev()
            .find(|a| a.hit(p, tolerance))
            .map(|a| a.id)
    }

    /// Turns everything with the image, which is `w` x `h` points before
    /// the turn. Text stays upright; `turn_picture` turns image layers.
    pub fn turn(&mut self, turn: Turn, w: f64, h: f64, turn_picture: impl Fn(&Picture) -> Picture) {
        let point = |p: Point| turn.map(p, w, h);
        let rect = |r: Rect| turn.map_rect(r, w, h);
        // Moves `r` so its center follows the turn, keeping its size.
        let follow = |r: Rect, width: f64, height: f64| {
            let c = point(Point::new(r.x + r.width / 2.0, r.y + r.height / 2.0));
            Rect::new(c.x - width / 2.0, c.y - height / 2.0, width, height)
        };
        for a in &mut self.annotations {
            match &mut a.shape {
                Shape::Arrow {
                    start, end, bend, ..
                } => {
                    *start = point(*start);
                    *end = point(*end);
                    *bend = bend.map(point);
                }
                Shape::Line { start, end } | Shape::Measure { start, end } => {
                    *start = point(*start);
                    *end = point(*end);
                }
                Shape::Magnify { source, callout } => {
                    *source = rect(*source);
                    *callout = rect(*callout);
                }
                Shape::TextHighlight { rects } => {
                    for r in rects.iter_mut() {
                        *r = rect(*r);
                    }
                }
                Shape::Rectangle { rect: r, .. }
                | Shape::Ellipse { rect: r, .. }
                | Shape::Pixelate { rect: r }
                | Shape::Blur { rect: r }
                | Shape::Spotlight { rect: r } => *r = rect(*r),
                Shape::Pen { points } | Shape::Highlighter { points } => {
                    for p in points.iter_mut() {
                        *p = point(*p);
                    }
                }
                Shape::Text { origin, bounds, .. } => {
                    let moved = follow(*bounds, bounds.width, bounds.height);
                    *origin =
                        Point::new(origin.x + moved.x - bounds.x, origin.y + moved.y - bounds.y);
                    *bounds = moved;
                }
                Shape::Counter { center, .. } => *center = point(*center),
                // Placed anew in the turned picture; a dragged one keeps
                // its spot on the picture.
                Shape::Watermark { free, .. } => {
                    *free = free.map(|f| turn.map(f, 1.0, 1.0));
                }
                Shape::Image {
                    rect: r, picture, ..
                } => {
                    let (width, height) = if turn.is_quarter() {
                        (r.height, r.width)
                    } else {
                        (r.width, r.height)
                    };
                    *r = follow(*r, width, height);
                    *picture = turn_picture(picture);
                }
            }
        }
        self.crop = self.crop.map(rect);
        self.orientation = self.orientation.then(turn);
    }

    /// Places the watermark in `frame`, the visible picture in image points.
    pub fn place_watermarks(&mut self, frame: Rect) {
        for a in &mut self.annotations {
            if let Shape::Watermark {
                style,
                picture,
                free,
                rect,
                frame: placed_in,
            } = &mut a.shape
            {
                *placed_in = frame;
                *rect = crate::watermark::place(frame, picture.size(), style, *free);
            }
        }
    }

    /// The watermark's id, if there is one.
    pub fn watermark(&self) -> Option<u64> {
        self.annotations
            .iter()
            .find(|a| matches!(a.shape, Shape::Watermark { .. }))
            .map(|a| a.id)
    }

    /// The number for the next counter: one more than the highest so far.
    pub fn next_counter(&self) -> u32 {
        self.annotations
            .iter()
            .filter_map(|a| match a.shape {
                Shape::Counter { number, .. } => Some(number),
                _ => None,
            })
            .max()
            .unwrap_or(0)
            + 1
    }
}

/// Grab points of a selected annotation for resizing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    Start,
    End,
    /// The middle of an arrow, dragged to bend it.
    Middle,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Annotation {
    pub fn counter_radius(&self) -> f64 {
        12.0 + self.width * 1.5
    }

    pub fn highlighter_width(&self) -> f64 {
        (self.width * 4.0).max(14.0)
    }

    /// Bounding box in image points.
    pub fn bounds(&self) -> Rect {
        let pad = self.width / 2.0;
        match &self.shape {
            Shape::Arrow {
                start,
                end,
                bend: Some(bend),
                ..
            } => expand(points_bounds(&curve_points(*start, *bend, *end)), pad),
            Shape::Magnify { source, callout } => source.union(callout),
            Shape::Measure { start, end } => expand(Rect::from_corners(*start, *end), MEASURE_TICK),
            Shape::TextHighlight { rects } => rects
                .iter()
                .copied()
                .reduce(|a, b| a.union(&b))
                .unwrap_or_default(),
            Shape::Arrow { start, end, .. } | Shape::Line { start, end } => {
                let r = Rect::from_corners(*start, *end);
                Rect::new(
                    r.x - pad,
                    r.y - pad,
                    r.width + 2.0 * pad,
                    r.height + 2.0 * pad,
                )
            }
            Shape::Rectangle { rect, .. }
            | Shape::Ellipse { rect, .. }
            | Shape::Pixelate { rect }
            | Shape::Blur { rect }
            | Shape::Spotlight { rect }
            | Shape::Image { rect, .. } => *rect,
            Shape::Pen { points } | Shape::Highlighter { points } => points_bounds(points),
            Shape::Text { bounds, .. } => *bounds,
            Shape::Counter { center, .. } => {
                let r = self.counter_radius();
                Rect::new(center.x - r, center.y - r, 2.0 * r, 2.0 * r)
            }
            Shape::Watermark { rect, .. } => *rect,
        }
    }

    pub fn hit(&self, p: Point, tolerance: f64) -> bool {
        let near = tolerance + self.width / 2.0;
        match &self.shape {
            Shape::Arrow {
                start,
                end,
                bend: Some(bend),
                ..
            } => polyline_distance(p, &curve_points(*start, *bend, *end)) <= near,
            Shape::Arrow { start, end, .. } | Shape::Line { start, end } => {
                segment_distance(p, *start, *end) <= near
            }
            Shape::Measure { start, end } => segment_distance(p, *start, *end) <= near + 2.0,
            Shape::Magnify { source, callout } => [source, callout].iter().any(|r| {
                let c = Point::new(r.x + r.width / 2.0, r.y + r.height / 2.0);
                (p.x - c.x).hypot(p.y - c.y) <= r.width / 2.0 + tolerance
            }),
            Shape::TextHighlight { rects } => {
                rects.iter().any(|r| expand(*r, tolerance).contains(p))
            }
            Shape::Rectangle { rect, filled } => {
                if *filled {
                    expand(*rect, near).contains(p)
                } else {
                    expand(*rect, near).contains(p) && !expand(*rect, -near).contains(p)
                }
            }
            Shape::Ellipse { rect, filled } => {
                let (cx, cy) = (rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
                let (rx, ry) = (rect.width / 2.0, rect.height / 2.0);
                if rx <= 0.0 || ry <= 0.0 {
                    return false;
                }
                // Normalized distance from the center; 1.0 is on the edge.
                let d = ((p.x - cx) / rx).hypot((p.y - cy) / ry);
                let edge = near / rx.min(ry);
                if *filled {
                    d <= 1.0 + edge
                } else {
                    (d - 1.0).abs() <= edge
                }
            }
            Shape::Pen { points } => polyline_distance(p, points) <= near,
            Shape::Highlighter { points } => {
                polyline_distance(p, points) <= tolerance + self.highlighter_width() / 2.0
            }
            Shape::Counter { center, .. } => {
                (p.x - center.x).hypot(p.y - center.y) <= self.counter_radius() + tolerance
            }
            Shape::Text { bounds, .. } => expand(*bounds, tolerance).contains(p),
            Shape::Pixelate { rect }
            | Shape::Blur { rect }
            | Shape::Spotlight { rect }
            | Shape::Image { rect, .. }
            | Shape::Watermark { rect, .. } => expand(*rect, tolerance).contains(p),
        }
    }

    pub fn translate(&mut self, dx: f64, dy: f64) {
        let mv = |p: &mut Point| {
            p.x += dx;
            p.y += dy;
        };
        match &mut self.shape {
            Shape::Arrow {
                start, end, bend, ..
            } => {
                mv(start);
                mv(end);
                if let Some(bend) = bend {
                    mv(bend);
                }
            }
            Shape::Line { start, end } | Shape::Measure { start, end } => {
                mv(start);
                mv(end);
            }
            // The magnified spot stays put; its handle moves it.
            Shape::Magnify { callout, .. } => *callout = callout.offset(dx, dy),
            Shape::TextHighlight { rects } => {
                for r in rects.iter_mut() {
                    *r = r.offset(dx, dy);
                }
            }
            Shape::Rectangle { rect, .. }
            | Shape::Ellipse { rect, .. }
            | Shape::Pixelate { rect }
            | Shape::Blur { rect }
            | Shape::Spotlight { rect }
            | Shape::Image { rect, .. } => *rect = rect.offset(dx, dy),
            Shape::Pen { points } | Shape::Highlighter { points } => points.iter_mut().for_each(mv),
            Shape::Text { origin, bounds, .. } => {
                mv(origin);
                *bounds = bounds.offset(dx, dy);
            }
            Shape::Counter { center, .. } => mv(center),
            // Dragging a watermark frees it from its corner.
            Shape::Watermark {
                rect, frame, free, ..
            } => {
                *rect = rect.offset(dx, dy);
                *free = Some(crate::watermark::center_in(*rect, *frame));
            }
        }
    }

    /// Handles shown when selected: endpoints for lines, corners for
    /// rectangular shapes, none for the rest.
    pub fn handles(&self) -> Vec<(Handle, Point)> {
        match &self.shape {
            Shape::Arrow {
                start, end, bend, ..
            } => vec![
                (Handle::Start, *start),
                (Handle::End, *end),
                (Handle::Middle, curve_middle(*start, *bend, *end)),
            ],
            Shape::Line { start, end } | Shape::Measure { start, end } => {
                vec![(Handle::Start, *start), (Handle::End, *end)]
            }
            Shape::Magnify { source, callout } => {
                let r = *callout;
                vec![
                    (Handle::TopLeft, Point::new(r.x, r.y)),
                    (Handle::TopRight, Point::new(r.max_x(), r.y)),
                    (Handle::BottomLeft, Point::new(r.x, r.max_y())),
                    (Handle::BottomRight, Point::new(r.max_x(), r.max_y())),
                    // Move the spot by its center, size it by its edge.
                    (Handle::Start, rect_center(*source)),
                    (
                        Handle::End,
                        Point::new(source.max_x(), source.y + source.height / 2.0),
                    ),
                ]
            }
            Shape::Rectangle { rect, .. }
            | Shape::Ellipse { rect, .. }
            | Shape::Pixelate { rect }
            | Shape::Blur { rect }
            | Shape::Spotlight { rect }
            | Shape::Image { rect, .. }
            | Shape::Watermark { rect, .. } => vec![
                (Handle::TopLeft, Point::new(rect.x, rect.y)),
                (Handle::TopRight, Point::new(rect.max_x(), rect.y)),
                (Handle::BottomLeft, Point::new(rect.x, rect.max_y())),
                (Handle::BottomRight, Point::new(rect.max_x(), rect.max_y())),
            ],
            _ => Vec::new(),
        }
    }

    /// Moves `handle` to `p`.
    pub fn drag_handle(&mut self, handle: Handle, p: Point) {
        // A watermark keeps its shape; its size is a share of the picture's
        // width, and an anchored one stays at its position.
        if let Shape::Watermark {
            style,
            picture,
            free,
            rect,
            frame,
        } = &mut self.shape
        {
            let (w, h) = picture.size();
            let resized = resize_keeping_ratio(*rect, handle, p, w / h.max(f64::MIN_POSITIVE));
            style.size = (resized.width / frame.width.max(1.0) * 100.0).clamp(1.0, 100.0);
            if free.is_some() {
                *free = Some(crate::watermark::center_in(resized, *frame));
            }
            *rect = crate::watermark::place(*frame, (w, h), style, *free);
            return;
        }
        if let Shape::Image { rect, picture, .. } = &mut self.shape {
            let (w, h) = picture.size();
            *rect = resize_keeping_ratio(*rect, handle, p, w / h);
            return;
        }
        if let Shape::Magnify { source, callout } = &mut self.shape {
            match handle {
                Handle::Start => {
                    let c = rect_center(*source);
                    *source = source.offset(p.x - c.x, p.y - c.y);
                }
                Handle::End => {
                    let c = rect_center(*source);
                    let r = (p.x - c.x).hypot(p.y - c.y).max(4.0);
                    *source = Rect::new(c.x - r, c.y - r, 2.0 * r, 2.0 * r);
                }
                Handle::Middle => {}
                corner => *callout = resize_keeping_ratio(*callout, corner, p, 1.0),
            }
            return;
        }
        match (&mut self.shape, handle) {
            (
                Shape::Arrow {
                    start, end, bend, ..
                },
                Handle::Middle,
            ) => {
                *bend = bend_for_handle(*start, *end, p);
            }
            (
                Shape::Arrow { start, .. }
                | Shape::Line { start, .. }
                | Shape::Measure { start, .. },
                Handle::Start,
            ) => *start = p,
            (
                Shape::Arrow { end, .. } | Shape::Line { end, .. } | Shape::Measure { end, .. },
                Handle::End,
            ) => *end = p,
            (
                Shape::Rectangle { rect, .. }
                | Shape::Ellipse { rect, .. }
                | Shape::Pixelate { rect }
                | Shape::Blur { rect }
                | Shape::Spotlight { rect },
                corner,
            ) => {
                // The opposite corner stays put.
                let fixed = match corner {
                    Handle::TopLeft => Point::new(rect.max_x(), rect.max_y()),
                    Handle::TopRight => Point::new(rect.x, rect.max_y()),
                    Handle::BottomLeft => Point::new(rect.max_x(), rect.y),
                    _ => Point::new(rect.x, rect.y),
                };
                *rect = Rect::from_corners(fixed, p);
            }
            _ => {}
        }
    }
}

/// Moves `corner` of `rect` toward `p`, keeping the opposite corner and
/// the width-to-height `ratio`.
fn resize_keeping_ratio(rect: Rect, corner: Handle, p: Point, ratio: f64) -> Rect {
    let fixed = match corner {
        Handle::TopLeft => Point::new(rect.max_x(), rect.max_y()),
        Handle::TopRight => Point::new(rect.x, rect.max_y()),
        Handle::BottomLeft => Point::new(rect.max_x(), rect.y),
        _ => Point::new(rect.x, rect.y),
    };
    // The larger of the two proposed sizes wins, so the corner follows
    // the pointer on whichever axis it moved most. Never smaller than 8.
    let width = (p.x - fixed.x)
        .abs()
        .max((p.y - fixed.y).abs() * ratio)
        .max(8.0);
    let height = width / ratio;
    let x = if p.x < fixed.x {
        fixed.x - width
    } else {
        fixed.x
    };
    let y = if p.y < fixed.y {
        fixed.y - height
    } else {
        fixed.y
    };
    Rect::new(x, y, width, height)
}

/// How far a measurement's end ticks reach from the line.
pub const MEASURE_TICK: f64 = 7.0;

pub fn rect_center(r: Rect) -> Point {
    Point::new(r.x + r.width / 2.0, r.y + r.height / 2.0)
}

/// The point at `t` (0 to 1) on the quadratic curve from `a` to `b` bent
/// toward `c`.
pub fn curve_point(a: Point, c: Point, b: Point, t: f64) -> Point {
    let u = 1.0 - t;
    Point::new(
        u * u * a.x + 2.0 * u * t * c.x + t * t * b.x,
        u * u * a.y + 2.0 * u * t * c.y + t * t * b.y,
    )
}

/// Points along the curve, for hit-testing and bounds.
pub fn curve_points(a: Point, c: Point, b: Point) -> Vec<Point> {
    (0..=32)
        .map(|i| curve_point(a, c, b, f64::from(i) / 32.0))
        .collect()
}

/// Where the bend handle of an arrow sits: the middle of its curve.
pub fn curve_middle(a: Point, bend: Option<Point>, b: Point) -> Point {
    match bend {
        Some(c) => curve_point(a, c, b, 0.5),
        None => Point::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0),
    }
}

/// The control point that puts the middle of the curve at `p`; `None`
/// (straight) when `p` is close to the straight line's middle.
pub fn bend_for_handle(a: Point, b: Point, p: Point) -> Option<Point> {
    let mid = Point::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
    if (p.x - mid.x).hypot(p.y - mid.y) < 4.0 {
        return None;
    }
    // B(0.5) = a/4 + c/2 + b/4.
    Some(Point::new(2.0 * p.x - mid.x, 2.0 * p.y - mid.y))
}

fn expand(r: Rect, by: f64) -> Rect {
    Rect::new(r.x - by, r.y - by, r.width + 2.0 * by, r.height + 2.0 * by)
}

fn points_bounds(points: &[Point]) -> Rect {
    let Some(first) = points.first() else {
        return Rect::default();
    };
    let (mut min, mut max) = (*first, *first);
    for p in points {
        min = Point::new(min.x.min(p.x), min.y.min(p.y));
        max = Point::new(max.x.max(p.x), max.y.max(p.y));
    }
    Rect::from_corners(min, max)
}

/// Distance from `p` to the segment `a`-`b`.
pub fn segment_distance(p: Point, a: Point, b: Point) -> f64 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 {
        0.0
    } else {
        (((p.x - a.x) * dx + (p.y - a.y) * dy) / len2).clamp(0.0, 1.0)
    };
    (p.x - (a.x + t * dx)).hypot(p.y - (a.y + t * dy))
}

fn polyline_distance(p: Point, points: &[Point]) -> f64 {
    match points {
        [] => f64::INFINITY,
        [only] => (p.x - only.x).hypot(p.y - only.y),
        _ => points
            .windows(2)
            .map(|w| segment_distance(p, w[0], w[1]))
            .fold(f64::INFINITY, f64::min),
    }
}

/// Undo and redo as document snapshots. Documents are small (annotation
/// geometry only; the image is not part of them), so snapshots are cheap.
#[derive(Default)]
pub struct History {
    undo: Vec<Document>,
    redo: Vec<Document>,
}

impl History {
    const LIMIT: usize = 200;

    /// Records `before` as the state to return to; call before a change.
    pub fn record(&mut self, before: &Document) {
        self.undo.push(before.clone());
        if self.undo.len() > Self::LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// Forgets the last recorded state, for a change that was abandoned
    /// (for example a click that did not draw anything).
    pub fn discard_last(&mut self) {
        self.undo.pop();
    }

    pub fn undo(&mut self, current: &mut Document) -> bool {
        match self.undo.pop() {
            Some(previous) => {
                self.redo.push(std::mem::replace(current, previous));
                true
            }
            None => false,
        }
    }

    pub fn redo(&mut self, current: &mut Document) -> bool {
        match self.redo.pop() {
            Some(next) => {
                self.undo.push(std::mem::replace(current, next));
                true
            }
            None => false,
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TURNS: [Turn; 4] = [
        Turn::Right,
        Turn::Left,
        Turn::FlipHorizontal,
        Turn::FlipVertical,
    ];

    /// Applies `turns` to `p` one by one in an image `w` x `h`.
    fn apply(turns: &[Turn], mut p: Point, (mut w, mut h): (f64, f64)) -> Point {
        for turn in turns {
            p = turn.map(p, w, h);
            if turn.is_quarter() {
                (w, h) = (h, w);
            }
        }
        p
    }

    #[test]
    fn parses_resize_input() {
        let native = (2000, 1000);
        let same = (2000, 1000);
        assert_eq!(resize_factor("1000", "1000", native, same), Ok(Some(0.5)));
        assert_eq!(resize_factor("2000", "500", native, same), Ok(Some(0.5)));
        assert_eq!(resize_factor("50%", "1000", native, same), Ok(Some(0.5)));
        assert_eq!(
            resize_factor(" 1500 px ", "1000", native, same),
            Ok(Some(0.75))
        );
        assert_eq!(resize_factor("2000", "1000", native, same), Ok(None));
        assert_eq!(resize_factor("100%", "1000", native, same), Ok(None));
        // Already resized to half: unchanged fields keep it.
        assert_eq!(
            resize_factor("1000", "500", native, (1000, 500)),
            Ok(Some(0.5))
        );
        assert_eq!(resize_factor("abc", "1000", native, same), Err(()));
        assert_eq!(resize_factor("0", "1000", native, same), Err(()));
        assert_eq!(resize_factor("-5%", "1000", native, same), Err(()));
    }

    #[test]
    fn turns_map_corners() {
        let (w, h) = (40.0, 10.0);
        let top_left = Point::new(0.0, 0.0);
        assert_eq!(Turn::Right.map(top_left, w, h), Point::new(10.0, 0.0));
        assert_eq!(Turn::Left.map(top_left, w, h), Point::new(0.0, 40.0));
        assert_eq!(
            Turn::FlipHorizontal.map(top_left, w, h),
            Point::new(40.0, 0.0)
        );
        assert_eq!(
            Turn::FlipVertical.map(top_left, w, h),
            Point::new(0.0, 10.0)
        );
        assert_eq!(
            Turn::Right.map_rect(Rect::new(0.0, 0.0, 40.0, 10.0), w, h),
            Rect::new(0.0, 0.0, 10.0, 40.0)
        );
    }

    #[test]
    fn orientations_compose_like_the_turns() {
        let size = (40.0, 10.0);
        let p = Point::new(3.0, 7.0);
        // Every sequence of up to three turns.
        for a in TURNS {
            for b in TURNS {
                for c in TURNS {
                    let sequence = [a, b, c];
                    let o = sequence
                        .iter()
                        .fold(Orientation::default(), |o, t| o.then(*t));
                    assert_eq!(
                        apply(&o.turns(), p, size),
                        apply(&sequence, p, size),
                        "{sequence:?} as {o:?}"
                    );
                    let (w, h) = o.size(size.0, size.1);
                    let quarters = sequence.iter().filter(|t| t.is_quarter()).count();
                    assert_eq!((w, h) == size, quarters % 2 == 0);
                }
            }
        }
        let back = Orientation::default().then(Turn::Right).then(Turn::Left);
        assert_eq!(back, Orientation::default());
    }

    #[test]
    fn turning_moves_annotations_and_keeps_text_upright() {
        let mut doc = Document::default();
        let arrow = doc.add(
            Shape::Arrow {
                start: Point::new(0.0, 0.0),
                end: Point::new(10.0, 5.0),
                style: ArrowStyle::Filled,
                bend: None,
            },
            Color::RED,
            4.0,
        );
        let text = doc.add(
            Shape::Text {
                origin: Point::new(0.0, 0.0),
                text: "Hi".into(),
                size: 24.0,
                style: TextStyle::Plain,
                bounds: Rect::new(0.0, 0.0, 20.0, 10.0),
            },
            Color::RED,
            4.0,
        );
        doc.crop = Some(Rect::new(0.0, 0.0, 40.0, 10.0));
        doc.turn(Turn::Right, 40.0, 10.0, Picture::clone);
        assert_eq!(doc.orientation.quarter_turns, 1);
        assert_eq!(doc.crop, Some(Rect::new(0.0, 0.0, 10.0, 40.0)));
        match &doc.get(arrow).unwrap().shape {
            Shape::Arrow { start, end, .. } => {
                assert_eq!(*start, Point::new(10.0, 0.0));
                assert_eq!(*end, Point::new(5.0, 10.0));
            }
            other => panic!("{other:?}"),
        }
        match &doc.get(text).unwrap().shape {
            // The 20 x 10 box centered at (10, 5) moves to (5, 10).
            Shape::Text { origin, bounds, .. } => {
                assert_eq!(*bounds, Rect::new(-5.0, 5.0, 20.0, 10.0));
                assert_eq!(*origin, Point::new(-5.0, 5.0));
            }
            other => panic!("{other:?}"),
        }
    }

    fn doc_with(shape: Shape) -> (Document, u64) {
        let mut doc = Document::default();
        let id = doc.add(shape, Color::RED, 4.0);
        (doc, id)
    }

    #[test]
    fn hits_lines_near_the_segment_only() {
        let (doc, id) = doc_with(Shape::Arrow {
            start: Point::new(0.0, 0.0),
            end: Point::new(100.0, 0.0),
            style: ArrowStyle::Filled,
            bend: None,
        });
        assert_eq!(doc.hit(Point::new(50.0, 3.0), 2.0), Some(id));
        assert_eq!(doc.hit(Point::new(50.0, 10.0), 2.0), None);
        assert_eq!(doc.hit(Point::new(110.0, 0.0), 2.0), None);
    }

    #[test]
    fn outline_rectangles_hit_on_the_edge_not_inside() {
        let rect = Rect::new(10.0, 10.0, 100.0, 50.0);
        let (doc, id) = doc_with(Shape::Rectangle {
            rect,
            filled: false,
        });
        assert_eq!(doc.hit(Point::new(10.0, 30.0), 2.0), Some(id));
        assert_eq!(doc.hit(Point::new(60.0, 35.0), 2.0), None);
        let (doc, id) = doc_with(Shape::Rectangle { rect, filled: true });
        assert_eq!(doc.hit(Point::new(60.0, 35.0), 2.0), Some(id));
    }

    #[test]
    fn ellipses_hit_on_the_outline() {
        let (doc, id) = doc_with(Shape::Ellipse {
            rect: Rect::new(0.0, 0.0, 100.0, 100.0),
            filled: false,
        });
        assert_eq!(doc.hit(Point::new(50.0, 1.0), 2.0), Some(id));
        assert_eq!(doc.hit(Point::new(50.0, 50.0), 2.0), None);
    }

    #[test]
    fn topmost_annotation_wins() {
        let mut doc = Document::default();
        let rect = Rect::new(0.0, 0.0, 50.0, 50.0);
        doc.add(Shape::Blur { rect }, Color::RED, 4.0);
        let top = doc.add(Shape::Pixelate { rect }, Color::RED, 4.0);
        assert_eq!(doc.hit(Point::new(25.0, 25.0), 0.0), Some(top));
    }

    #[test]
    fn counters_number_upward() {
        let mut doc = Document::default();
        assert_eq!(doc.next_counter(), 1);
        doc.add(
            Shape::Counter {
                center: Point::default(),
                number: 3,
            },
            Color::RED,
            4.0,
        );
        assert_eq!(doc.next_counter(), 4);
    }

    #[test]
    fn translate_and_resize() {
        let (mut doc, id) = doc_with(Shape::Rectangle {
            rect: Rect::new(10.0, 10.0, 20.0, 20.0),
            filled: false,
        });
        let a = doc.get_mut(id).unwrap();
        a.translate(5.0, -5.0);
        assert_eq!(a.bounds(), Rect::new(15.0, 5.0, 20.0, 20.0));
        a.drag_handle(Handle::TopLeft, Point::new(0.0, 0.0));
        assert_eq!(a.bounds(), Rect::new(0.0, 0.0, 35.0, 25.0));
        // Dragging past the fixed corner flips without going negative.
        a.drag_handle(Handle::BottomRight, Point::new(-10.0, -10.0));
        assert_eq!(a.bounds(), Rect::new(-10.0, -10.0, 10.0, 10.0));
    }

    #[test]
    fn layout_with_crop_and_background() {
        let mut doc = Document {
            crop: Some(Rect::new(100.0, 50.0, 200.0, 100.0)),
            ..Default::default()
        };
        let plain = doc.layout(800.0, 600.0);
        assert_eq!((plain.width, plain.height), (200.0, 100.0));
        assert_eq!(
            plain.to_canvas(Point::new(100.0, 50.0)),
            Point::new(0.0, 0.0)
        );

        doc.background = Some(Background {
            padding: 20.0,
            aspect: Aspect::Square,
            ..Default::default()
        });
        let l = doc.layout(800.0, 600.0);
        assert_eq!((l.width, l.height), (240.0, 240.0));
        assert_eq!(l.content, Rect::new(20.0, 70.0, 200.0, 100.0));
        let p = Point::new(150.0, 75.0);
        assert_eq!(l.to_image(l.to_canvas(p)), p);
    }

    #[test]
    fn history_undoes_and_redoes() {
        let mut history = History::default();
        let mut doc = Document::default();
        history.record(&doc);
        doc.add(
            Shape::Line {
                start: Point::default(),
                end: Point::new(1.0, 1.0),
            },
            Color::RED,
            2.0,
        );
        assert!(history.undo(&mut doc));
        assert!(doc.annotations.is_empty());
        assert!(history.redo(&mut doc));
        assert_eq!(doc.annotations.len(), 1);
        assert!(!history.redo(&mut doc));
    }

    #[test]
    fn documents_round_trip_through_json() {
        let (mut doc, _) = doc_with(Shape::Text {
            origin: Point::new(1.0, 2.0),
            text: "Hi".into(),
            size: 24.0,
            style: TextStyle::Background,
            bounds: Rect::new(1.0, 2.0, 30.0, 28.0),
        });
        doc.background = Some(Background::default());
        let json = serde_json::to_string(&doc).unwrap();
        assert_eq!(serde_json::from_str::<Document>(&json).unwrap(), doc);
    }
}
