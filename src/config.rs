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
    pub quick_access: QuickAccessSettings,
    pub text: TextSettings,
    pub recording: RecordingSettings,
    pub history: HistorySettings,
    pub upload: UploadSettings,
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
    /// From a URL or command-line request: what to do with the capture
    /// instead of copying and saving it. Never saved.
    #[serde(skip)]
    pub after: Option<crate::command::After>,
    /// From a `capture-text` request: overrides `keep_line_breaks`.
    #[serde(skip)]
    pub line_breaks: Option<bool>,
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
            after: None,
            line_breaks: None,
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    #[default]
    Left,
    Right,
}

/// The floating thumbnail shown after each capture.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct QuickAccessSettings {
    pub enabled: bool,
    /// Bottom-left or bottom-right corner of the screen.
    pub position: Side,
    /// Close each thumbnail after this many seconds (0: keep until closed).
    pub auto_close_seconds: u32,
    /// Close a thumbnail after it is dragged into another app.
    pub close_after_drag: bool,
}

impl Default for QuickAccessSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            position: Side::Left,
            auto_close_seconds: 0,
            close_after_drag: true,
        }
    }
}

/// Text recognition (Capture Text, Copy Text, redaction).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TextSettings {
    /// Keep line breaks in captured text; off joins lines with spaces.
    pub keep_line_breaks: bool,
    /// Recognition languages as BCP 47 codes, for example `["en-US",
    /// "de-DE"]`. Empty uses the system's preferred languages.
    pub languages: Vec<String>,
    /// Let Vision correct words using a dictionary. Better for photos of
    /// text, worse for code and identifiers on screen.
    pub language_correction: bool,
}

impl TextSettings {
    pub const DEFAULT: TextSettings = TextSettings {
        keep_line_breaks: true,
        languages: Vec::new(),
        language_correction: false,
    };
}

