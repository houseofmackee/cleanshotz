//! Reading finished recordings: duration, thumbnails and GIF conversion.

use std::path::Path;
use std::ptr::{self, NonNull};

use objc2::rc::Retained;
use objc2_av_foundation::{AVAssetImageGenerator, AVURLAsset};
use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString, CFURL, CGSize};
use objc2_core_graphics::CGImage;
use objc2_core_media::CMTime;
use objc2_foundation::{NSString, NSURL};
use objc2_image_io::{
    CGImageDestination, kCGImagePropertyGIFDelayTime, kCGImagePropertyGIFDictionary,
    kCGImagePropertyGIFLoopCount,
};

fn asset(path: &Path) -> Retained<AVURLAsset> {
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    // SAFETY: a file URL and no options.
    unsafe { AVURLAsset::URLAssetWithURL_options(&url, None) }
}

/// Duration in seconds.
pub fn duration(path: &Path) -> f64 {
    // SAFETY: synchronous property read on a local file.
    unsafe { asset(path).duration().seconds() }
}

/// Pixel size of the first video track (0 x 0 if there is none).
pub fn size(path: &Path) -> (f64, f64) {
    // SAFETY: synchronous track query on a local file.
    unsafe {
        let Some(video) = objc2_av_foundation::AVMediaTypeVideo else {
            return (0.0, 0.0);
        };
        #[allow(deprecated)]
        let tracks = asset(path).tracksWithMediaType(video);
        tracks.firstObject().map_or((0.0, 0.0), |t| {
            let s = t.naturalSize();
            (s.width, s.height)
        })
    }
}

/// Exact frames from a recording, scaled to fit `max_width` pixels.
struct Frames {
    generator: Retained<AVAssetImageGenerator>,
}

impl Frames {
    fn new(path: &Path, max_width: Option<f64>) -> Self {
        // SAFETY: configuring a generator for a local asset.
        let generator = unsafe {
            let generator = AVAssetImageGenerator::assetImageGeneratorWithAsset(&asset(path));
            generator.setAppliesPreferredTrackTransform(true);
            let exact = CMTime::with_seconds(0.0, 600);
            generator.setRequestedTimeToleranceBefore(exact);
            generator.setRequestedTimeToleranceAfter(exact);
            if let Some(w) = max_width {
                generator.setMaximumSize(CGSize::new(w, w * 10.0));
            }
            generator
        };
        Self { generator }
    }

    fn at(&self, seconds: f64) -> Option<CFRetained<CGImage>> {
        #[allow(deprecated)]
        // SAFETY: a valid time; no actual-time output.
        let image = unsafe {
            self.generator.copyCGImageAtTime_actualTime_error(
                CMTime::with_seconds(seconds, 600),
                ptr::null_mut(),
            )
        }
        .ok()?;
        // SAFETY: retaining a valid image as a CoreFoundation object.
        Some(unsafe { CFRetained::retain(NonNull::from(&*image)) })
    }
}

/// A frame shortly after the start, for the Quick Access thumbnail.
pub fn thumbnail(path: &Path) -> Option<CFRetained<CGImage>> {
    let frames = Frames::new(path, None);
    let duration = duration(path);
    frames
        .at((duration / 2.0).min(0.2))
        .or_else(|| frames.at(0.0))
}

/// Converts a recording to a looping GIF at `fps`, at most `max_width`
/// pixels wide. ImageIO reduces each frame to a 256-color palette.
pub fn to_gif(video: &Path, gif: &Path, fps: u32, max_width: u32) -> Result<(), String> {
    let duration = duration(video);
    if duration <= 0.0 {
        return Err("the recording is empty".into());
    }
    let frames = Frames::new(video, Some(max_width as f64));
    let count = ((duration * fps as f64).floor() as usize).max(1);
    let url = CFURL::from_file_path(gif).ok_or("invalid GIF path")?;
    let gif_type = CFString::from_static_str("com.compuserve.gif");
    // SAFETY: valid URL and type identifier.
    let destination = unsafe { CGImageDestination::with_url(&url, &gif_type, count, None) }
        .ok_or("could not create the GIF")?;

    // SAFETY: reading ImageIO constants.
    let (gif_key, loop_key, delay_key) = unsafe {
        (
            kCGImagePropertyGIFDictionary,
            kCGImagePropertyGIFLoopCount,
            kCGImagePropertyGIFDelayTime,
        )
    };
    let nested = |key: &CFString, value: &CFNumber| {
        let inner = CFDictionary::from_slices(&[key], &[value]);
        CFDictionary::from_slices(&[gif_key], &[&*inner])
    };
    // Loop forever.
    let file_properties = nested(loop_key, &CFNumber::new_i32(0));
    let frame_properties = nested(delay_key, &CFNumber::new_f64(1.0 / fps as f64));
    // SAFETY: dictionaries of ImageIO keys to CF values.
    unsafe { destination.set_properties(Some(file_properties.as_opaque())) };

    let mut written = 0;
    for i in 0..count {
        let Some(frame) = frames.at(i as f64 / fps as f64) else {
            continue;
        };
        // SAFETY: as above.
        unsafe { destination.add_image(&frame, Some(frame_properties.as_opaque())) };
        written += 1;
    }
    if written == 0 {
        return Err("no frames could be read from the recording".into());
    }
    // SAFETY: finalizing after adding images.
    if unsafe { destination.finalize() } {
        Ok(())
    } else {
        Err("could not write the GIF".into())
    }
}
