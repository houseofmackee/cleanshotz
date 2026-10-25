//! Studio recordings: the screen recorded without the cursor, with the
//! cursor, clicks, keys and camera kept as data, so zooms, cursor size,
//! backgrounds and the camera can be changed after recording. A recording
//! is a `.cszrec` package (see [`package`]).

mod captions;
pub mod capture;
pub mod compositor;
pub mod events;
pub mod media;
pub mod package;
mod pick;
pub mod project;
pub mod scene;
pub mod timeline;
mod track;
mod window;
pub mod zoom;

use std::path::Path;

use objc2::MainThreadMarker;

/// A Studio recording was saved: adds it to History and opens it for
/// editing.
pub fn recorded(mtm: MainThreadMarker, package: &Path) {
    let screen = package::screen(package);
    if let Some(thumbnail) = crate::record::media::thumbnail(&screen) {
        let duration = crate::record::media::duration(&screen);
        crate::history::record_video(package.to_path_buf(), thumbnail, duration);
    }
    open(mtm, package);
}

/// A package's events and edits, and its screen video's size and length.
pub struct Loaded {
    pub events: events::Events,
    pub project: project::Project,
    pub video: (f64, f64),
    pub duration: f64,
}

/// Loads a package; the first time, makes its automatic zooms.
pub fn load(package: &Path) -> Result<Loaded, String> {
    let events = package::load_events(package)?;
    let mut project = package::load_project(package);
    let (w, h, duration) = media::screen_info(package);
    if w <= 0.0 || h <= 0.0 {
        return Err(format!("{} has no screen video", package.display()));
    }
    if !project.zooms_made {
        let scale = if events.scale > 0.0 {
            events.scale
        } else {
            1.0
        };
        project.zooms = zoom::automatic(&events, (w / scale, h / scale), duration);
        project.zooms_made = true;
        // The default watermark for new recordings.
        let watermark = crate::watermark::settings();
        if watermark.recordings {
            project.watermark = Some(watermark.style);
        }
        if let Err(e) = package::save_project(package, &project) {
            tracing::warn!(error = %e, "studio project not saved");
        }
    }
    Ok(Loaded {
        events,
        project,
        video: (w, h),
        duration,
    })
}

/// Opens a `.cszrec` package in the Studio editor.
pub fn open(mtm: MainThreadMarker, package: &Path) {
    let id = registry::next_id();
    match window::StudioEditor::open(mtm, id, package) {
        Ok(editor) => registry::add(editor),
        Err(e) => {
            tracing::error!(error = %e, path = %package.display(), "studio recording not opened");
            crate::app::alert::show(mtm, "Could not open the recording", &e, &["OK"]);
        }
    }
}

/// Moves something AVFoundation and Core Image treat as thread-safe (the
/// renderer, images) into a background closure.
pub(crate) struct Shared<T>(pub T);
// SAFETY: only used for the renderer (Send + Sync) and CGImages, which
// are immutable and thread-safe.
unsafe impl<T> Send for Shared<T> {}

mod registry {
    use std::cell::{Cell, RefCell};

    use objc2::DefinedClass;
    use objc2::rc::Retained;

    use super::window::StudioEditor;

    thread_local! {
        static EDITORS: RefCell<Vec<Retained<StudioEditor>>> = const { RefCell::new(Vec::new()) };
        static NEXT_ID: Cell<u64> = const { Cell::new(0) };
    }

    pub fn next_id() -> u64 {
        NEXT_ID.with(|n| {
            n.set(n.get() + 1);
            n.get()
        })
    }

    pub fn add(editor: Retained<StudioEditor>) {
        EDITORS.with(|e| e.borrow_mut().push(editor));
    }

    pub fn get(id: u64) -> Option<Retained<StudioEditor>> {
        EDITORS.with(|e| {
            e.borrow()
                .iter()
                .find(|c| c.ivars().id_value() == id)
                .cloned()
        })
    }

    /// Releases a closed editor once the current event is done with it.
    pub fn remove(id: u64) {
        dispatch2::DispatchQueue::main().exec_async(move || {
            EDITORS.with(|e| e.borrow_mut().retain(|c| c.ivars().id_value() != id));
        });
    }
}

/// Debug builds: renders a package with its project (or the defaults) to
/// an MP4, to check the compositor without the editor.
#[cfg(debug_assertions)]
pub fn export_sample(package: &Path, output: &Path) -> Result<(), String> {
    let Loaded {
        events,
        project,
        video,
        duration,
    } = load(package)?;
    let renderer = std::sync::Arc::new(compositor::Renderer::new(
        package, &events, &project, video, duration,
    )?);
    let started = std::time::Instant::now();
    media::export(package, renderer, output)?;
    println!(
        "rendered {duration:.1} s in {:.1} s to {}",
        started.elapsed().as_secs_f64(),
        output.display()
    );
    Ok(())
}
