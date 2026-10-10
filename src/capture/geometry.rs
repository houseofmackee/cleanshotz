//! Pure geometry for the selection overlay. Coordinates are display-local
//! points with a top-left origin (y grows downward), the same space
//! ScreenCaptureKit uses for `sourceRect`.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn from_corners(a: Point, b: Point) -> Self {
        Self::new(
            a.x.min(b.x),
            a.y.min(b.y),
            (a.x - b.x).abs(),
            (a.y - b.y).abs(),
        )
    }

    pub fn max_x(&self) -> f64 {
        self.x + self.width
    }

    pub fn max_y(&self) -> f64 {
        self.y + self.height
    }

    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x < self.max_x() && p.y >= self.y && p.y < self.max_y()
    }

    pub fn intersection(&self, other: &Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let max_x = self.max_x().min(other.max_x());
        let max_y = self.max_y().min(other.max_y());
        Rect::new(x, y, (max_x - x).max(0.0), (max_y - y).max(0.0))
    }

    /// The smallest rect containing both.
    pub fn union(&self, other: &Rect) -> Rect {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let max_x = self.max_x().max(other.max_x());
        let max_y = self.max_y().max(other.max_y());
        Rect::new(x, y, max_x - x, max_y - y)
    }

    pub fn offset(&self, dx: f64, dy: f64) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.width, self.height)
    }

    /// Rounds the edges to whole points so selections map to whole pixels
    /// on 1x and 2x displays.
    pub fn rounded(&self) -> Rect {
        let x = self.x.round();
        let y = self.y.round();
        Rect::new(x, y, self.max_x().round() - x, self.max_y().round() - y)
    }

    pub fn is_usable(&self) -> bool {
        self.width >= 2.0 && self.height >= 2.0
    }
}

/// The selection for a drag from `anchor` to `current`.
///
/// - `square`: constrain to a square (Shift).
/// - `from_center`: `anchor` is the center instead of a corner (Option).
///
/// The result is rounded to whole points and clipped to `bounds`.
pub fn selection_rect(
    anchor: Point,
    current: Point,
    square: bool,
    from_center: bool,
    bounds: Rect,
) -> Rect {
    let mut dx = current.x - anchor.x;
    let mut dy = current.y - anchor.y;
    if square {
        let side = dx.abs().max(dy.abs());
        dx = side.copysign(dx);
        dy = side.copysign(dy);
    }
    let rect = if from_center {
        Rect::new(
            anchor.x - dx.abs(),
            anchor.y - dy.abs(),
            dx.abs() * 2.0,
            dy.abs() * 2.0,
        )
    } else {
        Rect::from_corners(anchor, Point::new(anchor.x + dx, anchor.y + dy))
    };
    rect.rounded().intersection(&bounds)
}

/// Moves `rect` by (`dx`, `dy`) but keeps it fully inside `bounds`.
pub fn move_within(rect: Rect, dx: f64, dy: f64, bounds: Rect) -> Rect {
    let x = (rect.x + dx).clamp(bounds.x, (bounds.max_x() - rect.width).max(bounds.x));
    let y = (rect.y + dy).clamp(bounds.y, (bounds.max_y() - rect.height).max(bounds.y));
    Rect::new(x, y, rect.width, rect.height)
}

/// Top-left corner for a `size` x `size` panel (the magnifier) next to the
/// cursor: below-right by default, flipped to the other side of the cursor
/// on an axis where it would leave `bounds`.
pub fn panel_origin(cursor: Point, size: f64, gap: f64, bounds: Rect) -> Point {
    let x = if cursor.x + gap + size <= bounds.max_x() {
        cursor.x + gap
    } else {
        cursor.x - gap - size
    };
    let y = if cursor.y + gap + size <= bounds.max_y() {
        cursor.y + gap
    } else {
        cursor.y - gap - size
    };
    Point::new(x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Rect = Rect {
        x: 0.0,
        y: 0.0,
        width: 1000.0,
        height: 800.0,
    };

    #[test]
    fn drag_in_any_direction_normalizes() {
        let r = selection_rect(
            Point::new(100.0, 100.0),
            Point::new(40.0, 60.0),
            false,
            false,
            SCREEN,
        );
        assert_eq!(r, Rect::new(40.0, 60.0, 60.0, 40.0));
    }

    #[test]
    fn square_uses_longer_side_and_keeps_direction() {
        let r = selection_rect(
            Point::new(100.0, 100.0),
            Point::new(130.0, 50.0),
            true,
            false,
            SCREEN,
        );
        assert_eq!(r, Rect::new(100.0, 50.0, 50.0, 50.0));
    }

    #[test]
    fn from_center_grows_both_ways() {
        let r = selection_rect(
            Point::new(100.0, 100.0),
            Point::new(110.0, 120.0),
            false,
            true,
            SCREEN,
        );
        assert_eq!(r, Rect::new(90.0, 80.0, 20.0, 40.0));
    }

    #[test]
    fn clipped_to_bounds_and_rounded() {
        let r = selection_rect(
            Point::new(990.4, 10.6),
            Point::new(1200.0, -50.0),
            false,
            false,
            SCREEN,
        );
        assert_eq!(r, Rect::new(990.0, 0.0, 10.0, 11.0));
    }

    #[test]
    fn move_stays_inside() {
        let r = Rect::new(10.0, 10.0, 100.0, 100.0);
        assert_eq!(
            move_within(r, -50.0, 0.0, SCREEN),
            Rect::new(0.0, 10.0, 100.0, 100.0)
        );
        assert_eq!(
            move_within(r, 2000.0, 2000.0, SCREEN),
            Rect::new(900.0, 700.0, 100.0, 100.0)
        );
    }

    #[test]
    fn panel_flips_near_edges() {
        assert_eq!(
            panel_origin(Point::new(10.0, 10.0), 100.0, 20.0, SCREEN),
            Point::new(30.0, 30.0)
        );
        assert_eq!(
            panel_origin(Point::new(950.0, 790.0), 100.0, 20.0, SCREEN),
            Point::new(830.0, 670.0)
        );
    }

    #[test]
    fn tiny_rects_are_not_usable() {
        assert!(!Rect::new(0.0, 0.0, 1.0, 50.0).is_usable());
        assert!(Rect::new(0.0, 0.0, 2.0, 2.0).is_usable());
    }
}
