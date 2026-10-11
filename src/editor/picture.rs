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

use super::model::{Orientation, Turn};
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

    /// The picture turned with the image.
    pub fn turned(&self, turn: Turn) -> Self {
        match turn_image(&self.0.cg, turn) {
            Some(cg) => Self::new(cg, self.0.scale, self.0.origin.clone()),
            None => self.clone(),
        }
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

/// `image` rotated a quarter turn or mirrored.
pub fn turn_image(image: &CGImage, turn: Turn) -> Option<CFRetained<CGImage>> {
    let (w, h) = image::pixel_size(image);
    let (fw, fh) = (w as f64, h as f64);
    // Where pixels go, with CoreGraphics' bottom-left origin.
    let (a, b, c, d, tx, ty, size) = match turn {
        Turn::Right => (0.0, -1.0, 1.0, 0.0, 0.0, fw, (h, w)),
        Turn::Left => (0.0, 1.0, -1.0, 0.0, fh, 0.0, (h, w)),
        Turn::FlipHorizontal => (-1.0, 0.0, 0.0, 1.0, fw, 0.0, (w, h)),
        Turn::FlipVertical => (1.0, 0.0, 0.0, -1.0, 0.0, fh, (w, h)),
    };
    let transform = objc2_core_foundation::CGAffineTransform { a, b, c, d, tx, ty };
    image::transformed(image, size.0, size.1, transform)
}

/// `image` in `orientation`, from the original.
pub fn orient_image(image: &CGImage, orientation: Orientation) -> Option<CFRetained<CGImage>> {
    let mut out: Option<CFRetained<CGImage>> = None;
    for turn in orientation.turns() {
        out = Some(turn_image(out.as_deref().unwrap_or(image), turn)?);
    }
    out.or_else(|| Some(objc2_core_foundation::Type::retain(image)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: u32 = 0xFFFF_0000;
    const GREEN: u32 = 0xFF00_FF00;
    const BLUE: u32 = 0xFF00_00FF;
    const WHITE: u32 = 0xFFFF_FFFF;

    /// Pixels, top row first, of `turn` applied to a 2 x 2 image with
    /// red, green on top and blue, white below.
    fn turned(turns: &[Turn]) -> Vec<u32> {
        let image = image::from_pixels(2, 2, &[RED, GREEN, BLUE, WHITE]).unwrap();
        let o = turns.iter().fold(Orientation::default(), |o, t| o.then(*t));
        let out = orient_image(&image, o).unwrap();
        image::to_pixels(&out).unwrap().2
    }

    #[test]
    fn turns_pixels() {
        assert_eq!(turned(&[]), [RED, GREEN, BLUE, WHITE]);
        assert_eq!(turned(&[Turn::Right]), [BLUE, RED, WHITE, GREEN]);
        assert_eq!(turned(&[Turn::Left]), [GREEN, WHITE, RED, BLUE]);
        assert_eq!(turned(&[Turn::FlipHorizontal]), [GREEN, RED, WHITE, BLUE]);
        assert_eq!(turned(&[Turn::FlipVertical]), [BLUE, WHITE, RED, GREEN]);
        assert_eq!(
            turned(&[Turn::Right, Turn::FlipVertical]),
            [WHITE, GREEN, BLUE, RED]
        );
    }

    #[test]
    fn turns_wide_images() {
        let image = image::from_pixels(3, 1, &[RED, GREEN, BLUE]).unwrap();
        let right = turn_image(&image, Turn::Right).unwrap();
        assert_eq!(
            image::to_pixels(&right).unwrap(),
            (1, 3, vec![RED, GREEN, BLUE])
        );
        let left = turn_image(&image, Turn::Left).unwrap();
        assert_eq!(
            image::to_pixels(&left).unwrap(),
            (1, 3, vec![BLUE, GREEN, RED])
        );
    }
}
