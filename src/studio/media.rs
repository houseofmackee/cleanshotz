//! The AVFoundation side of Studio recordings: the composition the preview
//! plays and the export encodes, both drawn by the Studio compositor.

use std::path::Path;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2_av_foundation::{
    AVMediaTypeAudio, AVMediaTypeVideo, AVMutableComposition, AVMutableVideoComposition, AVURLAsset,
};
use objc2_core_foundation::CGSize;
use objc2_core_media::{CMTime, CMTimeRange};
use objc2_foundation::{NSArray, NSString, NSURL};

use super::compositor::{Renderer, StudioCompositor, StudioInstruction, instruction_object};

/// Studio output is drawn at 60 frames a second, so the cursor and zooms
/// move smoothly even when the screen was recorded at 30.
pub const FPS: f64 = 60.0;

pub struct Prepared {
    pub composition: Retained<AVMutableComposition>,
    pub video: Retained<AVMutableVideoComposition>,
}

fn time(seconds: f64) -> CMTime {
    // SAFETY: constructing a CMTime.
    unsafe { CMTime::with_seconds(seconds, 600) }
}

/// The screen video's size in pixels and duration in seconds.
pub fn screen_info(package: &Path) -> (f64, f64, f64) {
    let screen = super::package::screen(package);
    let (w, h) = crate::record::media::size(&screen);
    (w, h, crate::record::media::duration(&screen))
}

/// A composition of the screen video (and its audio) with a video
/// composition that draws every frame through `renderer`.
///
/// # Safety
///
/// AVFoundation objects, used on one thread at a time.
pub unsafe fn prepare(package: &Path, renderer: Arc<Renderer>) -> Result<Prepared, String> {
    // SAFETY: per the function's contract.
    unsafe {
        let url = NSURL::fileURLWithPath(&NSString::from_str(
            &super::package::screen(package).to_string_lossy(),
        ));
        let asset = AVURLAsset::URLAssetWithURL_options(&url, None);
        let composition = AVMutableComposition::composition();
        let video = AVMediaTypeVideo.ok_or("no video media type")?;
        let audio = AVMediaTypeAudio.ok_or("no audio media type")?;
        #[allow(deprecated)]
        let source = asset
            .tracksWithMediaType(video)
            .firstObject()
            .ok_or("the recording has no video")?;
        let range = source.timeRange();
        let screen = composition
            .addMutableTrackWithMediaType_preferredTrackID(video, 0)
            .ok_or("could not add the video")?;
        screen
            .insertTimeRange_ofTrack_atTime_error(range, &source, time(0.0))
            .map_err(|e| e.localizedDescription().to_string())?;
        #[allow(deprecated)]
        for track in asset.tracksWithMediaType(audio).iter() {
            if let Some(target) =
                composition.addMutableTrackWithMediaType_preferredTrackID(audio, 0)
            {
                target
                    .insertTimeRange_ofTrack_atTime_error(range, &track, time(0.0))
                    .map_err(|e| e.localizedDescription().to_string())?;
            }
        }
        let camera = add_camera(package, &composition, range.duration.seconds());
        let (width, height) = renderer.output();
        let instruction = StudioInstruction::new(
            renderer,
            CMTimeRange {
                start: time(0.0),
                duration: composition.duration(),
            },
            screen.trackID(),
            camera,
        );
        let video_composition = AVMutableVideoComposition::videoComposition();
        video_composition.setCustomVideoCompositorClass(Some(StudioCompositor::class_object()));
        video_composition.setRenderSize(CGSize::new(width as f64, height as f64));
        video_composition.setFrameDuration(time(1.0 / FPS));
        video_composition.setInstructions(&NSArray::from_retained_slice(&[instruction_object(
            instruction,
        )]));
        Ok(Prepared {
            composition,
            video: video_composition,
        })
    }
}

/// Adds the camera recording, lined up with the screen, as its own video
/// track; returns its id. Recordings without a camera return `None`.
///
/// # Safety
///
/// AVFoundation objects, used on one thread at a time.
unsafe fn add_camera(
    package: &Path,
    composition: &AVMutableComposition,
    duration: f64,
) -> Option<i32> {
    let events = super::package::load_events(package).ok()?;
    let track = events.camera?;
    let file = package.join(&track.file);
    if !file.exists() {
        return None;
    }
    // SAFETY: per the function's contract.
    unsafe {
        let url = NSURL::fileURLWithPath(&NSString::from_str(&file.to_string_lossy()));
        let asset = AVURLAsset::URLAssetWithURL_options(&url, None);
        let video = AVMediaTypeVideo?;
        #[allow(deprecated)]
        let source = asset.tracksWithMediaType(video).firstObject()?;
        let length = source.timeRange().duration.seconds();
        // A camera that started before the screen loses its beginning.
        let skip = (-track.offset).max(0.0);
        let at = track.offset.max(0.0);
        let take = (length - skip).min(duration - at);
        if take <= 0.0 {
            return None;
        }
        let target = composition.addMutableTrackWithMediaType_preferredTrackID(video, 0)?;
        target
            .insertTimeRange_ofTrack_atTime_error(
                CMTimeRange {
                    start: time(skip),
                    duration: time(take),
                },
                &source,
                time(at),
            )
            .ok()?;
        Some(target.trackID())
    }
}

/// A bitrate that keeps screen content sharp: about 0.08 bits per pixel
/// per frame.
pub fn bitrate((width, height): (usize, usize)) -> u64 {
    ((width * height) as f64 * FPS * 0.08).clamp(2_000_000.0, 80_000_000.0) as u64
}

/// Renders the recording to an MP4 at `output`. Blocks; call it off the
/// main thread.
pub fn export(package: &Path, renderer: Arc<Renderer>, output: &Path) -> Result<(), String> {
    let size = renderer.output();
    let _ = std::fs::remove_file(output);
    // SAFETY: AVFoundation objects created and used on this thread.
    unsafe {
        let prepared = prepare(package, renderer)?;
        crate::video_editor::export::encode(
            &prepared.composition,
            &prepared.video,
            size,
            FPS,
            bitrate(size),
            output,
        )
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn picks_bitrates() {
        assert_eq!(super::bitrate((1920, 1080)), 9_953_280);
        assert_eq!(super::bitrate((100, 100)), 2_000_000);
    }
}
