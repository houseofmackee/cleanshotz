//! Exports an edited recording: the trimmed range as a composition (without
//! audio when muted), then to GIF if asked. Without crop, speed, quality
//! or a smaller size the samples are copied by `AVAssetExportSession`;
//! otherwise the frames are re-encoded with an asset reader and writer,
//! which control the crop, size and bitrate.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2_av_foundation::{
    AVAsset, AVAssetExportPreset960x540, AVAssetExportPreset1280x720, AVAssetExportPreset1920x1080,
    AVAssetExportPresetPassthrough, AVAssetExportSession, AVAssetExportSessionStatus, AVAssetTrack,
    AVFileTypeMPEG4, AVMediaTypeAudio, AVMediaTypeVideo, AVMutableComposition, AVURLAsset,
};
use objc2_core_media::{CMTime, CMTimeRange};
use objc2_foundation::{NSString, NSURL};

use super::plan::{Format, Plan, Preset};
use crate::record::media;

fn file_url(path: &Path) -> Retained<NSURL> {
    NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()))
}

fn time(seconds: f64) -> CMTime {
    // SAFETY: constructing a CMTime.
    unsafe { CMTime::with_seconds(seconds, 600) }
}

/// Runs the export described by `plan` from `source` into `output`
/// (replacing it). Blocks: call it off the main thread.
pub fn export(
    source: &Path,
    plan: &Plan,
    preset: Preset,
    gif: (u32, u32),
    output: &Path,
) -> Result<(), String> {
    let temp_dir = std::env::temp_dir().join("cleanshotz-video-export");
    std::fs::create_dir_all(&temp_dir).map_err(|e| e.to_string())?;
    let temp: PathBuf = temp_dir.join(format!("{}.mp4", std::process::id()));
    let _ = std::fs::remove_file(&temp);

    if plan.needs_reencode(media::size(source)) {
        export_reencoded(source, plan, &temp)?;
    } else {
        export_mp4(source, plan, preset, &temp)?;
    }
    let result = match plan.format {
        Format::Mp4 => replace(&temp, output),
        Format::Gif => {
            let (fps, width) = gif;
            media::to_gif(&temp, output, fps, width)
        }
    };
    let _ = std::fs::remove_file(&temp);
    result
}

/// Moves `from` over `to`, also across volumes.
fn replace(from: &Path, to: &Path) -> Result<(), String> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to)
        .map(|_| ())
        .map_err(|e| format!("{}: {e}", to.display()))
}

/// The trimmed (and, with a speed change, time-scaled) composition of
/// `source`, and its first source video track.
///
/// # Safety
///
/// AVFoundation objects, used on one thread.
unsafe fn composition(
    source: &Path,
    plan: &Plan,
) -> Result<
    (
        Retained<AVMutableComposition>,
        Option<Retained<AVAssetTrack>>,
    ),
    String,
> {
    // SAFETY: per the function's contract.
    unsafe {
        let asset = AVURLAsset::URLAssetWithURL_options(&file_url(source), None);
        let composition = AVMutableComposition::composition();
        let range = CMTimeRange {
            start: time(plan.start),
            duration: time(plan.duration()),
        };
        let video = AVMediaTypeVideo.ok_or("no video media type")?;
        let audio = AVMediaTypeAudio.ok_or("no audio media type")?;
        let mut kinds = vec![video];
        if !plan.mute {
            kinds.push(audio);
        }
        let mut source_video = None;
        for kind in kinds {
            #[allow(deprecated)]
            let tracks = asset.tracksWithMediaType(kind);
            for track in tracks.iter() {
                let Some(target) =
                    composition.addMutableTrackWithMediaType_preferredTrackID(kind, 0)
                else {
                    continue;
                };
                target
                    .insertTimeRange_ofTrack_atTime_error(range, &track, time(0.0))
                    .map_err(|e| e.localizedDescription().to_string())?;
                if kind == video {
                    target.setPreferredTransform(track.preferredTransform());
                    source_video.get_or_insert(track);
                }
            }
        }
        if (plan.speed - 1.0).abs() > 1e-6 {
            composition.scaleTimeRange_toDuration(
                CMTimeRange {
                    start: time(0.0),
                    duration: time(plan.duration()),
                },
                time(plan.output_duration()),
            );
        }
        Ok((composition, source_video))
    }
}

