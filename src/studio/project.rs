//! A Studio recording's edits (`project.json`): how the background, cursor
//! and output look. Everything has a default, so a new recording renders
//! without a project file.

use serde::{Deserialize, Serialize};

use crate::editor::model::Background;

pub const VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Project {
    pub version: u32,
    /// Around the recording; `None` shows the recording alone.
    pub background: Option<Background>,
    pub cursor: CursorStyle,
    pub output: OutputSize,
}

impl Default for Project {
    fn default() -> Self {
        Self {
            version: VERSION,
            background: Some(Background::default()),
            cursor: CursorStyle::default(),
            output: OutputSize::P1080,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CursorStyle {
    pub visible: bool,
    /// 0.5 to 3 times the recorded size.
    pub size: f64,
    /// 0 (follow exactly) to 1 (very smooth).
    pub smoothing: f64,
    /// Fade the cursor out when it has not moved for a while.
    pub hide_when_idle: bool,
    pub clicks: ClickEffect,
}

impl Default for CursorStyle {
    fn default() -> Self {
        Self {
            visible: true,
            size: 1.5,
            smoothing: 0.5,
            hide_when_idle: true,
            clicks: ClickEffect::Ripple,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClickEffect {
    None,
    /// A ring that shrinks onto the click.
    Ring,
    /// A circle that grows and fades from the click.
    Ripple,
}

/// The exported video's size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputSize {
    /// The canvas at the recording's own resolution.
    Source,
    /// Fits 1920 × 1080 (or 1080 × 1920 when upright).
    P1080,
    P1440,
    P2160,
}

impl OutputSize {
    pub const ALL: [OutputSize; 4] = [
        OutputSize::Source,
        OutputSize::P1080,
        OutputSize::P1440,
        OutputSize::P2160,
    ];

    pub fn title(self) -> &'static str {
        match self {
            OutputSize::Source => "Original Size",
            OutputSize::P1080 => "1080p",
            OutputSize::P1440 => "1440p",
            OutputSize::P2160 => "4K",
        }
    }

    /// The short side of the frame it fits.
    fn short_side(self) -> Option<f64> {
        match self {
            OutputSize::Source => None,
            OutputSize::P1080 => Some(1080.0),
            OutputSize::P1440 => Some(1440.0),
            OutputSize::P2160 => Some(2160.0),
        }
    }

    /// The output size in pixels for a canvas `w` × `h` pixels: scaled to
    /// fit the preset's frame (never up), with even sides as encoders need.
    pub fn fit(self, w: f64, h: f64) -> (usize, usize) {
        let scale = match self.short_side() {
            Some(short) => {
                let long = short * 16.0 / 9.0;
                (long / w.max(h)).min(short / w.min(h)).min(1.0)
            }
            None => 1.0,
        };
        let even = |v: f64| (((v * scale) / 2.0).round() as usize * 2).max(2);
        (even(w), even(h))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_output_sizes() {
        assert_eq!(OutputSize::P1080.fit(3840.0, 2160.0), (1920, 1080));
        assert_eq!(OutputSize::P1080.fit(2160.0, 3840.0), (1080, 1920));
        // Never scaled up; sides stay even.
        assert_eq!(OutputSize::P2160.fit(1401.0, 901.0), (1402, 902));
        // A square canvas fits the short side.
        assert_eq!(OutputSize::P1080.fit(2000.0, 2000.0), (1080, 1080));
        assert_eq!(OutputSize::Source.fit(1000.0, 500.0), (1000, 500));
    }

    #[test]
    fn reads_partial_projects() {
        let project: Project = serde_json::from_str(r#"{"cursor":{"size":2.0}}"#).unwrap();
        assert_eq!(project.cursor.size, 2.0);
        assert!(project.cursor.visible);
        assert_eq!(project.output, OutputSize::P1080);
    }
}
