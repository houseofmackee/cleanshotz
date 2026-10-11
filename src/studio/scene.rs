//! What a Studio frame shows at a given time, as plain data: where the
//! recording sits on the canvas, where the (smoothed) cursor is and how
//! visible, and which click effects are running. The compositor draws it;
//! everything here is pure and tested without AVFoundation.
//!
//! Units: video pixels for the recording, canvas pixels for the output
//! (both top-left origin).

use super::events::{Click, CursorSample, Events};
use super::project::{CursorStyle, Project};
use super::zoom::{Camera, View};
use crate::capture::geometry::{Point, Rect};
use crate::editor::model::Background;

/// Cursor positions are resampled at this rate before smoothing.
const RATE: f64 = 120.0;
/// Idle this long, the cursor fades out (when hiding is on)...
const IDLE: f64 = 2.0;
/// ...over this long, and fades back in this long before it moves again.
const FADE_OUT: f64 = 0.4;
const FADE_IN: f64 = 0.15;
/// How long a click effect lasts.
pub const CLICK: f64 = 0.5;

/// The canvas and where the recording sits on it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub canvas: (f64, f64),
    pub video: Rect,
    pub radius: f64,
    pub shadow: bool,
}

/// The canvas for a recording `video` pixels large, `scale` pixels per
/// point: the image editor's layout for the same background (padding and
/// corners in points, grown to the aspect ratio), so the background drawn
/// by the image editor lines up with the video exactly.
pub fn layout(video: (f64, f64), background: Option<&Background>, scale: f64) -> Layout {
    let doc = crate::editor::model::Document {
        background: background.cloned(),
        ..Default::default()
    };
    let l = doc.layout(video.0 / scale, video.1 / scale);
    Layout {
        canvas: (
            (l.width * scale / 2.0).round() * 2.0,
            (l.height * scale / 2.0).round() * 2.0,
        ),
        video: Rect::new(l.content.x * scale, l.content.y * scale, video.0, video.1),
        radius: background.map_or(0.0, |b| b.corner_radius.max(0.0) * scale),
        shadow: background.is_some_and(|b| b.shadow),
    }
}

/// The cursor's path through the recording: resampled, smoothed, passing
/// exactly through every click.
#[derive(Clone, Debug, Default)]
pub struct CursorPath {
    start: f64,
    /// Positions every 1/RATE seconds from `start`.
    points: Vec<Point>,
    /// (time, image) whenever the image changes.
    images: Vec<(f64, u32)>,
    /// Times the cursor moved (or clicked), for idle hiding.
    moves: Vec<f64>,
}

impl CursorPath {
    pub fn new(samples: &[CursorSample], clicks: &[Click], smoothing: f64) -> Self {
        let (Some(first), Some(last)) = (samples.first(), samples.last()) else {
            return Self::default();
        };
        let (start, end) = (first.t, last.t);
        let count = ((end - start) * RATE).floor() as usize + 1;
        // Resample by linear interpolation.
        let mut raw = Vec::with_capacity(count);
        let mut i = 0;
        for n in 0..count {
            let t = start + n as f64 / RATE;
            while i + 1 < samples.len() && samples[i + 1].t <= t {
                i += 1;
            }
            let a = samples[i];
            let b = samples[(i + 1).min(samples.len() - 1)];
            let f = if b.t > a.t {
                ((t - a.t) / (b.t - a.t)).clamp(0.0, 1.0)
            } else {
                0.0
            };
            raw.push(Point::new(a.x + (b.x - a.x) * f, a.y + (b.y - a.y) * f));
        }
        // Clicks are fixed points: the smoothed cursor gets there by the
        // time of the click (blended in over about 0.15 s before it) and
        // carries on from there. A press and its release at the same spot
        // count once.
        let mut anchors: Vec<(usize, Point)> = Vec::new();
        if smoothing > 0.0 {
            let mut sorted: Vec<&Click> = clicks.iter().collect();
            sorted.sort_by(|a, b| a.t.total_cmp(&b.t));
            let mut last: Option<(f64, Point)> = None;
            for c in sorted {
                let p = Point::new(c.x, c.y);
                let repeat =
                    last.is_some_and(|(t, q)| c.t - t < 0.2 && (p.x - q.x).hypot(p.y - q.y) < 2.0);
                if !repeat && c.t >= start {
                    let n = ((c.t - start) * RATE).round() as usize;
                    anchors.push((n.min(count - 1), p));
                    last = Some((c.t, p));
                }
            }
        }
        let mut points = smooth(&raw, smoothing, &anchors);
        let lead = (0.12 * RATE) as usize * 2;
        for &(at, target) in &anchors {
            let from = at.saturating_sub(lead);
            for (n, p) in points.iter_mut().enumerate().take(at).skip(from) {
                let dt = (at - n) as f64 / RATE;
                let w = (-(dt / 0.12).powi(2)).exp();
                p.x += (target.x - p.x) * w;
                p.y += (target.y - p.y) * w;
            }
        }
        let mut images: Vec<(f64, u32)> = Vec::new();
        for s in samples {
            if images.last().is_none_or(|(_, image)| *image != s.image) {
                images.push((s.t, s.image));
            }
        }
        let mut moves: Vec<f64> = samples
            .windows(2)
            .filter(|w| (w[1].x - w[0].x).hypot(w[1].y - w[0].y) > 0.5)
            .map(|w| w[1].t)
            .chain(clicks.iter().map(|c| c.t))
            .collect();
        moves.sort_by(f64::total_cmp);
        Self {
            start,
            points,
            images,
            moves,
        }
    }

