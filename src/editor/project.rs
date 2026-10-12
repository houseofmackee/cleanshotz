//! `.cszdoc` project files: the original image plus the annotations, so a
//! capture can be re-edited later.
//!
//! The format is a single JSON object, documented here so other tools can
//! read it:
//!
//! ```json
//! {
//!   "format": "cleanshotz-project",
//!   "version": 1,             // 2 to 4 when newer features are used
//!   "scale": 2.0,             // image pixels per point
//!   "image_png": "iVBOR...",  // the original image, base64 PNG
//!   "document": { ... }       // annotations, crop, background,
//!                             // orientation, output size
//! }
//! ```

use std::fs;
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::CGImage;
use serde::{Deserialize, Serialize};

use super::model::{Aspect, Document, Shape};
use super::render::BaseImage;
use crate::capture::image;
use crate::config::ImageFormat;

pub const EXTENSION: &str = "cszdoc";
const FORMAT: &str = "cleanshotz-project";
/// The newest version this app reads. Each file gets the lowest version
/// that can hold it, so older versions of the app open what they can show
/// and refuse the rest: 2 adds the orientation and output size, 3 the
/// magnify, measure and text highlight shapes, curved arrows, hand-drawn
/// shapes, image layer opacity, Balance and the newer background sizes,
/// 4 the watermark.
const VERSION: u32 = 4;

#[derive(Serialize, Deserialize)]
struct ProjectFile {
    format: String,
    version: u32,
    scale: f64,
    image_png: String,
    document: Document,
}

pub fn save(path: &Path, base: &BaseImage, doc: &Document) -> Result<(), String> {
    // The original capture: the document turns it.
    let png =
        image::encode(&base.original, ImageFormat::Png, 1.0).ok_or("could not encode the image")?;
    let file = ProjectFile {
        format: FORMAT.into(),
        version: version_for(doc),
        scale: base.scale,
        image_png: STANDARD.encode(png),
        document: doc.clone(),
    };
    let json = serde_json::to_vec(&file).map_err(|e| e.to_string())?;
    fs::write(path, json).map_err(|e| format!("{}: {e}", path.display()))
}

fn version_for(doc: &Document) -> u32 {
    let uses_3 = doc.annotations.iter().any(|a| {
        a.sketchy
            || match &a.shape {
                Shape::Magnify { .. } | Shape::Measure { .. } | Shape::TextHighlight { .. } => true,
                Shape::Arrow { bend, .. } => bend.is_some(),
                Shape::Image { opacity, .. } => *opacity < 1.0,
                _ => false,
            }
    }) || doc.background.as_ref().is_some_and(|b| {
        b.balance
            || !matches!(
                b.aspect,
                Aspect::Auto | Aspect::Square | Aspect::Wide | Aspect::Standard
            )
    });
    let uses_4 = doc
        .annotations
        .iter()
        .any(|a| matches!(a.shape, Shape::Watermark { .. }));
    if uses_4 {
        4
    } else if uses_3 {
        3
    } else if doc.orientation != Default::default() || doc.output_scale.is_some() {
        2
    } else {
        1
    }
}

pub fn load(path: &Path) -> Result<(CFRetained<CGImage>, f64, Document), String> {
    let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let file: ProjectFile = serde_json::from_slice(&bytes)
        .map_err(|e| format!("{} is not a CleanShotZ project: {e}", path.display()))?;
    if file.format != FORMAT || file.version > VERSION {
        return Err(format!(
            "{} was made by a newer or different app (format {:?}, version {})",
            path.display(),
            file.format,
            file.version
        ));
    }
    let png = STANDARD
        .decode(file.image_png)
        .map_err(|e| format!("damaged image data: {e}"))?;
    let image = image::decode(&png).ok_or("damaged image data")?;
    Ok((image, file.scale, file.document))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::geometry::{Point, Rect};
    use crate::editor::model::{Background, Color, Turn};

    #[test]
    fn writes_the_lowest_version_that_holds_the_document() {
        let mut doc = Document::default();
        doc.add(
            Shape::Rectangle {
                rect: Rect::new(0.0, 0.0, 10.0, 10.0),
                filled: false,
            },
            Color::RED,
            4.0,
        );
        assert_eq!(version_for(&doc), 1);
        doc.turn(Turn::Right, 100.0, 50.0, |p| p.clone());
        assert_eq!(version_for(&doc), 2);
        doc.background = Some(Background {
            aspect: Aspect::Story,
            ..Background::default()
        });
        assert_eq!(version_for(&doc), 3);
        let mut doc = Document::default();
        doc.add(
            Shape::Measure {
                start: Point::new(0.0, 0.0),
                end: Point::new(5.0, 0.0),
            },
            Color::RED,
            4.0,
        );
        assert_eq!(version_for(&doc), 3);
    }
}
