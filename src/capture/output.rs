//! What happens with a finished capture: sound, file, clipboard.

use std::cell::OnceCell;
use std::fs;
use std::path::PathBuf;

use objc2::rc::Retained;
use objc2::{AllocAnyThread, MainThreadMarker};
use objc2_app_kit::{NSPasteboard, NSPasteboardTypePNG, NSSound};
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::CGImage;
use objc2_foundation::{NSData, NSString};

use super::{image, naming};
use crate::config::{CaptureSettings, ImageFormat};

/// The macOS screenshot sound. CleanShot's own sounds are not used.
const SHUTTER_SOUND: &str = "/System/Library/Components/CoreAudio.component/Contents/SharedSupport/SystemSounds/system/Screen Capture.aif";

pub struct Captured {
    pub image: CFRetained<CGImage>,
    /// Pixels per point of `image` (2.0 for an unscaled Retina capture).
    pub scale: f64,
}

/// Plays the shutter sound, then encodes, saves and copies the capture on a
/// background thread so large captures do not block the UI.
pub fn deliver(mtm: MainThreadMarker, captured: Captured, settings: &CaptureSettings) {
    if settings.play_sound {
        play_shutter(mtm);
    }
    let settings = settings.clone();
    std::thread::spawn(move || {
        let (width, height) = image::pixel_size(&captured.image);
        let mut png = None;
        if settings.save_to_file {
            match save(&captured, &settings, &mut png) {
                Ok(path) => tracing::info!(path = %path.display(), width, height, "capture saved"),
                Err(e) => tracing::error!(error = %e, "could not save capture"),
            }
        }
        if settings.copy_to_clipboard {
            let png = png.or_else(|| image::encode(&captured.image, ImageFormat::Png, 1.0));
            match png {
                Some(png) => {
                    copy_png(&png);
                    tracing::info!(width, height, "capture copied to clipboard");
                }
                None => tracing::error!("could not encode capture for the clipboard"),
            }
        }
    });
}

/// Saves the capture. If it was encoded as PNG, the bytes are left in
/// `png` so the clipboard can reuse them.
fn save(
    captured: &Captured,
    settings: &CaptureSettings,
    png: &mut Option<Vec<u8>>,
) -> Result<PathBuf, String> {
    let bytes = image::encode(&captured.image, settings.format, settings.jpeg_quality())
        .ok_or("could not encode the image")?;
    let dir = naming::expand_home(&settings.save_directory);
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let stem = naming::file_stem(
        &settings.file_name,
        &chrono::Local::now(),
        captured.scale > 1.0,
    );
    let path = naming::unique_path(&dir, &stem, settings.format.extension());
    fs::write(&path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    if settings.format == ImageFormat::Png {
        *png = Some(bytes);
    }
    Ok(path)
}

fn copy_png(png: &[u8]) {
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    // SAFETY: reading an AppKit constant.
    let png_type = unsafe { NSPasteboardTypePNG };
    pasteboard.setData_forType(Some(&NSData::with_bytes(png)), png_type);
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
