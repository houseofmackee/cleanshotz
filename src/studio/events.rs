//! What a Studio recording knows besides its video: where the cursor was
//! and what it looked like, clicks, keys and the camera recording, saved as
//! `events.json` in the `.cszrec` package. Times are seconds of video time
//! (pauses removed); positions are points from the top-left of the
//! recorded area.

use serde::{Deserialize, Serialize};

use crate::capture::geometry::{Point, Rect};

pub const VERSION: u32 = 1;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Events {
    pub version: u32,
    /// The recorded area in global screen points (top-left origin).
    pub area: Rect,
    /// Video pixels per point.
    pub scale: f64,
    pub cursor: Vec<CursorSample>,
    pub cursor_images: Vec<CursorImage>,
    pub clicks: Vec<Click>,
    pub keys: Vec<KeyPress>,
    pub camera: Option<CameraTrack>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CursorSample {
    pub t: f64,
    pub x: f64,
    pub y: f64,
    /// Which of [`Events::cursor_images`].
    pub image: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CursorImage {
    pub id: u32,
    /// A PNG in the package's `cursors` folder.
    pub file: String,
    /// Where the pointer's tip is, in points from the image's top-left.
    pub hotspot: Point,
    /// Size in points.
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Button {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Click {
    pub t: f64,
    pub x: f64,
    pub y: f64,
    pub button: Button,
    /// Pressed (true) or released.
    pub down: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KeyPress {
    pub t: f64,
    /// As the keystroke display shows it, such as "⌘C".
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CameraTrack {
    /// A movie in the package.
    pub file: String,
    /// The video time at which the camera recording starts.
    pub offset: f64,
}

/// Turns raw clock readings (seconds since recording began) into video
/// time: zero at the first frame, with paused stretches cut out.
#[derive(Clone, Debug, Default)]
pub struct Timeline {
    /// When the first frame arrived.
    pub first_frame: f64,
    /// Paused stretches (start, end); the last may still be open.
    pub pauses: Vec<(f64, Option<f64>)>,
}

impl Timeline {
    /// The video time of clock reading `t`; `None` before the first frame
    /// or while paused.
    pub fn video_time(&self, t: f64) -> Option<f64> {
        if t < self.first_frame {
            return None;
        }
        let mut paused = 0.0;
        for &(start, end) in &self.pauses {
            let end = end.unwrap_or(f64::INFINITY);
            if t >= start && t < end {
                return None;
            }
            if t >= end {
                paused += end - start.max(self.first_frame);
            }
        }
        Some(t - self.first_frame - paused)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_clock_readings_to_video_time() {
        let timeline = Timeline {
            first_frame: 1.0,
            pauses: vec![(3.0, Some(5.0)), (8.0, None)],
        };
        assert_eq!(timeline.video_time(0.5), None, "before the first frame");
        assert_eq!(timeline.video_time(1.0), Some(0.0));
        assert_eq!(timeline.video_time(2.5), Some(1.5));
        assert_eq!(timeline.video_time(4.0), None, "paused");
        assert_eq!(
            timeline.video_time(6.0),
            Some(3.0),
            "two paused seconds cut"
        );
        assert_eq!(timeline.video_time(9.0), None, "paused until the end");
    }

    #[test]
    fn round_trips_through_json() {
        let events = Events {
            version: VERSION,
            area: Rect::new(10.0, 20.0, 800.0, 600.0),
            scale: 2.0,
            cursor: vec![CursorSample {
                t: 0.5,
                x: 1.0,
                y: 2.0,
                image: 0,
            }],
            clicks: vec![Click {
                t: 0.6,
                x: 1.0,
                y: 2.0,
                button: Button::Left,
                down: true,
            }],
            keys: vec![KeyPress {
                t: 1.0,
                label: "⌘C".into(),
            }],
            ..Events::default()
        };
        let json = serde_json::to_string(&events).unwrap();
        assert_eq!(serde_json::from_str::<Events>(&json).unwrap(), events);
    }
}
