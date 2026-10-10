//! Capture history: every capture and recording, searchable by the text in
//! it, kept for a configurable number of days.

pub mod store;
mod window;

pub use window::open as open_window;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use objc2_core_foundation::CFRetained;
use objc2_core_graphics::CGImage;

use self::store::{Kind, NewEntry, Store};
use crate::capture::image;
use crate::config::{HistorySettings, ImageFormat};

static SETTINGS: Mutex<HistorySettings> = Mutex::new(HistorySettings::DEFAULT);

/// Applies settings; callable from any thread.
pub fn configure(settings: HistorySettings) {
    if let Ok(mut s) = SETTINGS.lock() {
        *s = settings;
    }
}

pub fn settings() -> HistorySettings {
    SETTINGS.lock().map(|s| *s).unwrap_or_default()
}

pub fn db_path() -> PathBuf {
    crate::config::app_support_dir().join("history.sqlite")
}

/// Where captures that were not saved are kept while history lists them.
pub fn files_dir() -> PathBuf {
    crate::config::app_support_dir().join("History")
}

pub fn open_store() -> Option<Store> {
    Store::open(&db_path())
        .inspect_err(|e| tracing::error!(error = %e, "could not open history"))
        .ok()
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// A small PNG preview, at most 400 x 300 pixels.
fn thumbnail(image: &CGImage) -> Vec<u8> {
    let (w, h) = image::pixel_size(image);
    let scale = (400.0 / w as f64).min(300.0 / h as f64).min(1.0);
    let small = if scale < 1.0 {
        image::resize(
            image,
            ((w as f64 * scale).round() as usize).max(1),
            ((h as f64 * scale).round() as usize).max(1),
        )
    } else {
        None
    };
    image::encode(small.as_deref().unwrap_or(image), ImageFormat::Png, 1.0).unwrap_or_default()
}

/// Records a screenshot, then recognizes its text for search. Runs in the
/// background; returns at once.
pub fn record_image(image: CFRetained<CGImage>, path: PathBuf) {
    let settings = settings();
    if !settings.enabled {
        return;
    }
    let image = crate::video_editor::AssertSend(image);
    std::thread::spawn(move || {
        let image = image.into_inner();
        let Some(store) = open_store() else {
            return;
        };
        let (w, h) = image::pixel_size(&image);
        let entry = NewEntry {
            kind: Kind::Image,
            path,
            created: now(),
            width: w as u32,
            height: h as u32,
            duration: None,
            thumbnail: thumbnail(&image),
        };
        let id = match store.add(&entry) {
            Ok(id) => id,
            Err(e) => return tracing::error!(error = %e, "could not record capture in history"),
        };
        changed();
        if settings.recognize_text {
            let text_settings = crate::ocr::settings();
            match crate::ocr::recognize(&image, &text_settings, |_| Vec::new()) {
                Ok(lines) => {
                    let text = crate::ocr::assemble(&lines, false);
                    if let Err(e) = store.set_text(id, &text) {
                        tracing::warn!(error = %e, "could not index capture text");
                    }
                }
                Err(e) => tracing::warn!(error = %e, "could not recognize capture text"),
            }
        }
    });
}

/// Records a recording (MP4 or GIF) with its first-frame thumbnail.
pub fn record_video(path: PathBuf, thumbnail_image: CFRetained<CGImage>, duration: f64) {
    if !settings().enabled {
        return;
    }
    let kind = if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("gif"))
    {
        Kind::Gif
    } else {
        Kind::Video
    };
    let image = crate::video_editor::AssertSend(thumbnail_image);
    std::thread::spawn(move || {
        let image = image.into_inner();
        let Some(store) = open_store() else {
            return;
        };
        let (w, h) = image::pixel_size(&image);
        let entry = NewEntry {
            kind,
            path,
            created: now(),
            width: w as u32,
            height: h as u32,
            duration: Some(duration),
            thumbnail: thumbnail(&image),
        };
        match store.add(&entry) {
            Ok(_) => changed(),
            Err(e) => tracing::error!(error = %e, "could not record recording in history"),
        }
    });
}

/// After `path` was edited in place: refreshes its thumbnail (and text).
pub fn refresh(path: PathBuf, image: CFRetained<CGImage>, duration: Option<f64>) {
    if !settings().enabled {
        return;
    }
    let image = crate::video_editor::AssertSend(image);
    std::thread::spawn(move || {
        let image = image.into_inner();
        let Some(store) = open_store() else {
            return;
        };
        let (w, h) = image::pixel_size(&image);
        match store.update_media(&path, &thumbnail(&image), w as u32, h as u32, duration) {
            Ok(Some(id)) => {
                changed();
                if duration.is_none() && settings().recognize_text {
                    let text_settings = crate::ocr::settings();
                    if let Ok(lines) = crate::ocr::recognize(&image, &text_settings, |_| Vec::new())
                    {
                        let _ = store.set_text(id, &crate::ocr::assemble(&lines, false));
                    }
                }
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %e, "could not refresh history entry"),
        }
    });
}

/// Removes entries older than the retention period, and their files when
/// history owns them (unsaved captures). Saved files are left alone.
pub fn prune() {
    let settings = settings();
    if settings.retention_days == 0 {
        return;
    }
    std::thread::spawn(move || {
        let Some(store) = open_store() else {
            return;
        };
        let cutoff = now() - settings.retention_days as i64 * 86_400;
        let expired = store.older_than(cutoff).unwrap_or_default();
        let owned = files_dir();
        for (id, path) in &expired {
            let _ = store.delete(*id);
            if path.starts_with(&owned) {
                let _ = std::fs::remove_file(path);
            }
        }
        if !expired.is_empty() {
            tracing::info!(count = expired.len(), "pruned old history entries");
        }
    });
}

/// Remembers where `path` was uploaded.
pub fn set_link(path: PathBuf, link: String) {
    std::thread::spawn(move || {
        if let Some(store) = open_store()
            && let Err(e) = store.set_link(&path, &link)
        {
            tracing::warn!(error = %e, "could not save the upload link");
        }
        changed();
    });
}

/// Whether deleting the entry should also delete its file.
pub fn owns_file(path: &Path) -> bool {
    path.starts_with(files_dir())
}

/// Tells an open History window to reload.
fn changed() {
    dispatch2::DispatchQueue::main().exec_async(|| {
        if let Some(mtm) = objc2::MainThreadMarker::new() {
            window::reload_if_open(mtm);
        }
    });
}
