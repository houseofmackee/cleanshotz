//! Commands from `cleanshotz://` URLs and the `cleanshotz` command line.
//! Command and parameter names follow CleanShot X's URL scheme
//! (<https://cleanshot.com/docs-api>), so scripts written for it work with
//! a different scheme name.

use std::path::PathBuf;

use crate::capture::geometry::Rect;
use crate::capture::naming::expand_home;
use crate::pipeline::Step;

/// Every command, in the order `--help` lists them.
pub const NAMES: [&str; 21] = [
    "capture-area",
    "capture-previous-area",
    "capture-fullscreen",
    "capture-window",
    "self-timer",
    "scrolling-capture",
    "all-in-one",
    "capture-text",
    "record-screen",
    "pin",
    "open-annotate",
    "open-from-clipboard",
    "add-quick-access-overlay",
    "toggle-desktop-icons",
    "hide-desktop-icons",
    "show-desktop-icons",
    "open-history",
    "restore-recently-closed",
    "open-settings",
    "run-pipeline",
    "quit",
];

/// What to do with a screenshot instead of the usual copy and save
/// choices (the `action` parameter).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum After {
    Copy,
    Save,
    Annotate,
    Upload,
    Pin,
}

impl After {
    /// The same as pipeline steps.
    pub fn steps(self) -> Vec<Step> {
        match self {
            After::Copy => vec![Step::Copy, Step::QuickAccess],
            After::Save => vec![Step::Save(None), Step::QuickAccess],
            After::Annotate => vec![Step::Annotate],
            After::Upload => vec![Step::Upload, Step::CopyLink, Step::QuickAccess],
            After::Pin => vec![Step::Pin],
        }
    }
}

/// A screenshot request's `action` or `pipeline` parameter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Then {
    Action(After),
    /// A pipeline from the settings file, by name.
    Pipeline(String),
}

/// An area given in the request: points with a bottom-left origin on a
/// display (1 is the main display; `None` is the one under the pointer).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Region {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub display: Option<usize>,
}

impl Region {
    /// The region in display-local points with a top-left origin, clipped
    /// to a display of `size`; `None` if almost none of it is on it.
    pub fn on_display(self, size: (f64, f64)) -> Option<Rect> {
        let rect = Rect::new(
            self.x,
            size.1 - self.y - self.height,
            self.width,
            self.height,
        );
        let clipped = rect.intersection(&Rect::new(0.0, 0.0, size.0, size.1));
        clipped.is_usable().then_some(clipped)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Desktop {
    Toggle,
    Hide,
    Show,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    CaptureArea {
        region: Option<Region>,
        after: Option<Then>,
    },
    CapturePreviousArea {
        after: Option<Then>,
    },
    CaptureFullscreen {
        after: Option<Then>,
    },
    CaptureWindow {
        after: Option<Then>,
    },
    SelfTimer {
        after: Option<Then>,
    },
    ScrollingCapture {
        region: Option<Region>,
        autoscroll: bool,
    },
    CaptureText {
        file: Option<PathBuf>,
        region: Option<Region>,
        line_breaks: Option<bool>,
    },
    RecordScreen {
        region: Option<Region>,
    },
    Pin {
        file: Option<PathBuf>,
    },
    OpenAnnotate {
        file: Option<PathBuf>,
    },
    OpenFromClipboard,
    AddQuickAccessOverlay {
        file: PathBuf,
    },
    DesktopIcons(Desktop),
    OpenHistory,
    RestoreRecentlyClosed,
    OpenSettings {
        tab: Option<String>,
    },
    RunPipeline {
        name: String,
        file: PathBuf,
    },
    Quit,
}

/// Settings tabs `open-settings` accepts: CleanShot's names, then
/// CleanShotZ's own `history` and `uploads`.
pub const SETTINGS_TABS: [&str; 12] = [
    "general",
    "wallpaper",
    "shortcuts",
    "quickaccess",
    "recording",
    "screenshots",
    "annotate",
    "cloud",
    "advanced",
    "about",
    "history",
    "uploads",
];

/// Parses `scheme://command?query`. The scheme itself is not checked, so
/// `cleanshot://` URLs work too.
pub fn parse_url(url: &str) -> Result<Request, String> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let (command, query) = rest.split_once('?').unwrap_or((rest, ""));
    let command = command.trim_end_matches('/');
    let params: Vec<(String, String)> = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (percent_decode(k), percent_decode(v))
        })
        .collect();
    parse(command, &params)
}

