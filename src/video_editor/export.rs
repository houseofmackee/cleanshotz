//! Exports an edited recording: the trimmed range as a composition (without
//! audio when muted), through `AVAssetExportSession`, then to GIF if asked.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2_av_foundation::{
    AVAsset, AVAssetExportPreset960x540, AVAssetExportPreset1280x720, AVAssetExportPreset1920x1080,
    AVAssetExportPresetPassthrough, AVAssetExportSession, AVAssetExportSessionStatus,
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

    export_mp4(source, plan, preset, &temp)?;
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

fn export_mp4(source: &Path, plan: &Plan, preset: Preset, output: &Path) -> Result<(), String> {
    // SAFETY: AVFoundation calls on objects created and used on this thread.
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
                }
            }
        }

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
