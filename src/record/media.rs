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

/// Frame rate of the first video track (30 if unknown).
pub fn fps(path: &Path) -> f64 {
    // SAFETY: synchronous track query on a local file.
    let rate = unsafe {
        objc2_av_foundation::AVMediaTypeVideo.and_then(|video| {
            #[allow(deprecated)]
            asset(path)
                .tracksWithMediaType(video)
                .firstObject()
                .map(|t| f64::from(t.nominalFrameRate()))
        })
    };
    rate.filter(|r| r.is_finite() && *r > 0.0).unwrap_or(30.0)
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
    let written = write_gif(
        gif,
        count,
        1.0 / fps as f64,
        (0..count).filter_map(|i| frames.at(i as f64 / fps as f64)),
    )?;
    if written == 0 {
        return Err("no frames could be read from the recording".into());
    }
    Ok(())
}

/// Writes `frames` (at most `count`) as a looping GIF showing each for
/// `delay` seconds. Returns how many frames were written.
pub fn write_gif(
    gif: &Path,
    count: usize,
    delay: f64,
    frames: impl Iterator<Item = CFRetained<CGImage>>,
) -> Result<usize, String> {
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
    let frame_properties = nested(delay_key, &CFNumber::new_f64(delay));
    // SAFETY: dictionaries of ImageIO keys to CF values.
    unsafe { destination.set_properties(Some(file_properties.as_opaque())) };

    let mut written = 0;
    for frame in frames.take(count) {
        // SAFETY: as above.
        unsafe { destination.add_image(&frame, Some(frame_properties.as_opaque())) };
        written += 1;
    }
    // SAFETY: finalizing after adding images.
    if written > 0 && !unsafe { destination.finalize() } {
        return Err("could not write the GIF".into());
    }
    Ok(written)
}

