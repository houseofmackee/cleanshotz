//! `.cszdoc` project files: the original image plus the annotations, so a
//! capture can be re-edited later.
//!
//! The format is a single JSON object, documented here so other tools can
//! read it:
//!
//! ```json
//! {
//!   "format": "cleanshotz-project",
//!   "version": 1,
//!   "scale": 2.0,             // image pixels per point
//!   "image_png": "iVBOR...",  // the original image, base64 PNG
//!   "document": { ... }       // annotations, crop, background
//! }
//! ```

use std::fs;
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::CGImage;
use serde::{Deserialize, Serialize};

use super::model::Document;
use super::render::BaseImage;
use crate::capture::image;
use crate::config::ImageFormat;

pub const EXTENSION: &str = "cszdoc";
const FORMAT: &str = "cleanshotz-project";
const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct ProjectFile {
    format: String,
    version: u32,
    scale: f64,
    image_png: String,
    document: Document,
}

pub fn save(path: &Path, base: &BaseImage, doc: &Document) -> Result<(), String> {
    let png = image::encode(&base.cg, ImageFormat::Png, 1.0).ok_or("could not encode the image")?;
    let file = ProjectFile {
        format: FORMAT.into(),
        version: VERSION,
        scale: base.scale,
        image_png: STANDARD.encode(png),
        document: doc.clone(),
    };
    let json = serde_json::to_vec(&file).map_err(|e| e.to_string())?;
    fs::write(path, json).map_err(|e| format!("{}: {e}", path.display()))
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
