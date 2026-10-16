//! Small pieces of app state that persist across launches but are not user
//! settings (kept out of config.toml so it stays hand-editable).

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::capture::geometry::Rect;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub last_area: Option<SavedArea>,
    /// The annotation color, stroke width and arrow style last chosen in
    /// an editor.
    pub editor_color: Option<crate::editor::model::Color>,
    pub editor_width: Option<f64>,
    pub editor_arrow_style: Option<crate::editor::model::ArrowStyle>,
}

/// The last selected area, for Capture Previous Area.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SavedArea {
    pub display_id: u32,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl SavedArea {
    pub fn new(display_id: u32, rect: Rect) -> Self {
        Self {
            display_id,
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        }
    }

    pub fn rect(&self) -> Rect {
        Rect::new(self.x, self.y, self.width, self.height)
    }
}

impl State {
    /// Missing or unreadable state is not an error; it just starts empty.
    pub fn load(path: &Path) -> State {
        fs::read_to_string(path)
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) {
        let text = toml::to_string_pretty(self).expect("state is always serializable");
        if let Err(e) = path
            .parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| fs::write(path, text))
        {
            tracing::warn!(error = %e, path = %path.display(), "could not save state");
        }
    }
}

pub fn state_path() -> PathBuf {
    crate::config::app_support_dir().join("state.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_tolerates_garbage() {
        let dir = std::env::temp_dir().join(format!("cleanshotz-state-{}", std::process::id()));
        let path = dir.join("state.toml");
        let state = State {
            last_area: Some(SavedArea::new(7, Rect::new(1.0, 2.0, 30.0, 40.0))),
            editor_color: Some(crate::editor::model::Color::rgb(0.2, 0.4, 1.0)),
            editor_width: Some(6.0),
            editor_arrow_style: Some(crate::editor::model::ArrowStyle::Open),
        };
        state.save(&path);
        assert_eq!(State::load(&path), state);
        fs::write(&path, "last_area = 5").unwrap();
        assert_eq!(State::load(&path), State::default());
        fs::remove_dir_all(&dir).unwrap();
    }
}
