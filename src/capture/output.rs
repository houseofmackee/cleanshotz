//! What happens with a finished capture: sound, file, clipboard, then the
//! Quick Access Overlay.

use std::cell::OnceCell;
use std::fs;
use std::path::{Path, PathBuf};

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::{AllocAnyThread, MainThreadMarker};
use objc2_app_kit::{NSPasteboard, NSPasteboardTypePNG, NSSound};
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::CGImage;
use objc2_foundation::{NSData, NSString};

use super::geometry::Rect;
use super::{image, naming};
use crate::config::{CaptureSettings, ImageFormat};

/// The macOS screenshot sound. CleanShot's own sounds are not used.
const SHUTTER_SOUND: &str = "/System/Library/Components/CoreAudio.component/Contents/SharedSupport/SystemSounds/system/Screen Capture.aif";

pub struct Captured {
    pub image: CFRetained<CGImage>,
    /// Pixels per point of `image` (2.0 for an unscaled Retina capture).
    pub scale: f64,
    /// Where the captured content was on screen, in global CoreGraphics
    /// points (top-left origin). Pinned screenshots open there.
    pub source: Option<Rect>,
}

/// A capture after saving and copying, ready for Quick Access.
pub struct Delivered {
    pub image: CFRetained<CGImage>,
    pub scale: f64,
    pub source: Option<Rect>,
    /// The saved file, or a temporary PNG if saving is turned off, so the
    /// capture can always be dragged out as a file.
    pub file: PathBuf,
    pub saved: bool,
    /// PNG bytes, when they were already encoded (for the clipboard).
    pub png: Option<Vec<u8>>,
}

/// Plays the shutter sound, then encodes, saves and copies the capture on a
/// background thread so large captures do not block the UI. Finally shows
/// it in the Quick Access Overlay.
pub fn deliver(mtm: MainThreadMarker, captured: Captured, settings: &CaptureSettings) {
    if settings.play_sound {
        play_shutter(mtm);
    }
    let settings = settings.clone();
    std::thread::spawn(move || {
        let Some(delivered) = process(captured, &settings) else {
            return;
        };
        DispatchQueue::main().exec_async(move || {
            let mtm = MainThreadMarker::new().expect("the main queue runs on the main thread");
            crate::history::record_image(delivered.image.clone(), delivered.file.clone());
            if crate::upload::auto_upload() {
                crate::upload::start(mtm, delivered.file.clone());
            }
            crate::quick_access::add(mtm, delivered);
        });
    });
}

fn process(captured: Captured, settings: &CaptureSettings) -> Option<Delivered> {
    let (width, height) = image::pixel_size(&captured.image);
    let mut png = None;
    let mut saved = None;
    if settings.save_to_file {
        match save_to_directory(&captured.image, captured.scale, settings, None) {
            Ok((path, bytes)) => {
                tracing::info!(path = %path.display(), width, height, "capture saved");
                if settings.format == ImageFormat::Png {
                    png = Some(bytes);
                }
                saved = Some(path);
            }
            Err(e) => tracing::error!(error = %e, "could not save capture"),
        }
    }
    if settings.copy_to_clipboard || saved.is_none() {
        png = png.or_else(|| image::encode(&captured.image, ImageFormat::Png, 1.0));
    }
    if settings.copy_to_clipboard {
        match &png {
            Some(png) => {
                copy_png(png);
                tracing::info!(width, height, "capture copied to clipboard");
            }
            None => tracing::error!("could not encode capture for the clipboard"),
        }
    }
    let file = match &saved {
        Some(path) => path.clone(),
        None => {
            let stem = naming::file_stem(
                &settings.file_name,
                &chrono::Local::now(),
                captured.scale > 1.0,
            );
            let dir = unsaved_dir();
            let path = naming::unique_path(&dir, &stem, "png");
            let written = fs::create_dir_all(&dir)
                .and_then(|()| fs::write(&path, png.as_deref().unwrap_or_default()));
            if let Err(e) = written {
                tracing::error!(error = %e, "could not write temporary capture file");
                return None;
            }
            path
        }
    };
    Some(Delivered {
        image: captured.image,
        scale: captured.scale,
        source: captured.source,
        file,
        saved: saved.is_some(),
        png,
    })
}

/// Saves into the configured directory, named `stem` or by the configured
/// file name template. Returns the path and the encoded bytes.
pub fn save_to_directory(
    image: &CGImage,
    scale: f64,
    settings: &CaptureSettings,
    stem: Option<&str>,
) -> Result<(PathBuf, Vec<u8>), String> {
    let dir = naming::expand_home(&settings.save_directory);
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let stem = match stem {
        Some(stem) => stem.to_string(),
        None => naming::file_stem(&settings.file_name, &chrono::Local::now(), scale > 1.0),
    };
    let path = naming::unique_path(&dir, &stem, settings.format.extension());
    let bytes = save_to(&path, image, settings.format, settings.jpeg_quality())?;
    Ok((path, bytes))
}

/// Encodes and writes `image` to `path`. Returns the encoded bytes.
pub fn save_to(
    path: &Path,
    image: &CGImage,
    format: ImageFormat,
    quality: f64,
) -> Result<Vec<u8>, String> {
    let bytes = image::encode(image, format, quality).ok_or("could not encode the image")?;
    fs::write(path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(bytes)
}

pub fn copy_png(png: &[u8]) {
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    // SAFETY: reading an AppKit constant.
    let png_type = unsafe { NSPasteboardTypePNG };
    pasteboard.setData_forType(Some(&NSData::with_bytes(png)), png_type);
}

/// Temporary files for captures that were not saved:
/// `~/Library/Caches/CleanShotZ/Quick Access`.
pub fn temp_dir() -> PathBuf {
    naming::expand_home("~/Library/Caches/CleanShotZ/Quick Access")
}

/// Where captures that are not saved go: the history folder while history
/// is on (kept as long as history keeps them), otherwise the temp folder.
pub fn unsaved_dir() -> PathBuf {
    if crate::history::settings().enabled {
        crate::history::files_dir()
    } else {
        temp_dir()
    }
}

/// Removes temporary capture files left from a previous run.
pub fn clear_temp_dir() {
    let dir = temp_dir();
    if dir.exists()
        && let Err(e) = fs::remove_dir_all(&dir)
    {
        tracing::warn!(error = %e, "could not clear temporary captures");
    }
}

thread_local! {
    static SHUTTER: OnceCell<Option<Retained<NSSound>>> = const { OnceCell::new() };
}

fn play_shutter(_mtm: MainThreadMarker) {
    SHUTTER.with(|cell| {
        let sound = cell.get_or_init(|| {
            NSSound::initWithContentsOfFile_byReference(
                NSSound::alloc(),
                &NSString::from_str(SHUTTER_SOUND),
                true,
            )
        });
        if let Some(sound) = sound {
            sound.stop();
            sound.play();
        }
    });
}
