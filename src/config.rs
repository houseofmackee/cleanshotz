//! User settings, stored as TOML in the application support directory.

use std::path::{Path, PathBuf};
use std::{fs, io};

use serde::{Deserialize, Serialize};

use crate::action::Action;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub general: General,
    pub capture: CaptureSettings,
    pub hotkeys: Hotkeys,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    /// Set once the first-launch permission walkthrough has been shown.
    pub onboarding_completed: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageFormat {
    #[default]
    Png,
    Jpeg,
}

impl ImageFormat {
    pub fn extension(self) -> &'static str {
        match self {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpg",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptureSettings {
    /// Copy each capture to the clipboard (always as PNG).
    pub copy_to_clipboard: bool,
    /// Save each capture to `save_directory`.
    pub save_to_file: bool,
    pub save_directory: String,
    /// File name without extension. Tokens: `{date}`, `{time}`.
    pub file_name: String,
    pub format: ImageFormat,
    /// JPEG quality from 0.0 to 1.0.
    pub jpeg_quality: f64,
    /// Save Retina captures at 1x (point) resolution.
    pub downscale_retina: bool,
    /// Include the mouse pointer in fullscreen and area captures.
    pub show_cursor: bool,
    /// Freeze the screen while selecting an area, so the capture matches
    /// exactly what was on screen when the selection started.
    pub freeze_screen: bool,
    /// Leave desktop icons and widgets out of captures.
    pub hide_desktop_icons: bool,
    /// Include the window shadow (on a transparent background) in window captures.
    pub window_shadow: bool,
    pub show_magnifier: bool,
    pub self_timer_seconds: u32,
    pub play_sound: bool,
}

impl Default for CaptureSettings {
    fn default() -> Self {
        Self {
            copy_to_clipboard: true,
            save_to_file: true,
            save_directory: "~/Desktop".into(),
            file_name: "CleanShotZ {date} at {time}".into(),
            format: ImageFormat::Png,
            jpeg_quality: 0.9,
            downscale_retina: false,
            show_cursor: false,
            freeze_screen: false,
            hide_desktop_icons: false,
            window_shadow: true,
            show_magnifier: true,
            self_timer_seconds: 5,
            play_sound: true,
        }
    }
}

impl CaptureSettings {
    pub fn jpeg_quality(&self) -> f64 {
        self.jpeg_quality.clamp(0.0, 1.0)
    }

    pub fn self_timer_seconds(&self) -> u32 {
        self.self_timer_seconds.clamp(1, 60)
    }
}

/// Global shortcuts. An empty string disables the shortcut.
///
/// The defaults use Option+Shift so they do not collide with the system
/// screenshot shortcuts (Cmd+Shift+3/4/5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hotkeys {
    pub capture_area: String,
    pub capture_previous_area: String,
    pub capture_fullscreen: String,
    pub capture_window: String,
    pub self_timer: String,
    pub capture_scrolling: String,
    pub capture_text: String,
    pub record_screen: String,
    pub open_history: String,
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self {
            capture_area: "alt+shift+4".into(),
            capture_previous_area: "alt+shift+2".into(),
            capture_fullscreen: "alt+shift+3".into(),
            capture_window: "alt+shift+w".into(),
            self_timer: String::new(),
            capture_scrolling: "alt+shift+s".into(),
            capture_text: "alt+shift+o".into(),
            record_screen: "alt+shift+5".into(),
            open_history: String::new(),
        }
    }
}

impl Hotkeys {
    /// Configured (non-empty) shortcut strings and the action each triggers.
    pub fn bindings(&self) -> Vec<(Action, &str)> {
        [
            (Action::CaptureArea, &self.capture_area),
            (Action::CapturePreviousArea, &self.capture_previous_area),
            (Action::CaptureFullscreen, &self.capture_fullscreen),
            (Action::CaptureWindow, &self.capture_window),
            (Action::SelfTimer, &self.self_timer),
            (Action::CaptureScrolling, &self.capture_scrolling),
            (Action::CaptureText, &self.capture_text),
            (Action::RecordScreen, &self.record_screen),
            (Action::OpenHistory, &self.open_history),
        ]
        .into_iter()
        .filter(|(_, s)| !s.trim().is_empty())
        .map(|(a, s)| (a, s.as_str()))
        .collect()
    }

