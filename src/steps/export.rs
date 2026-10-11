//! Writing a guide: Markdown with an images folder next to it, one HTML
//! file with the images inside, or a PDF.

use std::path::{Path, PathBuf};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AllocAnyThread, MainThreadMarker};
use objc2_app_kit::{
    NSApplication, NSColor, NSCompositingOperation, NSFont, NSFontAttributeName,
    NSFontWeightSemibold, NSForegroundColorAttributeName, NSGraphicsContext, NSImage,
    NSModalResponseOK, NSPrintInfo, NSSavePanel, NSStringDrawingOptions,
    NSStringNSExtendedStringDrawing,
};
use objc2_core_foundation::{CFURL, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGContext, CGPDFContextBeginPage, CGPDFContextClose, CGPDFContextCreateWithURL,
    CGPDFContextEndPage,
};
use objc2_foundation::{NSAttributedStringKey, NSDictionary, NSString};

use super::Step;
use crate::config::ImageFormat;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Markdown,
    Html,
    Pdf,
}

impl Format {
    fn extension(self) -> &'static str {
        match self {
            Format::Markdown => "md",
            Format::Html => "html",
            Format::Pdf => "pdf",
        }
    }
}

/// Asks where to save and writes the guide; true when it was saved.
pub fn save(mtm: MainThreadMarker, format: Format, title: &str, steps: &[Step]) -> bool {
    let panel = NSSavePanel::savePanel(mtm);
    let stem = super::file_stem(title);
    panel.setNameFieldStringValue(&NSString::from_str(&format!(
        "{stem}.{}",
        format.extension()
    )));
    NSApplication::sharedApplication(mtm).activate();
    if panel.runModal() != NSModalResponseOK {
        return false;
    }
    let Some(path) = panel.URL().and_then(|u| u.path()) else {
        return false;
    };
    let path = PathBuf::from(path.to_string());
    let title = if title.trim().is_empty() {
        t!("Guide")
    } else {
        title.trim().to_string()
    };
    let result = match format {
        Format::Markdown => write_markdown(&path, &title, steps),
        Format::Html => write_html(&path, &title, steps),
        Format::Pdf => write_pdf(&path, &title, steps),
    };
    match result {
        Ok(()) => {
            tracing::info!(path = %path.display(), ?format, "guide exported");
            crate::ui::toast::show(mtm, &t!("Guide exported"));
            true
        }
        Err(e) => {
            tracing::error!(error = %e, "guide not exported");
            crate::app::alert::show(mtm, "Could not export the guide", &e, &["OK"]);
            false
        }
    }
}

fn png(step: &Step) -> Result<Vec<u8>, String> {
    crate::capture::image::encode(&step.image, ImageFormat::Png, 1.0)
        .ok_or_else(|| "a step image could not be encoded".to_string())
}

fn captions(steps: &[Step]) -> Vec<String> {
    steps.iter().map(|s| s.caption.clone()).collect()
}