    /// The (smoothed) position at `t`, in points of the recorded area.
    pub fn position(&self, t: f64) -> Option<Point> {
        if self.points.is_empty() {
            return None;
        }
        let x = ((t - self.start) * RATE).clamp(0.0, (self.points.len() - 1) as f64);
        let (i, f) = (x.floor() as usize, x.fract());
        let a = self.points[i];
        let b = self.points[(i + 1).min(self.points.len() - 1)];
        Some(Point::new(a.x + (b.x - a.x) * f, a.y + (b.y - a.y) * f))
    }

    /// The cursor image at `t`.
    pub fn image(&self, t: f64) -> u32 {
        let i = self.images.partition_point(|(at, _)| *at <= t);
        self.images
            .get(i.saturating_sub(1))
            .map_or(0, |(_, image)| *image)
    }

    /// 0 (hidden) to 1: fades out after the cursor rests for a while and
    /// back in just before it moves.
    pub fn idle_opacity(&self, t: f64) -> f64 {
        let i = self.moves.partition_point(|m| *m <= t);
        let last = if i == 0 {
            self.start
        } else {
            self.moves[i - 1]
        };
        let out = 1.0 - ((t - last - IDLE) / FADE_OUT).clamp(0.0, 1.0);
        let back = self
            .moves
            .get(i)
            .map_or(0.0, |next| 1.0 - ((next - t) / FADE_IN).clamp(0.0, 1.0));
        out.max(back)
    }
}

/// Smooths a path sampled at RATE with a critically damped spring
/// (no overshoot); 0 leaves it as it is. At each anchor (sample index and
/// position) the spring is put on the position and stopped.
fn smooth(points: &[Point], smoothing: f64, anchors: &[(usize, Point)]) -> Vec<Point> {
    let smoothing = smoothing.clamp(0.0, 1.0);
    if smoothing <= 0.0 || points.is_empty() {
        return points.to_vec();
    }
    // Stiffness: from snappy (60) to floaty (5).
    let w = 60.0 * (5.0_f64 / 60.0).powf(smoothing);
    let steps = 8;
    let dt = 1.0 / RATE / steps as f64;
    let (mut p, mut v) = (points[0], Point::new(0.0, 0.0));
    let mut out = Vec::with_capacity(points.len());
    for (n, target) in points.iter().enumerate() {
        if let Some((_, anchor)) = anchors.iter().find(|(at, _)| *at == n) {
            p = *anchor;
            v = Point::new(0.0, 0.0);
            out.push(p);
            continue;
        }
        for _ in 0..steps {
            let ax = w * w * (target.x - p.x) - 2.0 * w * v.x;
            let ay = w * w * (target.y - p.y) - 2.0 * w * v.y;
            v = Point::new(v.x + ax * dt, v.y + ay * dt);
            p = Point::new(p.x + v.x * dt, p.y + v.y * dt);
        }
        out.push(p);
    }
    out
}

/// One frame's cursor, in canvas pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CursorDraw {
    pub image: u32,
    /// Where the hotspot goes.
    pub tip: Point,
    /// Canvas pixels per point of the cursor image.
    pub scale: f64,
    pub opacity: f64,
}

