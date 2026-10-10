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
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Annotation {
    pub id: u64,
    pub shape: Shape,
    pub color: Color,
    /// Stroke width in points.
    pub width: f64,
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
    /// 16:9
    Wide,
    /// 4:3
    Standard,
}

impl Aspect {
    fn ratio(self) -> Option<f64> {
        match self {
            Aspect::Auto => None,
            Aspect::Square => Some(1.0),
            Aspect::Wide => Some(16.0 / 9.0),
            Aspect::Standard => Some(4.0 / 3.0),
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
        self.annotations
            .iter()
            .filter_map(|a| match &a.shape {
                Shape::Image { rect, .. } => Some(*rect),
                _ => None,
            })
            .fold(
                Rect::new(0.0, 0.0, image_width, image_height),
                |extent, rect| extent.union(&rect),
            )
    }

    pub fn layout(&self, image_width: f64, image_height: f64) -> CanvasLayout {
        let full = self.extent(image_width, image_height);
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
        }
    }

    pub fn hit(&self, p: Point, tolerance: f64) -> bool {
        let near = tolerance + self.width / 2.0;
        match &self.shape {
            Shape::Arrow { start, end, .. } | Shape::Line { start, end } => {
                segment_distance(p, *start, *end) <= near
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
            | Shape::Image { rect, .. } => expand(*rect, tolerance).contains(p),
        }
    }

    pub fn translate(&mut self, dx: f64, dy: f64) {
        let mv = |p: &mut Point| {
            p.x += dx;
            p.y += dy;
        };
        match &mut self.shape {
            Shape::Arrow { start, end, .. } | Shape::Line { start, end } => {
                mv(start);
                mv(end);
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
        }
    }

    /// Handles shown when selected: endpoints for lines, corners for
    /// rectangular shapes, none for the rest.
    pub fn handles(&self) -> Vec<(Handle, Point)> {
        match &self.shape {
            Shape::Arrow { start, end, .. } | Shape::Line { start, end } => {
                vec![(Handle::Start, *start), (Handle::End, *end)]
            }
            Shape::Rectangle { rect, .. }
            | Shape::Ellipse { rect, .. }
            | Shape::Pixelate { rect }
            | Shape::Blur { rect }
            | Shape::Spotlight { rect }
            | Shape::Image { rect, .. } => vec![
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
        if let Shape::Image { rect, picture } = &mut self.shape {
            let (w, h) = picture.size();
            *rect = resize_keeping_ratio(*rect, handle, p, w / h);
            return;
        }
        match (&mut self.shape, handle) {
            (Shape::Arrow { start, .. } | Shape::Line { start, .. }, Handle::Start) => *start = p,
            (Shape::Arrow { end, .. } | Shape::Line { end, .. }, Handle::End) => *end = p,
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
