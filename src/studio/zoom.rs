//! Zooms: stretches of a Studio recording shown closer up. Automatic ones
//! come from clicks and typing that happen close together; each can be
//! moved, resized and changed. While zoomed, a "camera" follows the
//! (smoothed) cursor with a dead zone and never leaves the recording.
//! Everything here is pure.

use serde::{Deserialize, Serialize};

use super::events::Events;
use crate::capture::geometry::{Point, Rect};

/// Zooming in or out takes this long.
pub const EASE: f64 = 0.6;
/// Automatic zooms start this long before the first event...
const LEAD: f64 = 0.6;
/// ...and end this long after the last.
const TAIL: f64 = 1.5;
/// Events further apart than this start a new zoom.
const GAP: f64 = 2.5;
pub const MIN_LEVEL: f64 = 1.25;
pub const MAX_LEVEL: f64 = 4.0;
const MAX_AUTO_LEVEL: f64 = 2.5;
/// The camera aims at where the cursor will be this much later, so it
/// gets there as things happen rather than after (the whole recording is
/// known when rendering).
const LOOK_AHEAD: f64 = 0.4;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Focus {
    /// Follow the cursor.
    Cursor,
    /// Stay on this point of the recorded area (points).
    Fixed { x: f64, y: f64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Zoom {
    pub start: f64,
    pub end: f64,
    /// 1.25 to 4 times closer.
    pub level: f64,
    pub focus: Focus,
    /// Made automatically (replaced when they are regenerated).
    #[serde(default)]
    pub auto: bool,
}

impl Zoom {
    /// How far zoomed in at `t`: 0 outside, easing to 1 inside.
    pub fn amount(&self, t: f64) -> f64 {
        if t <= self.start || t >= self.end {
            return 0.0;
        }
        let ramp = EASE.min((self.end - self.start) / 2.0);
        let into = ((t - self.start) / ramp).min(1.0);
        let out = ((self.end - t) / ramp).min(1.0);
        smoothstep(into.min(out))
    }
}

fn smoothstep(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// Automatic zooms for `events` in a recorded area `size` points large.
pub fn automatic(events: &Events, size: (f64, f64), duration: f64) -> Vec<Zoom> {
    // Clicks, and keys at the cursor's position when they were typed.
    let mut points: Vec<(f64, Point)> = events
        .clicks
        .iter()
        .filter(|c| c.down)
        .map(|c| (c.t, Point::new(c.x, c.y)))
        .collect();
    for key in &events.keys {
        let at = events
            .cursor
            .iter()
            .rev()
            .find(|s| s.t <= key.t)
            .or(events.cursor.first());
        if let Some(s) = at {
            points.push((key.t, Point::new(s.x, s.y)));
        }
    }
    points.sort_by(|a, b| a.0.total_cmp(&b.0));
    let reach = size.0.hypot(size.1) / 3.0;
    let mut groups: Vec<Vec<(f64, Point)>> = Vec::new();
    for (t, p) in points {
        let joins = groups.last().is_some_and(|g| {
            let (last, _) = g[g.len() - 1];
            let c = center(g);
            t - last <= GAP && (p.x - c.x).hypot(p.y - c.y) <= reach
        });
        if joins {
            groups.last_mut().expect("checked above").push((t, p));
        } else {
            groups.push(vec![(t, p)]);
        }
    }
    let mut zooms: Vec<Zoom> = groups
        .iter()
        .map(|g| {
            let bounds = g
                .iter()
                .map(|(_, p)| Rect::new(p.x, p.y, 0.0, 0.0))
                .reduce(|a, b| a.union(&b))
                .unwrap_or_default();
            // The group's area, with some room around it, fills about 60%
            // of the frame.
            let (w, h) = (bounds.width + 200.0, bounds.height + 140.0);
            let level = ((size.0 / w).min(size.1 / h) * 0.6).clamp(MIN_LEVEL, MAX_AUTO_LEVEL);
            Zoom {
                start: (g[0].0 - LEAD).max(0.0),
                end: (g[g.len() - 1].0 + TAIL).min(duration.max(0.0)),
                level,
                focus: Focus::Cursor,
                auto: true,
            }
        })
        .filter(|z| z.end - z.start >= 1.0)
        .collect();
    merge(&mut zooms);
    zooms
}

fn center(group: &[(f64, Point)]) -> Point {
    let n = group.len() as f64;
    let (x, y) = group
        .iter()
        .fold((0.0, 0.0), |(x, y), (_, p)| (x + p.x, y + p.y));
    Point::new(x / n, y / n)
}

/// Joins zooms that overlap or nearly touch (less than half a second
/// apart), keeping the gentler level.
pub fn merge(zooms: &mut Vec<Zoom>) {
    zooms.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut merged: Vec<Zoom> = Vec::with_capacity(zooms.len());
    for z in zooms.drain(..) {
        match merged.last_mut() {
            Some(last) if z.start - last.end < 0.5 => {
                last.end = last.end.max(z.end);
                last.level = last.level.min(z.level);
            }
            _ => merged.push(z),
        }
    }
    *zooms = merged;
}

/// Where the view is at each moment: how close (1 is the whole
/// recording) and the point of the recorded area at its center.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub level: f64,
    pub center: Point,
}

/// The camera's path through a recording, worked out once at a fixed rate.
#[derive(Clone, Debug, Default)]
pub struct Camera {
    rate: f64,
    views: Vec<View>,
}

impl Camera {
    /// `cursor(t)` gives the smoothed cursor position.
    pub fn new(
        zooms: &[Zoom],
        size: (f64, f64),
        duration: f64,
        rate: f64,
        cursor: impl Fn(f64) -> Option<Point>,
    ) -> Self {
        let whole = Point::new(size.0 / 2.0, size.1 / 2.0);
        let count = (duration.max(0.0) * rate).ceil() as usize + 1;
        let mut views = Vec::with_capacity(count);
        let mut center = whole;
        let mut velocity = Point::new(0.0, 0.0);
        let dt = 1.0 / rate;
        // A softer spring than the cursor's, so the view drifts gently.
        let w = 5.0;
        for n in 0..count {
            let t = n as f64 / rate;
            let active = zooms
                .iter()
                .map(|z| (z, z.amount(t)))
                .filter(|(_, a)| *a > 0.0)
                .max_by(|a, b| a.1.total_cmp(&b.1));
            let (level, target) = match active {
                Some((zoom, amount)) => {
                    let level = 1.0 + (zoom.level.clamp(MIN_LEVEL, MAX_LEVEL) - 1.0) * amount;
                    let target = match zoom.focus {
                        Focus::Fixed { x, y } => Point::new(x, y),
                        Focus::Cursor => cursor(t + LOOK_AHEAD).or(cursor(t)).unwrap_or(center),
                    };
                    (level, target)
                }
                None => (1.0, whole),
            };
            // Dead zone: the cursor may move within the middle part of the
            // view without the view following.
            let (half_w, half_h) = (size.0 / level / 2.0, size.1 / level / 2.0);
            let goal = if level > 1.0 + 1e-6 {
                let (dx, dy) = (target.x - center.x, target.y - center.y);
                let (zone_w, zone_h) = (half_w * 0.4, half_h * 0.4);
                Point::new(
                    center.x + dx.signum() * (dx.abs() - zone_w).max(0.0),
                    center.y + dy.signum() * (dy.abs() - zone_h).max(0.0),
                )
            } else {
                whole
            };
            let ax = w * w * (goal.x - center.x) - 2.0 * w * velocity.x;
            let ay = w * w * (goal.y - center.y) - 2.0 * w * velocity.y;
            velocity = Point::new(velocity.x + ax * dt, velocity.y + ay * dt);
            center = Point::new(center.x + velocity.x * dt, center.y + velocity.y * dt);
            // Never past the edges of the recording.
            let clamped = Point::new(
                center.x.clamp(half_w, size.0 - half_w),
                center.y.clamp(half_h, size.1 - half_h),
            );
            if clamped != center {
                velocity = Point::new(0.0, 0.0);
                center = clamped;
            }
            views.push(View { level, center });
        }
        Self { rate, views }
    }

    pub fn view(&self, t: f64) -> Option<View> {
        if self.views.is_empty() {
            return None;
        }
        let x = (t * self.rate).clamp(0.0, (self.views.len() - 1) as f64);
        let (i, f) = (x.floor() as usize, x.fract());
        let a = self.views[i];
        let b = self.views[(i + 1).min(self.views.len() - 1)];
        Some(View {
            level: a.level + (b.level - a.level) * f,
            center: Point::new(
                a.center.x + (b.center.x - a.center.x) * f,
                a.center.y + (b.center.y - a.center.y) * f,
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::studio::events::{Button, Click, CursorSample, KeyPress};

    fn click(t: f64, x: f64, y: f64) -> Click {
        Click {
            t,
            x,
            y,
            button: Button::Left,
            down: true,
        }
    }

    #[test]
    fn eases_in_and_out() {
        let zoom = Zoom {
            start: 1.0,
            end: 4.0,
            level: 2.0,
            focus: Focus::Cursor,
            auto: false,
        };
        let near = |a: f64, b: f64| (a - b).abs() < 1e-9;
        assert_eq!(zoom.amount(0.5), 0.0);
        assert!(near(zoom.amount(1.3), 0.5));
        assert_eq!(zoom.amount(2.5), 1.0);
        assert!(near(zoom.amount(3.7), 0.5));
        assert_eq!(zoom.amount(4.0), 0.0);
    }

    #[test]
    fn groups_events_into_zooms() {
        let events = Events {
            clicks: vec![
                click(2.0, 100.0, 100.0),
                click(3.0, 150.0, 120.0),
                // Far away in time: a second zoom.
                click(10.0, 900.0, 600.0),
                // Close in time but across the screen: its own group, but
                // overlapping in time, so merged into the second (the view
                // follows the cursor there).
                click(11.0, 50.0, 50.0),
            ],
            keys: vec![KeyPress {
                t: 4.0,
                label: "⌘S".into(),
            }],
            cursor: vec![CursorSample {
                t: 3.5,
                x: 160.0,
                y: 130.0,
                image: 0,
            }],
            ..Events::default()
        };
        let zooms = automatic(&events, (1000.0, 700.0), 20.0);
        assert_eq!(zooms.len(), 2, "{zooms:?}");
        assert!((zooms[0].start - 1.4).abs() < 1e-9);
        assert!(
            (zooms[0].end - 5.5).abs() < 1e-9,
            "the key at 4 s extends it"
        );
        assert!((zooms[1].start - 9.4).abs() < 1e-9 && (zooms[1].end - 12.5).abs() < 1e-9);
        assert!(
            zooms
                .iter()
                .all(|z| z.level >= MIN_LEVEL && z.level <= 2.5 && z.auto)
        );
    }

    #[test]
    fn merges_overlapping_zooms() {
        let z = |start, end, level| Zoom {
            start,
            end,
            level,
            focus: Focus::Cursor,
            auto: true,
        };
        let mut zooms = vec![z(5.0, 8.0, 2.0), z(1.0, 3.0, 2.5), z(3.2, 4.0, 1.5)];
        merge(&mut zooms);
        assert_eq!(zooms, vec![z(1.0, 4.0, 1.5), z(5.0, 8.0, 2.0)]);
    }

    #[test]
    fn the_camera_follows_the_cursor_within_the_recording() {
        let zooms = [Zoom {
            start: 0.0,
            end: 10.0,
            level: 2.0,
            focus: Focus::Cursor,
            auto: false,
        }];
        // The cursor sits in the top-left corner.
        let camera = Camera::new(&zooms, (1000.0, 600.0), 10.0, 60.0, |_| {
            Some(Point::new(10.0, 10.0))
        });
        let view = camera.view(5.0).unwrap();
        assert_eq!(view.level, 2.0);
        // The view (500 × 300) stays inside: its center at least 250, 150.
        assert!((view.center.x - 250.0).abs() < 1.0 && (view.center.y - 150.0).abs() < 1.0);
        // Outside a zoom: the whole recording, centered.
        let none = Camera::new(&[], (1000.0, 600.0), 2.0, 60.0, |_| None);
        assert_eq!(
            none.view(1.0).unwrap(),
            View {
                level: 1.0,
                center: Point::new(500.0, 300.0)
            }
        );
    }

    #[test]
    fn a_fixed_focus_stays_put() {
        let zooms = [Zoom {
            start: 0.0,
            end: 10.0,
            level: 2.0,
            focus: Focus::Fixed { x: 500.0, y: 300.0 },
            auto: false,
        }];
        let camera = Camera::new(&zooms, (1000.0, 600.0), 10.0, 60.0, |_| {
            Some(Point::new(0.0, 0.0))
        });
        let view = camera.view(6.0).unwrap();
        assert!((view.center.x - 500.0).abs() < 1.0 && (view.center.y - 300.0).abs() < 1.0);
    }
}