    pub fn for_action(&self, action: Action) -> Option<&str> {
        self.bindings()
            .into_iter()
            .find(|(a, _)| *a == action)
            .map(|(_, s)| s)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not read or write {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("invalid settings in {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
}

impl Config {
    /// Loads the config at `path`, writing the defaults there first if the
    /// file does not exist yet.
    pub fn load_or_create(path: &Path) -> Result<Config, ConfigError> {
        match fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|source| ConfigError::Parse {
                path: path.to_owned(),
                source,
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let config = Config::default();
                config.save(path)?;
                Ok(config)
            }
            Err(source) => Err(ConfigError::Io {
                path: path.to_owned(),
                source,
            }),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let io_err = |source| ConfigError::Io {
            path: path.to_owned(),
            source,
        };
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(io_err)?;
        }
        let text = toml::to_string_pretty(self).expect("config is always serializable");
        // Write to a temp file and rename so a crash never leaves a truncated config.
        let tmp = path.with_extension("toml.tmp");
        fs::write(&tmp, text).map_err(io_err)?;
        fs::rename(&tmp, path).map_err(io_err)
    }
}

/// `~/Library/Application Support/CleanShotZ`
pub fn app_support_dir() -> PathBuf {
    home_dir().join("Library/Application Support/CleanShotZ")
}

/// `~/Library/Logs/CleanShotZ`
pub fn log_dir() -> PathBuf {
    home_dir().join("Library/Logs/CleanShotZ")
}

pub fn config_path() -> PathBuf {
    app_support_dir().join("config.toml")
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .expect("HOME is not set")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shortcut::Shortcut;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cleanshotz-test-{}", std::process::id()));
        dir.join(name)
    }

    #[test]
    fn default_hotkeys_parse() {
        for (action, s) in Hotkeys::default().bindings() {
            assert!(Shortcut::parse(s).is_ok(), "{action:?}: {s}");
        }
    }

    #[test]
    fn creates_then_round_trips() {
        let path = temp_path("roundtrip/config.toml");
        let _ = fs::remove_file(&path);
        let created = Config::load_or_create(&path).unwrap();
        assert_eq!(created, Config::default());
        assert!(path.exists());

        let mut changed = created;
        changed.general.onboarding_completed = true;
        changed.hotkeys.capture_area = String::new();
        changed.save(&path).unwrap();
        let loaded = Config::load_or_create(&path).unwrap();
        assert_eq!(loaded, changed);
        assert_eq!(loaded.hotkeys.for_action(Action::CaptureArea), None);
    }

    #[test]
    fn missing_fields_use_defaults() {
        let config: Config = toml::from_str("[hotkeys]\ncapture_area = \"cmd+1\"\n").unwrap();
        assert_eq!(config.hotkeys.capture_area, "cmd+1");
        assert_eq!(config.hotkeys.capture_fullscreen, "alt+shift+3");
        assert!(!config.general.onboarding_completed);
        assert_eq!(config.capture, CaptureSettings::default());
    }

    #[test]
    fn parses_capture_settings() {
        let config: Config =
            toml::from_str("[capture]\nformat = \"jpeg\"\njpeg_quality = 3.0\n").unwrap();
        assert_eq!(config.capture.format, ImageFormat::Jpeg);
        assert_eq!(config.capture.jpeg_quality(), 1.0);
        assert!(config.capture.copy_to_clipboard);
    }

    #[test]
    fn reports_parse_errors() {
        let path = temp_path("bad/config.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "general = 5").unwrap();
        assert!(matches!(
            Config::load_or_create(&path),
            Err(ConfigError::Parse { .. })
        ));
    }
}
