//! Background presets: the built-in fills, plus presets saved from the
//! editor in `backgrounds.json` next to the settings file. Image presets
//! keep their own copy of the image in `Backgrounds/`, so they survive
//! the original being moved or deleted.

use std::fs;
use std::path::{Path, PathBuf};

use objc2::MainThreadMarker;
use objc2_app_kit::{NSScreen, NSWorkspace};
use serde::{Deserialize, Serialize};

use super::model::{Aspect, Background, Color, Fill};
use super::picture::{Origin, Picture};
use crate::capture::image;
use crate::config::ImageFormat;

/// Background images are kept at most this many pixels on a side.
const MAX_IMAGE: usize = 3840;

/// The built-in fills, in menu order; their names are translated where
/// they are shown.
pub fn builtin() -> Vec<(&'static str, Fill)> {
    let gradient = |from: Color, to: Color| Fill::Gradient { from, to };
    vec![
        (
            n!("Sunset"),
            gradient(Color::rgb(0.98, 0.45, 0.42), Color::rgb(0.55, 0.36, 0.96)),
        ),
        (
            n!("Ocean"),
            gradient(Color::rgb(0.15, 0.55, 0.95), Color::rgb(0.2, 0.85, 0.75)),
        ),
        (
            n!("Forest"),
            gradient(Color::rgb(0.18, 0.55, 0.33), Color::rgb(0.75, 0.85, 0.3)),
        ),
        (
            n!("Peach"),
            gradient(Color::rgb(1.0, 0.78, 0.55), Color::rgb(0.98, 0.45, 0.55)),
        ),
        (
            n!("Midnight"),
            gradient(Color::rgb(0.1, 0.1, 0.25), Color::rgb(0.32, 0.16, 0.46)),
        ),
        (
            n!("White"),
            Fill::Solid {
                color: Color::WHITE,
            },
        ),
        (
            n!("Light Gray"),
            Fill::Solid {
                color: Color::rgb(0.92, 0.92, 0.93),
            },
        ),
        (
            n!("Black"),
            Fill::Solid {
                color: Color::BLACK,
            },
        ),
    ]
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PresetFill {
    Solid {
        color: Color,
    },
    Gradient {
        from: Color,
        to: Color,
    },
    Image {
        path: PathBuf,
    },
    /// Whatever the desktop picture is when the preset is used.
    Wallpaper,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub fill: PresetFill,
    pub padding: f64,
    pub corner_radius: f64,
    pub shadow: bool,
    pub aspect: Aspect,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub balance: bool,
}

impl Preset {
    /// The background this preset describes. Loads image files, and the
    /// wallpaper (main thread only).
    pub fn background(&self, mtm: MainThreadMarker) -> Result<Background, String> {
        let fill = match &self.fill {
            PresetFill::Solid { color } => Fill::Solid { color: *color },
            PresetFill::Gradient { from, to } => Fill::Gradient {
                from: *from,
                to: *to,
            },
            PresetFill::Image { path } => Fill::Image {
                picture: Picture::load(path)
                    .ok_or_else(|| format!("{} is missing or not an image", path.display()))?
                    .limited(MAX_IMAGE),
            },
            PresetFill::Wallpaper => Fill::Image {
                picture: wallpaper(mtm).ok_or("the desktop picture could not be read")?,
            },
        };
        Ok(Background {
            fill,
            padding: self.padding,
            corner_radius: self.corner_radius,
            shadow: self.shadow,
            aspect: self.aspect,
            balance: self.balance,
        })
    }

    /// Whether `background` is this preset (as applied, before any change).
    pub fn matches(&self, background: &Background) -> bool {
        let fill = match (&self.fill, &background.fill) {
            (PresetFill::Solid { color }, Fill::Solid { color: c }) => color == c,
            (PresetFill::Gradient { from, to }, Fill::Gradient { from: f, to: t }) => {
                from == f && to == t
            }
            (PresetFill::Image { path }, Fill::Image { picture }) => {
                matches!(picture.origin(), Origin::File { path: p } if p == path)
            }
            (PresetFill::Wallpaper, Fill::Image { picture }) => {
                *picture.origin() == Origin::Wallpaper
            }
            _ => false,
        };
        fill && self.padding == background.padding
            && self.corner_radius == background.corner_radius
            && self.shadow == background.shadow
            && self.aspect == background.aspect
            && self.balance == background.balance
    }
}

/// The current desktop picture of the main screen, as a picture.
pub fn wallpaper(mtm: MainThreadMarker) -> Option<Picture> {
    let screen = NSScreen::mainScreen(mtm)?;
    let url = NSWorkspace::sharedWorkspace().desktopImageURLForScreen(&screen)?;
    let path = PathBuf::from(url.path()?.to_string());
    let cg = image::load(&path)?;
    Some(Picture::new(cg, 1.0, Origin::Wallpaper).limited(MAX_IMAGE))
}

fn dir() -> PathBuf {
    let config = crate::config::config_path();
    config
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

fn file() -> PathBuf {
    dir().join("backgrounds.json")
}

/// The saved presets, in the order they were added. A damaged file reads
/// as none (and is reported in the log).
pub fn saved() -> Vec<Preset> {
    match fs::read(file()) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            tracing::error!(error = %e, "could not read saved backgrounds");
            Vec::new()
        }),
        Err(_) => Vec::new(),
    }
}