/// Builds the URL for `command` and `params`.
pub fn to_url(command: &str, params: &[(String, String)]) -> String {
    let mut url = format!("cleanshotz://{command}");
    for (i, (k, v)) in params.iter().enumerate() {
        url.push(if i == 0 { '?' } else { '&' });
        url.push_str(&percent_encode(k));
        url.push('=');
        url.push_str(&percent_encode(v));
    }
    url
}

pub fn parse(command: &str, params: &[(String, String)]) -> Result<Request, String> {
    let p = Params::new(command, params)?;
    let request = match command {
        "capture-area" | "all-in-one" => Request::CaptureArea {
            region: p.region()?,
            after: p.after()?,
        },
        "capture-previous-area" => Request::CapturePreviousArea { after: p.after()? },
        "capture-fullscreen" => Request::CaptureFullscreen { after: p.after()? },
        "capture-window" => Request::CaptureWindow { after: p.after()? },
        "self-timer" => Request::SelfTimer { after: p.after()? },
        "scrolling-capture" => {
            // `start` is accepted for compatibility: scrolling capture
            // starts as soon as the area is chosen.
            p.bool("start")?;
            Request::ScrollingCapture {
                region: p.region()?,
                autoscroll: p.bool("autoscroll")?.unwrap_or(false),
            }
        }
        "capture-text" => Request::CaptureText {
            file: p.file()?,
            region: p.region()?,
            line_breaks: p.bool("linebreaks")?,
        },
        "record-screen" => Request::RecordScreen {
            region: p.region()?,
        },
        "pin" => Request::Pin { file: p.file()? },
        "open-annotate" => Request::OpenAnnotate { file: p.file()? },
        "open-from-clipboard" => Request::OpenFromClipboard,
        "add-quick-access-overlay" => Request::AddQuickAccessOverlay {
            file: p
                .file()?
                .ok_or("add-quick-access-overlay needs a filepath")?,
        },
        "toggle-desktop-icons" => Request::DesktopIcons(Desktop::Toggle),
        "hide-desktop-icons" => Request::DesktopIcons(Desktop::Hide),
        "show-desktop-icons" => Request::DesktopIcons(Desktop::Show),
        "open-history" => Request::OpenHistory,
        "restore-recently-closed" => Request::RestoreRecentlyClosed,
        "open-settings" => {
            let tab = p.get("tab").map(str::to_ascii_lowercase);
            if let Some(tab) = &tab
                && !SETTINGS_TABS.contains(&tab.as_str())
            {
                return Err(format!(
                    "unknown settings tab {tab:?}; use one of {}",
                    SETTINGS_TABS.join(", ")
                ));
            }
            Request::OpenSettings { tab }
        }
        "run-pipeline" => Request::RunPipeline {
            name: p
                .get("name")
                .filter(|n| !n.trim().is_empty())
                .ok_or("run-pipeline needs a name")?
                .trim()
                .to_string(),
            file: p.file()?.ok_or("run-pipeline needs a filepath")?,
        },
        "quit" => Request::Quit,
        "" => return Err("no command given".into()),
        other => return Err(format!("unknown command {other:?}")),
    };
    Ok(request)
}

/// Parameters each command accepts; anything else is an error, which
/// catches typos.
fn allowed(command: &str) -> &'static [&'static str] {
    const REGION: [&str; 5] = ["x", "y", "width", "height", "display"];
    match command {
        "capture-area" => &["x", "y", "width", "height", "display", "action", "pipeline"],
        "all-in-one" | "record-screen" => &REGION,
        "capture-previous-area" | "capture-fullscreen" | "capture-window" | "self-timer" => {
            &["action", "pipeline"]
        }
        "run-pipeline" => &["name", "filepath"],
        "scrolling-capture" => &[
            "x",
            "y",
            "width",
            "height",
            "display",
            "start",
            "autoscroll",
        ],
        "capture-text" => &[
            "filepath",
            "x",
            "y",
            "width",
            "height",
            "display",
            "linebreaks",
        ],
        "pin" | "open-annotate" | "add-quick-access-overlay" => &["filepath"],
        "open-settings" => &["tab"],
        _ => &[],
    }
}