/// Mixes all audio tracks of a recording (system audio and microphone)
/// into one stereo track, in place. The video is copied, not re-encoded.
/// Does nothing for recordings with fewer than two audio tracks.
pub fn mix_audio_tracks(path: &Path) -> Result<(), String> {
    use objc2_av_foundation::{
        AVAssetReader, AVAssetReaderAudioMixOutput, AVAssetReaderTrackOutput, AVAssetWriter,
        AVAssetWriterInput, AVFileTypeMPEG4, AVMediaTypeAudio, AVMediaTypeVideo,
    };
    use objc2_core_media::CMFormatDescription;
    use objc2_foundation::{NSDictionary, NSNumber};

    let source = asset(path);
    let mixed = path.with_extension("mixing.mp4");
    let constant =
        |c: Option<&'static objc2_foundation::NSString>| c.ok_or("missing AVFoundation constant");
    // SAFETY: track queries, reader and writer setup on local files; all
    // objects stay on this thread.
    unsafe {
        let (video_type, audio_type) = (constant(AVMediaTypeVideo)?, constant(AVMediaTypeAudio)?);
        #[allow(deprecated)]
        let audio_tracks = source.tracksWithMediaType(audio_type);
        if audio_tracks.count() < 2 {
            return Ok(());
        }
        #[allow(deprecated)]
        let video_track = source
            .tracksWithMediaType(video_type)
            .firstObject()
            .ok_or("the recording has no video")?;

        let reader = AVAssetReader::assetReaderWithAsset_error(&source)
            .map_err(|e| e.localizedDescription().to_string())?;
        let video_out = AVAssetReaderTrackOutput::assetReaderTrackOutputWithTrack_outputSettings(
            &video_track,
            None,
        );
        video_out.setAlwaysCopiesSampleData(false);
        reader.addOutput(&video_out);
        let number =
            |n: f64| -> Retained<objc2::runtime::AnyObject> { NSNumber::new_f64(n).into() };
        let settings = |pairs: &[(&str, f64)]| {
            let keys: Vec<Retained<NSString>> =
                pairs.iter().map(|(k, _)| NSString::from_str(k)).collect();
            let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
            let values: Vec<_> = pairs.iter().map(|(_, v)| number(*v)).collect();
            NSDictionary::from_retained_objects(&key_refs, &values)
        };
        const LPCM: f64 = 0x6c70_636d as f64; // 'lpcm'
        const AAC: f64 = 0x6161_6320 as f64; // 'aac '
        let pcm = settings(&[
            ("AVFormatIDKey", LPCM),
            ("AVSampleRateKey", 48_000.0),
            ("AVNumberOfChannelsKey", 2.0),
            ("AVLinearPCMBitDepthKey", 32.0),
            ("AVLinearPCMIsFloatKey", 1.0),
            ("AVLinearPCMIsNonInterleaved", 0.0),
        ]);
        let audio_out =
            AVAssetReaderAudioMixOutput::assetReaderAudioMixOutputWithAudioTracks_audioSettings(
                &audio_tracks,
                Some(&pcm),
            );
        reader.addOutput(&audio_out);

        let _ = std::fs::remove_file(&mixed);
        let url = NSURL::fileURLWithPath(&NSString::from_str(&mixed.to_string_lossy()));
        let writer =
            AVAssetWriter::assetWriterWithURL_fileType_error(&url, constant(AVFileTypeMPEG4)?)
                .map_err(|e| e.localizedDescription().to_string())?;
        // The array keeps the description alive while the input is made.
        let descriptions = video_track.formatDescriptions();
        let first = descriptions.firstObject();
        let hint = first
            .as_ref()
            .map(|d| &*(Retained::as_ptr(d) as *const CMFormatDescription));
        let video_in =
            AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings_sourceFormatHint(
                video_type, None, hint,
            );
        video_in.setTransform(video_track.preferredTransform());
        let aac = settings(&[
            ("AVFormatIDKey", AAC),
            ("AVSampleRateKey", 48_000.0),
            ("AVNumberOfChannelsKey", 2.0),
            ("AVEncoderBitRateKey", 192_000.0),
        ]);
        let audio_in = AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(
            audio_type,
            Some(&aac),
        );
        for input in [&video_in, &audio_in] {
            if !writer.canAddInput(input) {
                return Err("could not set up the mixed recording".into());
            }
            writer.addInput(input);
        }
        if !reader.startReading() || !writer.startWriting() {
            return Err("could not start mixing the audio".into());
        }
        writer.startSessionAtSourceTime(CMTime::with_seconds(0.0, 600));

        let pairs = [
            (
                &*video_out as &objc2_av_foundation::AVAssetReaderOutput,
                &*video_in,
            ),
            (&*audio_out, &*audio_in),
        ];
        if let Err(e) = transfer(&reader, &writer, &pairs, None) {
            let _ = std::fs::remove_file(&mixed);
            return Err(e);
        }
    }
    std::fs::rename(&mixed, path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Copies every sample from each reader output to its writer input,
/// feeding whichever input is ready, then finishes the file. The reader
/// and writer must be started, with the writer's session begun.
///
/// # Safety
///
/// The objects must be set up for each other and used only on this
/// thread while this runs.
pub unsafe fn transfer(
    reader: &objc2_av_foundation::AVAssetReader,
    writer: &objc2_av_foundation::AVAssetWriter,
    pairs: &[(
        &objc2_av_foundation::AVAssetReaderOutput,
        &objc2_av_foundation::AVAssetWriterInput,
    )],
    watermark: Option<&crate::watermark::Overlay>,
) -> Result<(), String> {
    use objc2_av_foundation::{AVAssetReaderStatus, AVAssetWriterStatus};
    let failed = |writer: &objc2_av_foundation::AVAssetWriter, what: &str| {
        // SAFETY: plain property read.
        unsafe { writer.error() }
            .map(|e| e.localizedDescription().to_string())
            .unwrap_or_else(|| what.to_string())
    };
    // SAFETY: per the function's contract.
    unsafe {
        let mut done = vec![false; pairs.len()];
        while !done.iter().all(|d| *d) {
            let mut progressed = false;
            for (i, (out, input)) in pairs.iter().enumerate() {
                if done[i] || !input.isReadyForMoreMediaData() {
                    continue;
                }
                progressed = true;
                match out.copyNextSampleBuffer() {
                    Some(sample) => {
                        // The first pair is the video.
                        if i == 0
                            && let Some(watermark) = watermark
                        {
                            watermark.draw_on_frame(&sample);
                        }
                        if !input.appendSampleBuffer(&sample) {
                            reader.cancelReading();
                            writer.cancelWriting();
                            return Err(failed(writer, "writing the video failed"));
                        }
                    }
                    None => {
                        input.markAsFinished();
                        done[i] = true;
                    }
                }
            }
            if !progressed {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
        if reader.status() == AVAssetReaderStatus::Failed {
            writer.cancelWriting();
            return Err(reader
                .error()
                .map(|e| e.localizedDescription().to_string())
                .unwrap_or_else(|| "reading the video failed".into()));
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let block = block2::RcBlock::new(move || {
            let _ = tx.send(());
        });
        writer.finishWritingWithCompletionHandler(&block);
        rx.recv_timeout(std::time::Duration::from_secs(600))
            .map_err(|_| "finishing the video timed out".to_string())?;
        if writer.status() == AVAssetWriterStatus::Completed {
            Ok(())
        } else {
            Err(failed(writer, "finishing the video failed"))
        }
    }
}
