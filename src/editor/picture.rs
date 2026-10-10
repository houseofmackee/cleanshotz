//! Images placed in a document: pasted or dropped images, combined
//! captures and background images. A `Picture` is shared (cheaply cloned)
//! between undo snapshots and saved in projects as base64 PNG.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::CGImage;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::capture::image;
use crate::config::ImageFormat;

/// Where a picture came from; decides how a saved background preset
/// refers to it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Origin {
    /// Pasted or dropped: only the pixels are known.
    #[default]
    Pixels,
    File {
        path: PathBuf,
    },
    /// The desktop picture at the time it was chosen.
    Wallpaper,
}

#[derive(Clone)]
pub struct Picture(Arc<Inner>);

struct Inner {
    cg: CFRetained<CGImage>,
    scale: f64,
    origin: Origin,
}

impl Picture {
    pub fn new(cg: CFRetained<CGImage>, scale: f64, origin: Origin) -> Self {
        Self(Arc::new(Inner {
            cg,
            scale: if scale > 0.0 { scale } else { 1.0 },
            origin,
        }))
    }

    /// Loads an image file; `name@2x.png` files count as Retina.
    pub fn load(path: &Path) -> Option<Self> {
        let cg = image::load(path)?;
        Some(Self::new(
            cg,
            image::scale_for_path(path),
            Origin::File {
                path: path.to_path_buf(),
            },
        ))
    }

    pub fn cg(&self) -> &CGImage {
        &self.0.cg
    }

    /// Pixels per point.
    pub fn scale(&self) -> f64 {
        self.0.scale
    }

    pub fn origin(&self) -> &Origin {
        &self.0.origin
    }

    /// Size in points.
    pub fn size(&self) -> (f64, f64) {
        let (w, h) = image::pixel_size(&self.0.cg);
        (w as f64 / self.0.scale, h as f64 / self.0.scale)
    }

    /// A copy at most `max` pixels on its longer side (for large
    /// backgrounds such as 6K wallpapers).
    pub fn limited(self, max: usize) -> Self {
        let (w, h) = image::pixel_size(&self.0.cg);
        let longest = w.max(h);
        if longest <= max {
            return self;
        }
        let factor = max as f64 / longest as f64;
        let (nw, nh) = (
            ((w as f64 * factor).round() as usize).max(1),
            ((h as f64 * factor).round() as usize).max(1),
        );
        match image::resize(&self.0.cg, nw, nh) {
            Some(cg) => Self::new(cg, self.0.scale * factor, self.0.origin.clone()),
            None => self,
        }
    }
}

/// The same picture, not merely equal pixels; enough for undo and for
/// telling whether a change happened.
impl PartialEq for Picture {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl fmt::Debug for Picture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (w, h) = self.size();
        write!(f, "Picture({w}x{h} pt, {:?})", self.0.origin)
    }
}

#[derive(Serialize, Deserialize)]
struct Stored {
    png: String,
    scale: f64,
    #[serde(default)]
    origin: Origin,
}

impl Serialize for Picture {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let png = image::encode(&self.0.cg, ImageFormat::Png, 1.0)
            .ok_or_else(|| serde::ser::Error::custom("could not encode an image"))?;
        Stored {
            png: STANDARD.encode(png),
            scale: self.0.scale,
            origin: self.0.origin.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Picture {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let stored = Stored::deserialize(deserializer)?;
        let png = STANDARD
            .decode(stored.png)
            .map_err(|e| serde::de::Error::custom(format!("damaged image data: {e}")))?;
        let cg =
            image::decode(&png).ok_or_else(|| serde::de::Error::custom("damaged image data"))?;
        Ok(Picture::new(cg, stored.scale, stored.origin))
    }
}