/// A running click effect, in canvas pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClickDraw {
    pub at: Point,
    /// 0 when the click happens, 1 when the effect ends.
    pub progress: f64,
    /// Canvas pixels per point, to size the effect.
    pub scale: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    /// How close the view is and where it looks.
    pub view: View,
    /// The view a frame earlier, for motion blur.
    pub previous: View,
    pub cursor: Option<CursorDraw>,
    pub clicks: Vec<ClickDraw>,
}

/// Everything needed to describe any frame of a recording.
#[derive(Clone, Debug)]
pub struct Scene {
    pub layout: Layout,
    /// Video pixels per point of the recorded area.
    pub scale: f64,
    pub style: CursorStyle,
    path: CursorPath,
    clicks: Vec<Click>,
    camera: Camera,
    /// The recorded area's size in points.
    area: (f64, f64),
    pub motion_blur: bool,
}

impl Scene {
    /// `video`: the screen video's size in pixels; `duration` its length.
    pub fn new(events: &Events, project: &Project, video: (f64, f64), duration: f64) -> Self {
        let scale = if events.scale > 0.0 {
            events.scale
        } else {
            1.0
        };
        let path = CursorPath::new(&events.cursor, &events.clicks, project.cursor.smoothing);
        let area = (video.0 / scale, video.1 / scale);
        let camera = Camera::new(&project.zooms, area, duration, 60.0, |t| path.position(t));
        Self {
            layout: layout(video, project.background.as_ref(), scale),
            scale,
            style: project.cursor,
            path,
            clicks: events.clicks.iter().filter(|c| c.down).copied().collect(),
            camera,
            area,
            motion_blur: project.motion_blur,
        }
    }

    /// The view at `t` (the whole recording when there is no zoom).
    pub fn view(&self, t: f64) -> View {
        self.camera.view(t).unwrap_or(View {
            level: 1.0,
            center: Point::new(self.area.0 / 2.0, self.area.1 / 2.0),
        })
    }

    /// A point of the recorded area on the canvas, through `view`.
    pub fn to_canvas(&self, p: Point, view: View) -> Point {
        let left = view.center.x - self.area.0 / view.level / 2.0;
        let top = view.center.y - self.area.1 / view.level / 2.0;
        Point::new(
            self.layout.video.x + (p.x - left) * view.level * self.scale,
            self.layout.video.y + (p.y - top) * view.level * self.scale,
        )
    }