/// `Guide.md` and its images in `Guide/` next to it.
fn write_markdown(path: &Path, title: &str, steps: &[Step]) -> Result<(), String> {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Guide".into());
    let folder = path.with_file_name(&stem);
    std::fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    for (i, step) in steps.iter().enumerate() {
        let file = folder.join(super::image_name(i));
        std::fs::write(&file, png(step)?).map_err(|e| format!("{}: {e}", file.display()))?;
    }
    let text = super::markdown(title, &captions(steps), &stem);
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

fn write_html(path: &Path, title: &str, steps: &[Step]) -> Result<(), String> {
    let images = steps
        .iter()
        .map(|s| {
            let (w, _) = crate::capture::image::pixel_size(&s.image);
            Ok((png(s)?, w as f64 / s.scale))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let text = super::html(title, &captions(steps), &images);
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Where each block goes: `(page, top)` for blocks of the given heights
/// on pages with `height` points for content, the first page starting
/// at `first_top`. A block taller than a page gets a page of its own.
pub fn paginate(heights: &[f64], first_top: f64, height: f64) -> Vec<(usize, f64)> {
    let mut page = 0;
    let mut top = first_top;
    heights
        .iter()
        .map(|&h| {
            if top > 0.0 && top + h > height {
                page += 1;
                top = 0.0;
            }
            let at = (page, top);
            top += h + GAP;
            at
        })
        .collect()
}

const MARGIN: f64 = 48.0;
const GAP: f64 = 28.0;
const TITLE_SIZE: f64 = 22.0;
const HEADING_SIZE: f64 = 14.0;
const TEXT_SIZE: f64 = 11.0;

fn attributes(
    font: &NSFont,
    gray: f64,
) -> Retained<NSDictionary<NSAttributedStringKey, AnyObject>> {
    let color = NSColor::colorWithWhite_alpha(gray, 1.0);
    let font: &AnyObject = font.as_ref();
    let color: &AnyObject = color.as_ref();
    // SAFETY: reading AppKit constants.
    let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
    NSDictionary::from_slices(&keys, &[font, color])
}

fn text_height(
    text: &str,
    width: f64,
    attributes: &NSDictionary<NSAttributedStringKey, AnyObject>,
) -> f64 {
    if text.trim().is_empty() {
        return 0.0;
    }
    // SAFETY: the attributes map text attribute keys to valid values.
    let r = unsafe {
        NSString::from_str(text).boundingRectWithSize_options_attributes_context(
            CGSize::new(width, f64::MAX),
            NSStringDrawingOptions::UsesLineFragmentOrigin,
            Some(attributes),
            None,
        )
    };
    r.size.height.ceil()
}

fn draw_text(
    text: &str,
    rect: CGRect,
    attributes: &NSDictionary<NSAttributedStringKey, AnyObject>,
) {
    // SAFETY: the attributes map text attribute keys to valid values; a
    // graphics context is current.
    unsafe {
        NSString::from_str(text).drawWithRect_options_attributes_context(
            rect,
            NSStringDrawingOptions::UsesLineFragmentOrigin,
            Some(attributes),
            None,
        );
    }
}

/// One step per block: its heading, caption and screenshot (scaled to the
/// page width, and down to fit a page).
fn write_pdf(path: &Path, title: &str, steps: &[Step]) -> Result<(), String> {
    let paper = NSPrintInfo::sharedPrintInfo().paperSize();
    let (page_w, page_h) = (paper.width.max(200.0), paper.height.max(200.0));
    let (content_w, content_h) = (page_w - 2.0 * MARGIN, page_h - 2.0 * MARGIN);
    // SAFETY: reading an AppKit constant.
    let semibold = unsafe { NSFontWeightSemibold };
    let title_attrs = attributes(&NSFont::systemFontOfSize_weight(TITLE_SIZE, semibold), 0.1);
    let heading_attrs = attributes(
        &NSFont::systemFontOfSize_weight(HEADING_SIZE, semibold),
        0.1,
    );
    let text_attrs = attributes(&NSFont::systemFontOfSize(TEXT_SIZE), 0.2);

    let title_h = text_height(title, content_w, &title_attrs);
    struct Block {
        heading: f64,
        caption: f64,
        image: (f64, f64),
    }
    let heading_h = text_height("Step 1", content_w, &heading_attrs);
    let blocks: Vec<Block> = steps
        .iter()
        .map(|s| {
            let caption = text_height(&s.caption, content_w, &text_attrs);
            let (w, h) = crate::capture::image::pixel_size(&s.image);
            let (w, h) = (w as f64 / s.scale, h as f64 / s.scale);
            let room = (content_h - heading_h - caption - 16.0).max(100.0);
            let fit = (content_w / w).min(room / h).min(1.0);
            Block {
                heading: heading_h,
                caption,
                image: (w * fit, h * fit),
            }
        })
        .collect();
    let heights: Vec<f64> = blocks
        .iter()
        .map(|b| b.heading + 6.0 + b.caption + 10.0 + b.image.1)
        .collect();
    let places = paginate(&heights, title_h + GAP, content_h);

    let url = CFURL::from_file_path(path).ok_or("the file path is not valid")?;
    let media = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(page_w, page_h));
    // SAFETY: a valid URL and media box; no auxiliary info.
    let pdf = unsafe { CGPDFContextCreateWithURL(Some(&url), &media, None) }
        .ok_or("the PDF could not be created")?;
    let previous = NSGraphicsContext::currentContext();
    let graphics = NSGraphicsContext::graphicsContextWithCGContext_flipped(&pdf, true);
    NSGraphicsContext::setCurrentContext(Some(&graphics));
    let mut current_page = None;
    let begin = |page: usize, current: &mut Option<usize>| {
        if *current == Some(page) {
            return;
        }
        if current.is_some() {
            CGPDFContextEndPage(Some(&pdf));
        }
        // SAFETY: a PDF context; no page info.
        unsafe { CGPDFContextBeginPage(Some(&pdf), None) };
        // Top-left origin, as the layout is computed.
        CGContext::translate_ctm(Some(&pdf), 0.0, page_h);
        CGContext::scale_ctm(Some(&pdf), 1.0, -1.0);
        *current = Some(page);
    };
    begin(0, &mut current_page);
    draw_text(
        title,
        CGRect::new(
            CGPoint::new(MARGIN, MARGIN),
            CGSize::new(content_w, title_h),
        ),
        &title_attrs,
    );
    for (i, ((step, block), (page, top))) in steps.iter().zip(&blocks).zip(places).enumerate() {
        begin(page, &mut current_page);
        let mut y = MARGIN + top;
        draw_text(
            &t!("Step {n}", n = i + 1),
            CGRect::new(
                CGPoint::new(MARGIN, y),
                CGSize::new(content_w, block.heading),
            ),
            &heading_attrs,
        );
        y += block.heading + 6.0;
        if block.caption > 0.0 {
            draw_text(
                step.caption.trim(),
                CGRect::new(
                    CGPoint::new(MARGIN, y),
                    CGSize::new(content_w, block.caption),
                ),
                &text_attrs,
            );
            y += block.caption;
        }
        y += 10.0;
        let (w, h) = block.image;
        let image = NSImage::initWithCGImage_size(NSImage::alloc(), &step.image, CGSize::new(w, h));
        // SAFETY: drawing into the current (PDF) context.
        unsafe {
            image.drawInRect_fromRect_operation_fraction_respectFlipped_hints(
                CGRect::new(CGPoint::new(MARGIN, y), CGSize::new(w, h)),
                CGRect::ZERO,
                NSCompositingOperation::SourceOver,
                1.0,
                true,
                None,
            );
        }
    }
    CGPDFContextEndPage(Some(&pdf));
    CGPDFContextClose(Some(&pdf));
    NSGraphicsContext::setCurrentContext(previous.as_deref());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_each_step_on_one_page() {
        // 700 points per page; the title takes the first 60.
        assert_eq!(
            paginate(&[300.0, 300.0, 300.0, 900.0, 100.0], 60.0, 700.0),
            vec![
                (0, 60.0),
                // 60 + 300 + 28 = 388, and 388 + 300 still fits.
                (0, 388.0),
                // 716 does not: next page.
                (1, 0.0),
                // Taller than a page: a page of its own.
                (2, 0.0),
                (3, 0.0),
            ]
        );
    }
}