/// Re-encodes: crop and scale through a video composition, speed through
/// the composition's time scaling (audio keeps its pitch), and the bitrate
/// from the plan's quality.
fn export_reencoded(source: &Path, plan: &Plan, output: &Path) -> Result<(), String> {
    use objc2::runtime::ProtocolObject;
    use objc2_av_foundation::{
        AVAssetReader, AVAssetReaderAudioMixOutput, AVAssetReaderOutput,
        AVAssetReaderVideoCompositionOutput, AVAssetWriter, AVAssetWriterInput,
        AVMutableVideoComposition, AVMutableVideoCompositionInstruction,
        AVMutableVideoCompositionLayerInstruction, AVVideoCompositionInstructionProtocol,
        AVVideoCompositionLayerInstruction,
    };
    use objc2_core_foundation::{CGAffineTransform, CGSize};
    use objc2_foundation::{NSArray, NSDictionary, NSNumber};

    let source_size = media::size(source);
    // SAFETY: AVFoundation objects created and used on this thread.
    unsafe {
        let (composition, source_video) = composition(source, plan)?;
        let source_video = source_video.ok_or("the recording has no video")?;
        let video = AVMediaTypeVideo.ok_or("no video media type")?;
        let audio = AVMediaTypeAudio.ok_or("no audio media type")?;
        #[allow(deprecated)]
        let video_tracks = composition.tracksWithMediaType(video);
        #[allow(deprecated)]
        let audio_tracks = composition.tracksWithMediaType(audio);
        let video_track = video_tracks
            .firstObject()
            .ok_or("the recording has no video")?;
        // Reader outputs take plain asset tracks.
        let as_asset_tracks = |tracks: &NSArray<objc2_av_foundation::AVMutableCompositionTrack>| {
            let tracks: Vec<Retained<AVAssetTrack>> = tracks
                .iter()
                .map(|t| Retained::into_super(Retained::into_super(t)))
                .collect();
            NSArray::from_retained_slice(&tracks)
        };
        let (video_tracks, audio_tracks) = (
            as_asset_tracks(&video_tracks),
            as_asset_tracks(&audio_tracks),
        );

        let fps = f64::from(source_video.nominalFrameRate());
        let fps = if fps.is_finite() && fps > 0.0 {
            fps.min(120.0)
        } else {
            30.0
        };
        let (width, height) = plan.output_size(source_size);
        let crop = plan.crop.unwrap_or(crate::capture::geometry::Rect::new(
            0.0,
            0.0,
            source_size.0,
            source_size.1,
        ));
        let scale = width as f64 / crop.width;
        // The track's own orientation first, then the crop and scale.
        let p = source_video.preferredTransform();
        let transform = CGAffineTransform {
            a: p.a * scale,
            b: p.b * scale,
            c: p.c * scale,
            d: p.d * scale,
            tx: (p.tx - crop.x) * scale,
            ty: (p.ty - crop.y) * scale,
        };
        let layer = AVMutableVideoCompositionLayerInstruction::videoCompositionLayerInstructionWithAssetTrack(&video_track);
        layer.setTransform_atTime(transform, time(0.0));
        let instruction = AVMutableVideoCompositionInstruction::videoCompositionInstruction();
        instruction.setTimeRange(CMTimeRange {
            start: time(0.0),
            duration: composition.duration(),
        });
        let layer: Retained<AVVideoCompositionLayerInstruction> = Retained::into_super(layer);
        instruction.setLayerInstructions(&NSArray::from_retained_slice(&[layer]));
        let instruction: Retained<ProtocolObject<dyn AVVideoCompositionInstructionProtocol>> =
            ProtocolObject::from_retained(instruction);
        let video_composition = AVMutableVideoComposition::videoComposition();
        video_composition.setRenderSize(CGSize::new(width as f64, height as f64));
        video_composition.setFrameDuration(CMTime::with_seconds(1.0 / fps, 600));
        video_composition.setInstructions(&NSArray::from_retained_slice(&[instruction]));

        let number =
            |n: f64| -> Retained<objc2::runtime::AnyObject> { NSNumber::new_f64(n).into() };
        let dict = |pairs: &[(&str, Retained<objc2::runtime::AnyObject>)]| {
            let keys: Vec<Retained<NSString>> =
                pairs.iter().map(|(k, _)| NSString::from_str(k)).collect();
            let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
            let values: Vec<_> = pairs.iter().map(|(_, v)| v.clone()).collect();
            NSDictionary::from_retained_objects(&key_refs, &values)
        };

        let reader = AVAssetReader::assetReaderWithAsset_error(&composition)
            .map_err(|e| e.localizedDescription().to_string())?;
        const BGRA: f64 = 0x4247_5241 as f64; // 'BGRA'
        let frames_out = AVAssetReaderVideoCompositionOutput::assetReaderVideoCompositionOutputWithVideoTracks_videoSettings(
            &video_tracks,
            Some(&dict(&[("PixelFormatType", number(BGRA))])),
        );
        frames_out.setVideoComposition(Some(&video_composition));
        frames_out.setAlwaysCopiesSampleData(false);
        reader.addOutput(&frames_out);
        const LPCM: f64 = 0x6c70_636d as f64; // 'lpcm'
        let sound_out = (audio_tracks.count() > 0).then(|| {
            let out =
                AVAssetReaderAudioMixOutput::assetReaderAudioMixOutputWithAudioTracks_audioSettings(
                    &audio_tracks,
                    Some(&dict(&[
                        ("AVFormatIDKey", number(LPCM)),
                        ("AVSampleRateKey", number(48_000.0)),
                        ("AVNumberOfChannelsKey", number(2.0)),
                        ("AVLinearPCMBitDepthKey", number(32.0)),
                        ("AVLinearPCMIsFloatKey", number(1.0)),
                        ("AVLinearPCMIsNonInterleaved", number(0.0)),
                    ])),
                );
            reader.addOutput(&out);
            out
        });

        let writer = AVAssetWriter::assetWriterWithURL_fileType_error(
            &file_url(output),
            AVFileTypeMPEG4.ok_or("no MPEG-4 file type")?,
        )
        .map_err(|e| e.localizedDescription().to_string())?;
        writer.setShouldOptimizeForNetworkUse(true);
        // H.264 tops out at 4096 x 2304.
        let codec = if width <= 4096 && height <= 2304 {
            "avc1"
        } else {
            "hvc1"
        };
        let compression = dict(&[
            (
                "AverageBitRate",
                number(plan.bitrate(source_size, fps) as f64),
            ),
            ("ExpectedFrameRate", number(fps)),
            ("MaxKeyFrameInterval", number((fps * 2.0).round())),
        ]);
        let frames_in = AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(
            video,
            Some(&dict(&[
                ("AVVideoCodecKey", NSString::from_str(codec).into()),
                ("AVVideoWidthKey", number(width as f64)),
                ("AVVideoHeightKey", number(height as f64)),
                ("AVVideoCompressionPropertiesKey", compression.into()),
            ])),
        );
        const AAC: f64 = 0x6161_6320 as f64; // 'aac '
        let sound_in = sound_out.as_ref().map(|_| {
            AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(
                audio,
                Some(&dict(&[
                    ("AVFormatIDKey", number(AAC)),
                    ("AVSampleRateKey", number(48_000.0)),
                    ("AVNumberOfChannelsKey", number(2.0)),
                    ("AVEncoderBitRateKey", number(192_000.0)),
                ])),
            )
        });
        for input in std::iter::once(&frames_in).chain(sound_in.as_ref()) {
            if !writer.canAddInput(input) {
                return Err("these export settings are not supported".into());
            }
            writer.addInput(input);
        }
        if !reader.startReading() {
            return Err(reader
                .error()
                .map(|e| e.localizedDescription().to_string())
                .unwrap_or_else(|| "could not read the recording".into()));
        }
        if !writer.startWriting() {
            return Err(writer
                .error()
                .map(|e| e.localizedDescription().to_string())
                .unwrap_or_else(|| "could not start the export".into()));
        }
        writer.startSessionAtSourceTime(time(0.0));
        let mut pairs: Vec<(&AVAssetReaderOutput, &AVAssetWriterInput)> =
            vec![(&frames_out, &frames_in)];
        if let (Some(out), Some(input)) = (&sound_out, &sound_in) {
            pairs.push((out, input));
        }
        media::transfer(&reader, &writer, &pairs)
    }
}