fn write(presets: &[Preset]) -> Result<(), String> {
    let path = file();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let json = serde_json::to_vec_pretty(presets).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json).map_err(|e| format!("{}: {e}", tmp.display()))?;
    fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn find(name: &str) -> Option<Preset> {
    saved().into_iter().find(|p| p.name == name)
}

/// Saves `background` as preset `name`, replacing one with that name.
pub fn add(name: &str, background: &Background) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("the preset needs a name".into());
    }
    let fill = match &background.fill {
        Fill::Solid { color } => PresetFill::Solid { color: *color },
        Fill::Gradient { from, to } => PresetFill::Gradient {
            from: *from,
            to: *to,
        },
        Fill::Image { picture } if *picture.origin() == Origin::Wallpaper => PresetFill::Wallpaper,
        Fill::Image { picture } => PresetFill::Image {
            path: keep_image(name, picture)?,
        },
    };
    let preset = Preset {
        name: name.to_string(),
        fill,
        padding: background.padding,
        corner_radius: background.corner_radius,
        shadow: background.shadow,
        aspect: background.aspect,
        balance: background.balance,
    };
    let mut presets = saved();
    match presets.iter_mut().find(|p| p.name == preset.name) {
        Some(existing) => *existing = preset,
        None => presets.push(preset),
    }
    write(&presets)
}

pub fn remove(name: &str) -> Result<(), String> {
    let mut presets = saved();
    let before = presets.len();
    let removed: Vec<Preset> = presets.extract_if(.., |p| p.name == name).collect();
    if presets.len() == before {
        return Ok(());
    }
    // Delete image copies no other preset uses.
    for preset in removed {
        if let PresetFill::Image { path } = preset.fill
            && path.starts_with(images_dir())
            && !presets
                .iter()
                .any(|p| matches!(&p.fill, PresetFill::Image { path: q } if *q == path))
        {
            let _ = fs::remove_file(path);
        }
    }
    write(&presets)
}

fn images_dir() -> PathBuf {
    dir().join("Backgrounds")
}

/// Writes the preset's own copy of `picture` and returns its path. A
/// picture that already is such a copy is used as it is.
fn keep_image(name: &str, picture: &Picture) -> Result<PathBuf, String> {
    let dir = images_dir();
    if let Origin::File { path } = picture.origin()
        && path.starts_with(&dir)
        && path.is_file()
    {
        return Ok(path.clone());
    }
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let stem: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let path = crate::capture::naming::unique_path(&dir, stem.trim(), "png");
    crate::capture::output::save_to(&path, picture.cg(), ImageFormat::Png, 1.0)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_only_the_same_settings() {
        let preset = Preset {
            name: "Blue".into(),
            fill: PresetFill::Solid {
                color: Color::rgb(0.0, 0.0, 1.0),
            },
            padding: 40.0,
            corner_radius: 8.0,
            shadow: true,
            aspect: Aspect::Auto,
            balance: false,
        };
        let mut background = Background {
            fill: Fill::Solid {
                color: Color::rgb(0.0, 0.0, 1.0),
            },
            padding: 40.0,
            corner_radius: 8.0,
            shadow: true,
            aspect: Aspect::Auto,
            balance: false,
        };
        assert!(preset.matches(&background));
        background.padding = 41.0;
        assert!(!preset.matches(&background));
        background.padding = 40.0;
        background.fill = Fill::Solid {
            color: Color::WHITE,
        };
        assert!(!preset.matches(&background));
    }

    #[test]
    fn presets_round_trip_through_json() {
        let presets = vec![Preset {
            name: "Wall".into(),
            fill: PresetFill::Wallpaper,
            padding: 64.0,
            corner_radius: 12.0,
            shadow: false,
            aspect: Aspect::Wide,
            balance: false,
        }];
        let json = serde_json::to_string(&presets).unwrap();
        assert_eq!(serde_json::from_str::<Vec<Preset>>(&json).unwrap(), presets);
    }
}
