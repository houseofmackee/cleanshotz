//! The video editor: trim, mute, resize and convert recordings.

pub mod export;
pub mod plan;
mod window;

use std::cell::RefCell;
use std::path::Path;

use objc2::MainThreadMarker;

use crate::config::RecordingSettings;

thread_local! {
    static SETTINGS: RefCell<RecordingSettings> = RefCell::new(RecordingSettings::default());
}

/// The recording settings (GIF frame rate and width) to export with.
pub fn configure(settings: RecordingSettings) {
    SETTINGS.with(|s| *s.borrow_mut() = settings);
}

/// Whether `path` looks like a video the editor can open.
pub fn is_video(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "mp4" | "mov" | "m4v"))
}

pub fn open(mtm: MainThreadMarker, path: &Path) {
    let (fps, width) = SETTINGS.with(|s| {
        let s = s.borrow();
        (s.gif_fps(), s.gif_max_width)
    });
    let id = registry::next_id();
    let editor = window::VideoEditor::open(mtm, id, path, fps, width);
    registry::add(editor);
}

/// Moves a value into a background closure (thumbnails are CGImages,
/// which are immutable and thread-safe).
pub(crate) struct AssertSend<T>(pub T);
// SAFETY: see the type's documentation.
unsafe impl<T> Send for AssertSend<T> {}
impl<T> AssertSend<T> {
    pub fn into_inner(self) -> T {
        self.0
    }
}

mod registry {
    use std::cell::{Cell, RefCell};

    use objc2::DefinedClass;
    use objc2::rc::Retained;

    use super::window::VideoEditor;

    thread_local! {
        static EDITORS: RefCell<Vec<Retained<VideoEditor>>> = const { RefCell::new(Vec::new()) };
        static NEXT_ID: Cell<u64> = const { Cell::new(0) };
    }

    pub fn next_id() -> u64 {
        NEXT_ID.with(|n| {
            n.set(n.get() + 1);
            n.get()
        })
    }

    pub fn add(editor: Retained<VideoEditor>) {
        EDITORS.with(|e| e.borrow_mut().push(editor));
    }

    pub fn get(id: u64) -> Option<Retained<VideoEditor>> {
        EDITORS.with(|e| e.borrow().iter().find(|c| c.ivars().id == id).cloned())
    }

    /// Releases a closed editor once the current event is done with it.
    pub fn remove(id: u64) {
        dispatch2::DispatchQueue::main().exec_async(move || {
            EDITORS.with(|e| e.borrow_mut().retain(|c| c.ivars().id != id));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_videos() {
        assert!(is_video(Path::new("a/b.MP4")));
        assert!(is_video(Path::new("x.mov")));
        assert!(!is_video(Path::new("x.gif")));
        assert!(!is_video(Path::new("x")));
    }
}