fn export_mp4(source: &Path, plan: &Plan, preset: Preset, output: &Path) -> Result<(), String> {
    // SAFETY: AVFoundation calls on objects created and used on this thread.
    unsafe {
        let (composition, _) = composition(source, plan)?;
        let preset_name = match preset {
            Preset::Passthrough => AVAssetExportPresetPassthrough,
            Preset::P1080 => AVAssetExportPreset1920x1080,
            Preset::P720 => AVAssetExportPreset1280x720,
            Preset::P540 => AVAssetExportPreset960x540,
        };
        let composition: &AVAsset = &composition;
        let session =
            AVAssetExportSession::exportSessionWithAsset_presetName(composition, preset_name)
                .ok_or("this video cannot be exported with these settings")?;
        session.setOutputURL(Some(&file_url(output)));
        session.setOutputFileType(AVFileTypeMPEG4);
        session.setShouldOptimizeForNetworkUse(true);

        let (tx, rx) = mpsc::sync_channel(1);
        let block = RcBlock::new(move || {
            let _ = tx.send(());
        });
        session.exportAsynchronouslyWithCompletionHandler(&block);
        rx.recv_timeout(Duration::from_secs(600))
            .map_err(|_| "the export timed out".to_string())?;
        if session.status() == AVAssetExportSessionStatus::Completed {
            Ok(())
        } else {
            Err(session
                .error()
                .map(|e| e.localizedDescription().to_string())
                .unwrap_or_else(|| "the export failed".into()))
        }
    }
}