struct Params<'a> {
    params: &'a [(String, String)],
}

impl<'a> Params<'a> {
    fn new(command: &str, params: &'a [(String, String)]) -> Result<Self, String> {
        let allowed = allowed(command);
        if let Some((name, _)) = params.iter().find(|(k, _)| !allowed.contains(&k.as_str())) {
            return Err(if allowed.is_empty() {
                format!("{command} takes no parameters (got {name:?})")
            } else {
                format!(
                    "{command} does not take {name:?}; it takes {}",
                    allowed.join(", ")
                )
            });
        }
        Ok(Self { params })
    }

    fn get(&self, name: &str) -> Option<&'a str> {
        self.params
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    fn number(&self, name: &str) -> Result<Option<f64>, String> {
        self.get(name)
            .map(|v| {
                v.trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|n| n.is_finite())
                    .ok_or_else(|| format!("{name} must be a number, not {v:?}"))
            })
            .transpose()
    }

    fn bool(&self, name: &str) -> Result<Option<bool>, String> {
        self.get(name)
            .map(|v| match v.to_ascii_lowercase().as_str() {
                "true" | "1" | "yes" | "" => Ok(true),
                "false" | "0" | "no" => Ok(false),
                _ => Err(format!("{name} must be true or false, not {v:?}")),
            })
            .transpose()
    }

    fn file(&self) -> Result<Option<PathBuf>, String> {
        match self.get("filepath") {
            None => Ok(None),
            Some("") => Err("filepath is empty".into()),
            Some(path) => {
                let path = expand_home(path);
                if !path.is_absolute() {
                    return Err(format!("filepath must be absolute: {}", path.display()));
                }
                if !path.is_file() {
                    return Err(format!("no such file: {}", path.display()));
                }
                Ok(Some(path))
            }
        }
    }

    fn after(&self) -> Result<Option<Then>, String> {
        let action = self.action()?.map(Then::Action);
        match (action, self.get("pipeline").map(str::trim)) {
            (Some(_), Some(_)) => Err("use action or pipeline, not both".into()),
            (_, Some("")) => Err("pipeline needs a name".into()),
            (action, Some(name)) => Ok(action.or(Some(Then::Pipeline(name.to_string())))),
            (action, None) => Ok(action),
        }
    }

    fn action(&self) -> Result<Option<After>, String> {
        self.get("action")
            .map(|v| match v.to_ascii_lowercase().as_str() {
                "copy" => Ok(After::Copy),
                "save" => Ok(After::Save),
                "annotate" => Ok(After::Annotate),
                "upload" => Ok(After::Upload),
                "pin" => Ok(After::Pin),
                _ => Err(format!(
                    "action must be copy, save, annotate, upload or pin, not {v:?}"
                )),
            })
            .transpose()
    }

    /// A region needs all of x, y, width and height; `display` alone is
    /// allowed only with them.
    fn region(&self) -> Result<Option<Region>, String> {
        let parts = [
            self.number("x")?,
            self.number("y")?,
            self.number("width")?,
            self.number("height")?,
        ];
        let display = self
            .get("display")
            .map(|v| {
                v.trim()
                    .parse::<usize>()
                    .ok()
                    .filter(|d| *d >= 1)
                    .ok_or_else(|| format!("display must be 1 or more, not {v:?}"))
            })
            .transpose()?;
        match parts {
            [None, None, None, None] if display.is_none() => Ok(None),
            [Some(x), Some(y), Some(width), Some(height)] => {
                if width < 1.0 || height < 1.0 {
                    return Err("width and height must be at least 1".into());
                }
                Ok(Some(Region {
                    x,
                    y,
                    width,
                    height,
                    display,
                }))
            }
            _ => Err("an area needs all of x, y, width and height".into()),
        }
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%'
            && let (Some(hi), Some(lo)) = (
                bytes.get(i + 1).copied().and_then(hex),
                bytes.get(i + 2).copied().and_then(hex),
            )
        {
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands_and_parameters() {
        assert_eq!(
            parse_url("cleanshotz://capture-area").unwrap(),
            Request::CaptureArea {
                region: None,
                after: None
            }
        );
        assert_eq!(
            parse_url(
                "cleanshot://capture-area?x=10&y=20.5&width=300&height=200&display=2&action=pin"
            )
            .unwrap(),
            Request::CaptureArea {
                region: Some(Region {
                    x: 10.0,
                    y: 20.5,
                    width: 300.0,
                    height: 200.0,
                    display: Some(2)
                }),
                after: Some(Then::Action(After::Pin))
            }
        );
        assert_eq!(
            parse_url("cleanshotz://scrolling-capture?autoscroll=true&start=true").unwrap(),
            Request::ScrollingCapture {
                region: None,
                autoscroll: true
            }
        );
        assert_eq!(
            parse_url("cleanshotz://open-settings?tab=Shortcuts").unwrap(),
            Request::OpenSettings {
                tab: Some("shortcuts".into())
            }
        );
        assert_eq!(
            parse_url("cleanshotz://capture-window?pipeline=share").unwrap(),
            Request::CaptureWindow {
                after: Some(Then::Pipeline("share".into()))
            }
        );
        assert_eq!(
            parse_url("cleanshotz://hide-desktop-icons/").unwrap(),
            Request::DesktopIcons(Desktop::Hide)
        );
    }

    #[test]
    fn converts_bottom_left_regions() {
        let region = Region {
            x: 100.0,
            y: 50.0,
            width: 200.0,
            height: 100.0,
            display: None,
        };
        assert_eq!(
            region.on_display((1440.0, 900.0)),
            Some(Rect::new(100.0, 750.0, 200.0, 100.0))
        );
        let partly_off = Region {
            x: 1400.0,
            ..region
        };
        assert_eq!(
            partly_off.on_display((1440.0, 900.0)),
            Some(Rect::new(1400.0, 750.0, 40.0, 100.0))
        );
        let off = Region {
            x: 2000.0,
            ..region
        };
        assert_eq!(off.on_display((1440.0, 900.0)), None);
    }

    #[test]
    fn rejects_bad_requests() {
        for url in [
            "cleanshotz://nope",
            "cleanshotz://",
            "cleanshotz://capture-area?x=1&y=2&width=3",
            "cleanshotz://capture-area?display=1",
            "cleanshotz://capture-area?action=print",
            "cleanshotz://capture-area?x=a&y=2&width=3&height=4",
            "cleanshotz://capture-area?x=1&y=2&width=0&height=4",
            "cleanshotz://capture-area?x=1&y=2&width=3&height=4&display=0",
            "cleanshotz://capture-fullscreen?x=1",
            "cleanshotz://open-history?tab=general",
            "cleanshotz://open-settings?tab=nope",
            "cleanshotz://add-quick-access-overlay",
            "cleanshotz://pin?filepath=relative.png",
            "cleanshotz://pin?filepath=/no/such/file.png",
            "cleanshotz://capture-text?linebreaks=maybe",
            "cleanshotz://capture-area?action=copy&pipeline=share",
            "cleanshotz://capture-fullscreen?pipeline=",
            "cleanshotz://run-pipeline?name=share",
            "cleanshotz://record-screen?pipeline=share",
        ] {
            assert!(parse_url(url).is_err(), "{url} should be rejected");
        }
    }

    #[test]
    fn decodes_file_paths() {
        let dir = std::env::temp_dir().join("cleanshotz command test");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a+b ü.png");
        std::fs::write(&file, b"x").unwrap();
        let url = to_url(
            "open-annotate",
            &[("filepath".into(), file.to_string_lossy().into_owned())],
        );
        assert!(!url.contains(' '));
        assert_eq!(
            parse_url(&url).unwrap(),
            Request::OpenAnnotate {
                file: Some(file.clone())
            }
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn every_command_parses_without_parameters() {
        for name in NAMES {
            let result = parse(name, &[]);
            if name == "add-quick-access-overlay" || name == "run-pipeline" {
                assert!(result.is_err());
            } else {
                assert!(result.is_ok(), "{name}: {result:?}");
            }
        }
    }
}