impl Default for TextSettings {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoFormat {
    #[default]
    Mp4,
    Gif,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Codec {
    /// H.264 when the size allows it (most compatible), otherwise HEVC.
    #[default]
    Auto,
    H264,
    Hevc,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    Low,
    Medium,
    #[default]
    High,
}

impl Quality {
    /// Target bits per pixel per frame; screen content compresses well.
    pub fn bits_per_pixel(self) -> f64 {
        match self {
            Quality::Low => 0.04,
            Quality::Medium => 0.07,
            Quality::High => 0.12,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeystrokeMode {
    #[default]
    Off,
    /// Only presses with Command, Control or Option, and keys that never
    /// type text, so typed text (passwords too) is never shown.
    Shortcuts,
    /// Every key.
    All,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CameraShape {
    #[default]
    Circle,
    Rounded,
}

/// Screen recording.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecordingSettings {
    pub format: VideoFormat,
    pub codec: Codec,
    pub fps: u32,
    pub quality: Quality,
    /// Record at 1x instead of the display's Retina resolution.
    pub downscale_retina: bool,
    pub show_cursor: bool,
    /// Highlight mouse clicks (macOS 15 and later).
    pub show_clicks: bool,
    pub system_audio: bool,
    /// Record the microphone (macOS 15 and later).
    pub microphone: bool,
    /// Seconds of countdown before recording starts; 0 starts at once.
    pub countdown_seconds: u32,
    pub gif_fps: u32,
    /// Maximum GIF width in pixels; larger recordings are scaled down.
    pub gif_max_width: u32,
    /// Show the camera in a draggable bubble that is part of the recording.
    pub camera: bool,
    /// Camera bubble size in points.
    pub camera_size: u32,
    pub camera_shape: CameraShape,
    /// Show pressed keys in the recording.
    pub keystrokes: KeystrokeMode,
}

impl Default for RecordingSettings {
    fn default() -> Self {
        Self {
            format: VideoFormat::Mp4,
            codec: Codec::Auto,
            fps: 30,
            quality: Quality::High,
            downscale_retina: false,
            show_cursor: true,
            show_clicks: true,
            system_audio: false,
            microphone: false,
            countdown_seconds: 3,
            gif_fps: 15,
            gif_max_width: 1280,
            camera: false,
            camera_size: 180,
            camera_shape: CameraShape::Circle,
            keystrokes: KeystrokeMode::Off,
        }
    }
}

impl RecordingSettings {
    pub fn fps(&self) -> u32 {
        self.fps.clamp(1, 120)
    }

    pub fn gif_fps(&self) -> u32 {
        self.gif_fps.clamp(1, 50)
    }

    /// Video bitrate in bits per second for a `width` x `height` recording.
    pub fn bitrate(&self, width: usize, height: usize) -> u64 {
        let bits = width as f64 * height as f64 * self.fps() as f64 * self.quality.bits_per_pixel();
        (bits as u64).clamp(1_000_000, 100_000_000)
    }

    /// The codec to use for a recording of this size. H.264 tops out at
    /// 4096 x 2304.
    pub fn codec_for(&self, width: usize, height: usize) -> Codec {
        match self.codec {
            Codec::Auto if width <= 4096 && height <= 2304 => Codec::H264,
            Codec::Auto => Codec::Hevc,
            other => other,
        }
    }
}

/// Capture history.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HistorySettings {
    pub enabled: bool,
    /// Days to keep history entries (and unsaved captures); 0 keeps them.
    pub retention_days: u32,
    /// Recognize text in screenshots so history can be searched by it.
    pub recognize_text: bool,
}

impl HistorySettings {
    pub const DEFAULT: HistorySettings = HistorySettings {
        enabled: true,
        retention_days: 30,
        recognize_text: true,
    };
}

impl Default for HistorySettings {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Destination {
    /// Uploading is off.
    #[default]
    None,
    Http,
    S3,
    Script,
}

/// Uploading captures and copying a link to them.
///
/// Values may be `keychain:NAME` to read a secret from the Keychain (set
/// it with `cleanshotz secret set NAME`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UploadSettings {
    pub destination: Destination,
    /// Upload every capture and recording automatically.
    pub auto_upload: bool,
    pub http: HttpUpload,
    pub s3: S3Upload,
    pub script: ScriptUpload,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HttpUpload {
    pub url: String,
    /// `POST` sends multipart form data; `PUT` sends the file as the body.
    pub method: String,
    /// Multipart field name for the file.
    pub field: String,
    /// Extra multipart fields.
    pub form: std::collections::BTreeMap<String, String>,
    pub headers: std::collections::BTreeMap<String, String>,
    /// Where the link is in the response: `text` (the whole body),
    /// `json:path.to.link`, or `header:Name`.
    pub link: String,
}

impl Default for HttpUpload {
    fn default() -> Self {
        Self {
            url: String::new(),
            method: "POST".into(),
            field: "file".into(),
            form: Default::default(),
            headers: Default::default(),
            link: "text".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct S3Upload {
    pub bucket: String,
    pub region: String,
    /// Custom endpoint for S3-compatible storage, for example
    /// `https://<account>.r2.cloudflarestorage.com`. Empty means AWS.
    pub endpoint: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    /// Prepended to file names, for example `screenshots/`.
    pub prefix: String,
    /// Base of public links, for example `https://cdn.example.com`;
    /// empty uses the object URL.
    pub public_url: String,
    /// Canned ACL such as `public-read`; empty sends none.
    pub acl: String,
}

impl Default for S3Upload {
    fn default() -> Self {
        Self {
            bucket: String::new(),
            region: "us-east-1".into(),
            endpoint: String::new(),
            access_key_id: String::new(),
            secret_access_key: "keychain:s3-secret".into(),
            prefix: String::new(),
            public_url: String::new(),
            acl: String::new(),
        }
    }
}

/// Runs a command with the file path as its argument; the first line it
/// prints is the link.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScriptUpload {
    pub command: String,
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

/// The settings file: `CLEANSHOTZ_CONFIG` if set, otherwise
/// `config.toml` in the application support directory.
pub fn config_path() -> PathBuf {
    match std::env::var_os("CLEANSHOTZ_CONFIG") {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => app_support_dir().join("config.toml"),
    }
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
        assert_eq!(config.quick_access, QuickAccessSettings::default());
        assert_eq!(config.text, TextSettings::default());
        assert_eq!(config.recording, RecordingSettings::default());
        assert_eq!(config.history, HistorySettings::default());
        assert_eq!(config.upload, UploadSettings::default());
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
    fn recording_picks_codec_and_bitrate() {
        let r = RecordingSettings::default();
        assert_eq!(r.codec_for(3456, 2234), Codec::H264);
        assert_eq!(r.codec_for(5120, 2880), Codec::Hevc);
        let hevc = RecordingSettings {
            codec: Codec::Hevc,
            ..Default::default()
        };
        assert_eq!(hevc.codec_for(100, 100), Codec::Hevc);
        assert_eq!(r.bitrate(10, 10), 1_000_000);
        assert_eq!(r.bitrate(1920, 1080), 7_464_960);
    }

    #[test]
    fn parses_upload_settings() {
        let config: Config = toml::from_str(
            "[upload]\ndestination = \"http\"\n[upload.http]\nurl = \"https://x\"\nlink = \"json:data.link\"\n[upload.http.headers]\nAuthorization = \"keychain:imgur\"\n",
        )
        .unwrap();
        assert_eq!(config.upload.destination, Destination::Http);
        assert_eq!(config.upload.http.method, "POST");
        assert_eq!(
            config.upload.http.headers["Authorization"],
            "keychain:imgur"
        );
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
