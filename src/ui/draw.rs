//! Small drawing helpers for custom views. Call only from `drawRect:`.
//! Coordinates are in the drawing view's space (our views are flipped).

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{
    NSBezierPath, NSColor, NSCompositingOperation, NSFont, NSFontAttributeName, NSFontWeightMedium,
    NSFontWeightSemibold, NSForegroundColorAttributeName, NSImage, NSImageSymbolConfiguration,
    NSStringDrawing,
};
use objc2_foundation::{
    NSArray, NSAttributedStringKey, NSDictionary, NSPoint, NSRect, NSSize, NSString,
};

use crate::capture::geometry::{Point, Rect};

pub fn ns_rect(r: Rect) -> NSRect {
    NSRect::new(NSPoint::new(r.x, r.y), NSSize::new(r.width, r.height))
}

pub fn inset(r: Rect, by: f64) -> Rect {
    Rect::new(r.x + by, r.y + by, r.width - 2.0 * by, r.height - 2.0 * by)
}

pub fn fill_rect(rect: Rect, color: &NSColor) {
    color.setFill();
    NSBezierPath::fillRect(ns_rect(rect));
}

pub fn fill_rounded(rect: Rect, radius: f64, color: &NSColor) {
    color.setFill();
    NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(ns_rect(rect), radius, radius).fill();
}

pub fn stroke_rect(rect: Rect, width: f64) {
    let path = NSBezierPath::bezierPathWithRect(ns_rect(rect));
    path.setLineWidth(width);
    path.stroke();
}

pub fn stroke_rounded(rect: Rect, radius: f64, width: f64, color: &NSColor) {
    color.setStroke();
    let path =
        NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(ns_rect(rect), radius, radius);
    path.setLineWidth(width);
    path.stroke();
}

#[derive(Clone, Copy)]
pub enum Weight {
    Medium,
    Semibold,
}

/// Monospaced-digit system font attributes in the given color.
fn attributes(
    size: f64,
    weight: Weight,
    color: &NSColor,
) -> Retained<NSDictionary<NSAttributedStringKey, AnyObject>> {
    // SAFETY: reading AppKit constants.
    let weight = unsafe {
        match weight {
            Weight::Medium => NSFontWeightMedium,
            Weight::Semibold => NSFontWeightSemibold,
        }
    };
    let font = NSFont::monospacedDigitSystemFontOfSize_weight(size, weight);
    let font: &AnyObject = font.as_ref();
    let color: &AnyObject = color.as_ref();
    // SAFETY: reading AppKit constants.
    let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
    NSDictionary::from_slices(&keys, &[font, color])
}

fn font_attributes(
    font: &NSFont,
    color: &NSColor,
) -> Retained<NSDictionary<NSAttributedStringKey, AnyObject>> {
    let font: &AnyObject = font.as_ref();
    let color: &AnyObject = color.as_ref();
    // SAFETY: reading AppKit constants.
    let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
    NSDictionary::from_slices(&keys, &[font, color])
}

pub fn text_size_with_font(text: &str, font: &NSFont) -> NSSize {
    let attributes = font_attributes(font, &NSColor::whiteColor());
    // SAFETY: the attributes map text attribute keys to valid values.
    unsafe { NSString::from_str(text).sizeWithAttributes(Some(&attributes)) }
}

/// Draws `text` in `font` with its top-left at `origin`.
pub fn draw_text_with_font(text: &str, origin: Point, font: &NSFont, color: &NSColor) {
    let attributes = font_attributes(font, color);
    // SAFETY: the attributes map text attribute keys to valid values.
    unsafe {
        NSString::from_str(text)
            .drawAtPoint_withAttributes(NSPoint::new(origin.x, origin.y), Some(&attributes));
    }
}

pub fn text_size(text: &str, size: f64, weight: Weight) -> NSSize {
    let attributes = attributes(size, weight, &NSColor::whiteColor());
    // SAFETY: the attributes map text attribute keys to valid values.
    unsafe { NSString::from_str(text).sizeWithAttributes(Some(&attributes)) }
}

/// Draws `text` with its top-left at `origin`.
pub fn draw_text(text: &str, origin: Point, size: f64, weight: Weight, color: &NSColor) {
    let attributes = attributes(size, weight, color);
    // SAFETY: the attributes map text attribute keys to valid values.
    unsafe {
        NSString::from_str(text)
            .drawAtPoint_withAttributes(NSPoint::new(origin.x, origin.y), Some(&attributes));
    }
}

/// Draws `text` centered in `rect`.
pub fn draw_text_centered(text: &str, rect: Rect, size: f64, weight: Weight, color: &NSColor) {
    let s = text_size(text, size, weight);
    let origin = Point::new(
        rect.x + (rect.width - s.width) / 2.0,
        rect.y + (rect.height - s.height) / 2.0,
    );
    draw_text(text, origin, size, weight, color);
}

/// White text on a dark rounded background, top-left at `origin`.
pub fn draw_label(text: &str, origin: Point, size: f64) {
    let s = text_size(text, size, Weight::Medium);
    let (pad_x, pad_y) = (6.0, 3.0);
    let background = Rect::new(
        origin.x,
        origin.y,
        s.width + pad_x * 2.0,
        s.height + pad_y * 2.0,
    );
    fill_rounded(background, 5.0, &NSColor::colorWithWhite_alpha(0.0, 0.75));
    draw_text(
        text,
        Point::new(origin.x + pad_x, origin.y + pad_y),
        size,
        Weight::Medium,
        &NSColor::whiteColor(),
    );
}

/// Draws an SF Symbol centered in `rect`, tinted with `color`.
pub fn draw_symbol(name: &str, rect: Rect, point_size: f64, color: &NSColor) {
    let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(name),
        None,
    ) else {
        return;
    };
    let config = NSImageSymbolConfiguration::configurationWithPointSize_weight(point_size, 0.0)
        .configurationByApplyingConfiguration(
            &NSImageSymbolConfiguration::configurationWithPaletteColors(&NSArray::from_slice(&[
                color,
            ])),
        );
    let Some(image) = image.imageWithSymbolConfiguration(&config) else {
        return;
    };
    let size = image.size();
    let at = Rect::new(
        rect.x + (rect.width - size.width) / 2.0,
        rect.y + (rect.height - size.height) / 2.0,
        size.width,
        size.height,
    );
    // SAFETY: drawing inside drawRect:.
    unsafe {
        image.drawInRect_fromRect_operation_fraction_respectFlipped_hints(
            ns_rect(at),
            NSRect::ZERO,
            NSCompositingOperation::SourceOver,
            1.0,
            true,
            None,
        );
    }
}
