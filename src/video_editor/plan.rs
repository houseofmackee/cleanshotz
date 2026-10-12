//! What an export does, decided without AVFoundation: trim range, crop,
//! speed, output size and bitrate, and whether the video can simply be
//! copied.

use crate::capture::geometry::Rect;

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

/// Playback speeds offered, as factors.
pub const SPEEDS: [f64; 7] = [0.5, 0.75, 1.0, 1.25, 1.5, 2.0, 4.0];

/// Bits per pixel per frame at the lowest and highest quality.
const BPP_RANGE: (f64, f64) = (0.03, 0.20);
/// The quality used when re-encoding without one chosen: about the
/// recording's own "high" quality.
pub const DEFAULT_QUALITY: f64 = 0.55;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plan {
    /// Seconds into the source.
    pub start: f64,
    pub end: f64,
    pub mute: bool,
    pub resolution: Resolution,
    pub format: Format,
    /// The part of the frame to keep, in source pixels (top-left origin).
    pub crop: Option<Rect>,
    /// Playback speed factor: 2.0 plays twice as fast.
    pub speed: f64,
    /// Quality from 0 to 1, when chosen; otherwise the video is copied
    /// when nothing else needs re-encoding.
    pub quality: Option<f64>,
    /// Put the watermark from Settings > Watermark on every frame.
    pub watermark: bool,
}

impl Plan {
    /// Length of the trimmed range in the source.
    pub fn duration(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }

    /// Length of the result, after the speed change.
    pub fn output_duration(&self) -> f64 {
        self.duration() / self.speed.max(0.01)
    }

    /// Whether the frames must be decoded and encoded again; otherwise the
    /// samples are copied, which is fast and lossless.
    pub fn needs_reencode(&self, source_size: (f64, f64)) -> bool {
        self.crop.is_some()
            || self.watermark
            || (self.speed - 1.0).abs() > 1e-6
            || self.quality.is_some()
            || preset(self.resolution, source_size) != Preset::Passthrough
    }

    /// Whether saving would change the file at all.
    pub fn changes_anything(&self, source_duration: f64, source_size: (f64, f64)) -> bool {
        let trimmed = self.start > 0.01 || self.end < source_duration - 0.01;
        trimmed || self.mute || self.format == Format::Gif || self.needs_reencode(source_size)
    }

    /// Output size in pixels: the crop (or the whole frame), fitted into
    /// the resolution, never upscaled, in even numbers.
    pub fn output_size(&self, source_size: (f64, f64)) -> (usize, usize) {
        let (w, h) = self.crop.map_or(source_size, |c| (c.width, c.height));
        let factor = match self.resolution.bounds() {
            Some((bw, bh)) => {
                let (bw, bh) = if h > w { (bh, bw) } else { (bw, bh) };
                (bw / w).min(bh / h).min(1.0)
            }
            None => 1.0,
        };
        let even = |v: f64| (((v * factor) / 2.0).round() as usize * 2).max(2);
        (even(w), even(h))
    }

    /// Video bitrate for re-encoding at `fps`.
    pub fn bitrate(&self, source_size: (f64, f64), fps: f64) -> u64 {
        let (w, h) = self.output_size(source_size);
        let q = self.quality.unwrap_or(DEFAULT_QUALITY).clamp(0.0, 1.0);
        let bpp = BPP_RANGE.0 + q * (BPP_RANGE.1 - BPP_RANGE.0);
        let fps = if fps.is_finite() && fps > 0.0 {
            fps.min(120.0)
        } else {
            30.0
        };
        ((w * h) as f64 * fps * bpp).clamp(250_000.0, 150_000_000.0) as u64
    }

    /// Estimated file size in bytes: from the target bitrate when
    /// re-encoding (an upper bound; encoders often use less), otherwise
    /// the source's share for the trimmed range.
    pub fn estimated_size(
        &self,
        source_size: (f64, f64),
        source_duration: f64,
        source_bytes: u64,
        fps: f64,
    ) -> u64 {
        let audio = if self.mute { 0.0 } else { 192_000.0 / 8.0 };
        if self.needs_reencode(source_size) {
            let video = self.bitrate(source_size, fps) as f64 / 8.0;
            ((video + audio) * self.output_duration()) as u64
        } else if source_duration > 0.0 {
            (source_bytes as f64 * self.duration() / source_duration) as u64
        } else {
            source_bytes
        }
    }
}

