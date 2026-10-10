//! What an export does, decided without AVFoundation: trim range, export
//! preset and output size.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    Original,
    P1080,
    P720,
    P540,
}

impl Resolution {
    pub const ALL: [Resolution; 4] = [
        Resolution::Original,
        Resolution::P1080,
        Resolution::P720,
        Resolution::P540,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Resolution::Original => "Original Size",
            Resolution::P1080 => "1080p",
            Resolution::P720 => "720p",
            Resolution::P540 => "540p",
        }
    }

    /// The box the video must fit in, landscape (width, height).
    fn bounds(self) -> Option<(f64, f64)> {
        match self {
            Resolution::Original => None,
            Resolution::P1080 => Some((1920.0, 1080.0)),
            Resolution::P720 => Some((1280.0, 720.0)),
            Resolution::P540 => Some((960.0, 540.0)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Mp4,
    Gif,
}

/// An AVFoundation export preset, by meaning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    /// Copy the samples without re-encoding: fast and lossless.
    Passthrough,
    P1080,
    P720,
    P540,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plan {
    /// Seconds into the source.
    pub start: f64,
    pub end: f64,
    pub mute: bool,
    pub resolution: Resolution,
    pub format: Format,
}

impl Plan {
    pub fn duration(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }

    /// Whether saving would change the file at all.
    pub fn changes_anything(&self, source_duration: f64, source_size: (f64, f64)) -> bool {
        let trimmed = self.start > 0.01 || self.end < source_duration - 0.01;
        trimmed
            || self.mute
            || self.format == Format::Gif
            || preset(self.resolution, source_size) != Preset::Passthrough
    }
}

/// The trim range from the player's playback end times (each `None` when
/// not set), clamped to the video. An empty range falls back to the whole
/// video.
pub fn trim_range(duration: f64, start: Option<f64>, end: Option<f64>) -> (f64, f64) {
    let valid = |t: Option<f64>| t.filter(|t| t.is_finite());
    let start = valid(start).unwrap_or(0.0).clamp(0.0, duration);
    let end = valid(end).unwrap_or(duration).clamp(0.0, duration);
    if end - start < 0.05 {
        (0.0, duration)
    } else {
        (start, end)
    }
}

/// The preset for `resolution`. Never upscales: if the video already fits,
/// it is copied as is.
pub fn preset(resolution: Resolution, source_size: (f64, f64)) -> Preset {
    let Some((bw, bh)) = resolution.bounds() else {
        return Preset::Passthrough;
    };
    let (w, h) = source_size;
    // Compare the long and short sides, so portrait videos fit too.
    let (long, short) = (w.max(h), w.min(h));
    if long <= bw && short <= bh {
        return Preset::Passthrough;
    }
    match resolution {
        Resolution::P1080 => Preset::P1080,
        Resolution::P720 => Preset::P720,
        _ => Preset::P540,
    }
}

/// GIF width in pixels: the video's width, limited by the chosen
/// resolution and by `max_width` from the settings.
pub fn gif_width(resolution: Resolution, source_size: (f64, f64), max_width: u32) -> u32 {
    let (w, h) = source_size;
    let fitted = match resolution.bounds() {
        Some((bw, bh)) => {
            let (bw, bh) = if h > w { (bh, bw) } else { (bw, bh) };
            w * (bw / w).min(bh / h).min(1.0)
        }
        None => w,
    };
    (fitted.round() as u32).clamp(16, max_width.max(16))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_range_defaults_and_clamps() {
        assert_eq!(trim_range(10.0, None, None), (0.0, 10.0));
        assert_eq!(trim_range(10.0, Some(2.0), Some(7.5)), (2.0, 7.5));
        assert_eq!(trim_range(10.0, Some(-1.0), Some(99.0)), (0.0, 10.0));
        assert_eq!(trim_range(10.0, Some(f64::NAN), Some(4.0)), (0.0, 4.0));
        // Empty selection: keep everything.
        assert_eq!(trim_range(10.0, Some(5.0), Some(5.0)), (0.0, 10.0));
    }

    #[test]
    fn presets_never_upscale() {
        assert_eq!(
            preset(Resolution::Original, (3456.0, 2234.0)),
            Preset::Passthrough
        );
        assert_eq!(preset(Resolution::P1080, (3456.0, 2234.0)), Preset::P1080);
        assert_eq!(
            preset(Resolution::P1080, (1600.0, 1000.0)),
            Preset::Passthrough
        );
        assert_eq!(preset(Resolution::P720, (1600.0, 1000.0)), Preset::P720);
        // Portrait 1080x1920 fits in 1080p.
        assert_eq!(
            preset(Resolution::P1080, (1080.0, 1920.0)),
            Preset::Passthrough
        );
        assert_eq!(preset(Resolution::P540, (1080.0, 1920.0)), Preset::P540);
    }

    #[test]
    fn gif_width_respects_resolution_and_limit() {
        assert_eq!(
            gif_width(Resolution::Original, (1600.0, 1000.0), 1280),
            1280
        );
        assert_eq!(gif_width(Resolution::Original, (800.0, 500.0), 1280), 800);
        assert_eq!(gif_width(Resolution::P540, (1600.0, 1000.0), 1280), 864);
        assert_eq!(gif_width(Resolution::P720, (1000.0, 2000.0), 1280), 640);
    }

    #[test]
    fn detects_no_op_saves() {
        let plan = Plan {
            start: 0.0,
            end: 10.0,
            mute: false,
            resolution: Resolution::Original,
            format: Format::Mp4,
        };
        assert!(!plan.changes_anything(10.0, (1600.0, 1000.0)));
        assert!(Plan { start: 1.0, ..plan }.changes_anything(10.0, (1600.0, 1000.0)));
        assert!(Plan { mute: true, ..plan }.changes_anything(10.0, (1600.0, 1000.0)));
        assert!(
            !Plan {
                resolution: Resolution::P1080,
                ..plan
            }
            .changes_anything(10.0, (1600.0, 1000.0))
        );
        assert!(
            Plan {
                format: Format::Gif,
                ..plan
            }
            .changes_anything(10.0, (1600.0, 1000.0))
        );
    }
}