    pub fn frame(&self, t: f64) -> Frame {
        let style = self.style;
        let view = self.view(t);
        let previous = self.view(t - 1.0 / 60.0);
        let cursor = (style.visible)
            .then(|| self.path.position(t))
            .flatten()
            .map(|p| CursorDraw {
                image: self.path.image(t),
                tip: self.to_canvas(p, view),
                scale: self.scale * style.size.clamp(0.5, 3.0) * view.level,
                opacity: if style.hide_when_idle {
                    self.path.idle_opacity(t)
                } else {
                    1.0
                },
            })
            .filter(|c| c.opacity > 0.0);
        let clicks = if style.clicks == super::project::ClickEffect::None {
            Vec::new()
        } else {
            self.clicks
                .iter()
                .filter(|c| t >= c.t && t < c.t + CLICK)
                .map(|c| ClickDraw {
                    at: self.to_canvas(Point::new(c.x, c.y), view),
                    progress: (t - c.t) / CLICK,
                    scale: self.scale * style.size.clamp(0.5, 3.0) * view.level,
                })
                .collect()
        };
        Frame {
            view,
            previous,
            cursor,
            clicks,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::model::Aspect;
    use crate::studio::events::Button;

    fn sample(t: f64, x: f64, y: f64) -> CursorSample {
        CursorSample { t, x, y, image: 0 }
    }

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
    fn lays_out_the_canvas() {
        let none = layout((1000.0, 500.0), None, 2.0);
        assert_eq!(none.canvas, (1000.0, 500.0));
        let bg = Background {
            padding: 50.0,
            aspect: Aspect::Auto,
            ..Background::default()
        };
        let padded = layout((1000.0, 500.0), Some(&bg), 2.0);
        assert_eq!(padded.canvas, (1200.0, 700.0));
        assert_eq!(padded.video, Rect::new(100.0, 100.0, 1000.0, 500.0));
        let square = layout(
            (1000.0, 500.0),
            Some(&Background {
                aspect: Aspect::Square,
                ..bg
            }),
            2.0,
        );
        assert_eq!(square.canvas, (1200.0, 1200.0));
        assert_eq!(square.video.y, 350.0);
    }

    #[test]
    fn follows_the_cursor_exactly_without_smoothing() {
        let samples: Vec<CursorSample> = (0..=120)
            .map(|i| sample(f64::from(i) / 120.0, f64::from(i), 0.0))
            .collect();
        let path = CursorPath::new(&samples, &[], 0.0);
        let p = path.position(0.5).unwrap();
        assert!((p.x - 60.0).abs() < 1e-9, "{p:?}");
        assert_eq!(
            path.position(5.0).unwrap().x,
            120.0,
            "holds the last position"
        );
    }

    #[test]
    fn smoothing_lags_without_overshooting_and_hits_clicks() {
        // A jump from 0 to 100 at t = 1.
        let samples: Vec<CursorSample> = (0..=360)
            .map(|i| {
                let t = f64::from(i) / 120.0;
                sample(t, if t < 1.0 { 0.0 } else { 100.0 }, 0.0)
            })
            .collect();
        let path = CursorPath::new(&samples, &[], 0.6);
        let soon = path.position(1.05).unwrap().x;
        assert!(soon > 0.0 && soon < 80.0, "lags behind: {soon}");
        let max = (0..360)
            .map(|i| path.position(f64::from(i) / 120.0).unwrap().x)
            .fold(0.0, f64::max);
        assert!(max <= 100.0 + 1e-6, "no overshoot: {max}");
        // With a click just after the jump, the cursor is there at the click.
        let clicked = CursorPath::new(&samples, &[click(1.05, 100.0, 0.0)], 0.6);
        assert!((clicked.position(1.05).unwrap().x - 100.0).abs() < 0.5);
        // A press and release right after each other do not push it twice
        // as far (the cursor never passes the click).
        let mut release = click(1.08, 100.0, 0.0);
        release.down = false;
        let both = CursorPath::new(&samples, &[click(1.05, 100.0, 0.0), release], 0.6);
        assert!((both.position(1.05).unwrap().x - 100.0).abs() < 0.5);
        let max = (0..360)
            .map(|i| both.position(f64::from(i) / 120.0).unwrap().x)
            .fold(0.0, f64::max);
        assert!(max <= 100.5, "no overshoot past the click: {max}");
        // After the click the cursor stays on it (the mouse did not move).
        for t in [1.06, 1.1, 1.3, 2.0] {
            assert!((both.position(t).unwrap().x - 100.0).abs() < 0.5, "at {t}");
        }
    }

    #[test]
    fn hides_an_idle_cursor() {
        // Moves for one second, rests for three, moves again at 4.
        let mut samples: Vec<CursorSample> = (0..=120)
            .map(|i| sample(f64::from(i) / 120.0, f64::from(i), 0.0))
            .collect();
        samples.push(sample(4.0, 120.0, 0.0));
        samples.push(sample(4.01, 130.0, 0.0));
        let path = CursorPath::new(&samples, &[], 0.0);
        assert_eq!(path.idle_opacity(1.5), 1.0);
        assert_eq!(path.idle_opacity(3.5), 0.0);
        assert!(path.idle_opacity(3.2) > 0.0 && path.idle_opacity(3.2) < 1.0);
        assert!(path.idle_opacity(3.95) > 0.5, "fades back in before moving");
    }

    #[test]
    fn describes_frames() {
        let events = Events {
            scale: 2.0,
            cursor: vec![sample(0.0, 10.0, 20.0), sample(1.0, 10.0, 20.0)],
            clicks: vec![click(0.5, 10.0, 20.0)],
            ..Events::default()
        };
        let project = Project {
            background: None,
            ..Project::default()
        };
        let scene = Scene::new(&events, &project, (400.0, 300.0), 1.0);
        let frame = scene.frame(0.6);
        let cursor = frame.cursor.unwrap();
        assert_eq!(cursor.tip, Point::new(20.0, 40.0));
        assert_eq!(cursor.scale, 2.0 * project.cursor.size);
        assert_eq!(frame.clicks.len(), 1);
        assert!((frame.clicks[0].progress - 0.2).abs() < 1e-9);
        assert!(scene.frame(1.2).clicks.is_empty());
    }
}