/// `crop` (source pixels) limited to the frame, or `None` when it is
/// too small to use or covers the whole frame.
pub fn clamp_crop(crop: Rect, source_size: (f64, f64)) -> Option<Rect> {
    let frame = Rect::new(0.0, 0.0, source_size.0, source_size.1);
    let c = crop.intersection(&frame).rounded();
    let whole = c.width >= frame.width - 1.0 && c.height >= frame.height - 1.0;
    (c.width >= 16.0 && c.height >= 16.0 && !whole).then_some(c)
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
            crop: None,
            speed: 1.0,
            quality: None,
            watermark: false,
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

    fn plan() -> Plan {
        Plan {
            start: 0.0,
            end: 10.0,
            mute: false,
            resolution: Resolution::Original,
            format: Format::Mp4,
            crop: None,
            speed: 1.0,
            quality: None,
            watermark: false,
        }
    }

    #[test]
    fn watermarks_need_reencoding() {
        let plan = Plan {
            watermark: true,
            ..plan()
        };
        assert!(plan.needs_reencode((1600.0, 1000.0)));
        assert!(plan.changes_anything(10.0, (1600.0, 1000.0)));
    }

    #[test]
    fn crop_speed_and_quality_need_reencoding() {
        let size = (1600.0, 1000.0);
        assert!(!plan().needs_reencode(size));
        assert!(
            Plan {
                speed: 2.0,
                ..plan()
            }
            .needs_reencode(size)
        );
        assert!(
            Plan {
                quality: Some(0.3),
                ..plan()
            }
            .needs_reencode(size)
        );
        let crop = Some(Rect::new(100.0, 50.0, 800.0, 600.0));
        assert!(Plan { crop, ..plan() }.needs_reencode(size));
        assert!(Plan { crop, ..plan() }.changes_anything(10.0, size));
        assert_eq!(
            Plan {
                speed: 2.0,
                ..plan()
            }
            .output_duration(),
            5.0
        );
    }

    #[test]
    fn output_size_crops_then_fits_without_upscaling() {
        let size = (3456.0, 2234.0);
        assert_eq!(plan().output_size(size), (3456, 2234));
        let crop = Some(Rect::new(10.0, 10.0, 1001.0, 501.0));
        // Odd sizes become even.
        assert_eq!(Plan { crop, ..plan() }.output_size(size), (1002, 502));
        let p = Plan {
            resolution: Resolution::P720,
            ..plan()
        };
        assert_eq!(p.output_size(size), (1114, 720));
        // A crop already smaller than 720p stays as it is.
        assert_eq!(Plan { crop, ..p }.output_size(size), (1002, 502));
    }

    #[test]
    fn quality_scales_the_bitrate() {
        let size = (1920.0, 1080.0);
        let low = Plan {
            quality: Some(0.0),
            ..plan()
        }
        .bitrate(size, 30.0);
        let high = Plan {
            quality: Some(1.0),
            ..plan()
        }
        .bitrate(size, 30.0);
        let default = plan().bitrate(size, 30.0);
        assert!(low < default && default < high);
        assert_eq!(high, (1920.0 * 1080.0 * 30.0 * 0.20) as u64);
    }

    #[test]
    fn estimates_sizes() {
        let size = (1920.0, 1080.0);
        // Copied: the trimmed share of the source.
        let half = Plan { end: 5.0, ..plan() };
        assert_eq!(half.estimated_size(size, 10.0, 8_000_000, 30.0), 4_000_000);
        // Re-encoded at double speed: half the duration.
        let fast = Plan {
            speed: 2.0,
            mute: true,
            ..plan()
        };
        let bytes = fast.estimated_size(size, 10.0, 8_000_000, 30.0);
        assert_eq!(bytes, (fast.bitrate(size, 30.0) as f64 / 8.0 * 5.0) as u64);
    }

    #[test]
    fn crops_are_clamped() {
        let size = (1000.0, 800.0);
        assert_eq!(
            clamp_crop(Rect::new(-10.0, 100.0, 300.0, 300.0), size),
            Some(Rect::new(0.0, 100.0, 290.0, 300.0))
        );
        assert_eq!(clamp_crop(Rect::new(0.0, 0.0, 1000.0, 800.0), size), None);
        assert_eq!(clamp_crop(Rect::new(10.0, 10.0, 8.0, 300.0), size), None);
    }
}
