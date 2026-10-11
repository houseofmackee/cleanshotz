//! Printing images. The image is scaled to the page's printable width;
//! taller images (scrolling captures) run on over as many pages as they
//! need instead of being shrunk onto one.

use objc2::{AllocAnyThread, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSImage, NSImageScaling, NSImageView, NSPrintInfo, NSPrintOperation, NSPrintingPaginationMode,
};
use objc2_core_graphics::CGImage;
use objc2_foundation::{NSCopying, NSPoint, NSRect, NSSize, NSString};

/// How tall the printed image is, in points, at the page's printable
/// `width`: `width` × the image's aspect ratio. With it, the number of
/// pages for a page `page_height` tall.
pub fn layout(image: (f64, f64), width: f64, page_height: f64) -> (f64, usize) {
    let height = width * image.1 / image.0.max(1.0);
    (
        height,
        ((height / page_height.max(1.0)) - 1e-9).ceil().max(1.0) as usize,
    )
}

/// Shows the print panel for `image`, titled `title`.
pub fn print_image(mtm: MainThreadMarker, image: &CGImage, title: &str) {
    let (w, h) = crate::capture::image::pixel_size(image);
    let info = NSPrintInfo::sharedPrintInfo().copy();
    info.setHorizontalPagination(NSPrintingPaginationMode::Fit);
    info.setVerticalPagination(NSPrintingPaginationMode::Automatic);
    info.setVerticallyCentered(false);
    let page = info.imageablePageBounds();
    let (height, _) = layout((w as f64, h as f64), page.size.width, page.size.height);
    let frame = NSRect::new(NSPoint::ZERO, NSSize::new(page.size.width, height));
    let ns = NSImage::initWithCGImage_size(NSImage::alloc(), image, frame.size);
    let view = NSImageView::initWithFrame(NSImageView::alloc(mtm), frame);
    view.setImage(Some(&ns));
    view.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
    let operation = NSPrintOperation::printOperationWithView_printInfo(&view, &info);
    operation.setJobTitle(Some(&NSString::from_str(title)));
    operation.runOperation();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_tall_images_across_pages() {
        // A tall scrolling capture on a 500 × 700 printable page.
        assert_eq!(layout((1000.0, 4000.0), 500.0, 700.0), (2000.0, 3));
        // A screenshot fits on one page.
        assert_eq!(layout((1600.0, 1000.0), 500.0, 700.0), (312.5, 1));
        // Exactly one page tall is one page.
        assert_eq!(layout((500.0, 700.0), 500.0, 700.0), (700.0, 1));
    }
}
