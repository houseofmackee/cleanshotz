//! The `.cszrec` package: a folder macOS shows as one file.
//!
//! ```text
//! Name.cszrec/
//!   screen.mp4     the screen, without the cursor
//!   camera.mov     the camera, when it was on
//!   events.json    cursor, clicks, keys (see `events`)
//!   cursors/*.png  the cursor images
//!   project.json   the edits
//!   watermark.png  the watermark's mark, when it has one (a copy, in case
//!                  the logo file moves)
//! ```

use std::path::{Path, PathBuf};

use super::events::Events;

pub const EXTENSION: &str = "cszrec";
pub const SCREEN: &str = "screen.mp4";
pub const CAMERA: &str = "camera.mov";
pub const EVENTS: &str = "events.json";
pub const PROJECT: &str = "project.json";
pub const WATERMARK: &str = "watermark.png";

pub fn is_package(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(EXTENSION))
}

pub fn screen(package: &Path) -> PathBuf {
    package.join(SCREEN)
}

pub fn load_events(package: &Path) -> Result<Events, String> {
    let bytes =
        std::fs::read(package.join(EVENTS)).map_err(|e| format!("{}: {e}", package.display()))?;
    let events: Events = serde_json::from_slice(&bytes)
        .map_err(|e| format!("{} is damaged: {e}", package.display()))?;
    if events.version > super::events::VERSION {
        return Err(format!(
            "{} was made by a newer version of CleanShotZ",
            package.display()
        ));
    }
    Ok(events)
}

/// The edits, or the defaults when there are none yet (or they cannot be
/// read).
pub fn load_project(package: &Path) -> super::project::Project {
    std::fs::read(package.join(PROJECT))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save_project(package: &Path, project: &super::project::Project) -> Result<(), String> {
    let json = serde_json::to_vec_pretty(project).map_err(|e| e.to_string())?;
    std::fs::write(package.join(PROJECT), json).map_err(|e| e.to_string())
}
