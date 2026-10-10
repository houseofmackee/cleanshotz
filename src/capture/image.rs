//! CGImage cropping, scaling and encoding. All functions here are
//! thread-safe (CoreGraphics and ImageIO, no AppKit).

use std::ffi::c_void;
use std::ptr::{self, NonNull};

use objc2_core_foundation::{CFData, CFURL};
use objc2_core_foundation::{
    CFDictionary, CFMutableData, CFNumber, CFRetained, CFString, CGPoint, CGRect, CGSize,
};
use objc2_core_graphics::{
    CGBitmapContextCreateImage, CGBitmapContextGetBytesPerRow, CGBitmapContextGetData,
    CGColorSpace, CGContext, CGImage, CGInterpolationQuality, kCGColorSpaceSRGB,
};
use objc2_image_io::{
    CGImageDestination, CGImageSource, kCGImageDestinationLossyCompressionQuality,
};

use super::geometry::Rect;
use crate::config::ImageFormat;

// Not exposed by objc2-core-graphics 0.3.
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C-unwind" {
    fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: Option<&CGColorSpace>,
        bitmap_info: u32,
    ) -> Option<NonNull<CGContext>>;
}

const K_CG_IMAGE_ALPHA_PREMULTIPLIED_FIRST: u32 = 2;
const K_CG_BITMAP_BYTE_ORDER_32_LITTLE: u32 = 2 << 12;

/// Crops `image` to `rect` given in points, where the image covers the
/// display at `scale` pixels per point.
pub fn crop(image: &CGImage, rect: Rect, scale: f64) -> Option<CFRetained<CGImage>> {
    let pixels = CGRect::new(
        CGPoint::new((rect.x * scale).round(), (rect.y * scale).round()),
        CGSize::new((rect.width * scale).round(), (rect.height * scale).round()),
    );
    CGImage::with_image_in_rect(Some(image), pixels)
}

/// Resamples `image` to `width` x `height` pixels.
pub fn resize(image: &CGImage, width: usize, height: usize) -> Option<CFRetained<CGImage>> {
    let context = draw_into_bitmap(image, width, height)?;
    CGBitmapContextCreateImage(Some(&context))
}

/// Crops away fully transparent rows and columns at the edges (the unused
/// margin around a window shadow).
pub fn trim_transparent(image: &CGImage) -> Option<CFRetained<CGImage>> {
    let (width, height) = pixel_size(image);
    let context = draw_into_bitmap(image, width, height)?;
    let data = CGBitmapContextGetData(Some(&context)) as *const u8;
    let stride = CGBitmapContextGetBytesPerRow(Some(&context));
    if data.is_null() {
        return None;
    }
    // SAFETY: the context owns a buffer of `stride * height` bytes, alive
    // as long as `context`.
    let pixels = unsafe { std::slice::from_raw_parts(data, stride * height) };
    // BGRA in memory: alpha is the 4th byte; row 0 is the top row.
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (width, height, 0, 0);
    for y in 0..height {
        let row = &pixels[y * stride..y * stride + width * 4];
        for (x, px) in row.as_chunks::<4>().0.iter().enumerate() {
            if px[3] != 0 {
                min_x = min_x.min(x);
                max_x = max_x.max(x);
                min_y = min_y.min(y);
                max_y = max_y.max(y);
            }
        }
    }
    if min_x > max_x || min_y > max_y {
        return None;
    }
    CGImage::with_image_in_rect(
        Some(image),
        CGRect::new(
            CGPoint::new(min_x as f64, min_y as f64),
            CGSize::new((max_x - min_x + 1) as f64, (max_y - min_y + 1) as f64),
        ),
    )
}

/// A new, transparent 8-bit premultiplied BGRA bitmap context in
/// `space` (sRGB if `None`).
pub fn bitmap_context(
    width: usize,
    height: usize,
    space: Option<&CGColorSpace>,
) -> Option<CFRetained<CGContext>> {
    let srgb;
    let space = match space {
        Some(space) => space,
        None => {
            srgb = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))?;
            &srgb
        }
    };
    // SAFETY: a null data pointer makes CoreGraphics allocate the buffer;
    // 8-bit premultiplied BGRA is a supported bitmap context format.
    unsafe {
        let raw = CGBitmapContextCreate(
            ptr::null_mut(),
            width,
            height,
            8,
            0,
            Some(space),
            K_CG_IMAGE_ALPHA_PREMULTIPLIED_FIRST | K_CG_BITMAP_BYTE_ORDER_32_LITTLE,
        )?;
        Some(CFRetained::from_raw(raw))
    }
}

/// Draws `image` scaled to `width` x `height` into a new 8-bit
/// premultiplied BGRA bitmap context.
fn draw_into_bitmap(image: &CGImage, width: usize, height: usize) -> Option<CFRetained<CGContext>> {
    let space = CGImage::color_space(Some(image));
    let context = bitmap_context(width, height, space.as_deref())?;
    CGContext::set_interpolation_quality(Some(&context), CGInterpolationQuality::High);
    CGContext::draw_image(
        Some(&context),
        CGRect::new(
            CGPoint::new(0.0, 0.0),
            CGSize::new(width as f64, height as f64),
        ),
        Some(image),
    );
    Some(context)
}

pub fn pixel_size(image: &CGImage) -> (usize, usize) {
    (CGImage::width(Some(image)), CGImage::height(Some(image)))
}

/// Encodes `image` as PNG or JPEG (`quality` 0.0 to 1.0, JPEG only).
pub fn encode(image: &CGImage, format: ImageFormat, quality: f64) -> Option<Vec<u8>> {
    let data = CFMutableData::new(None, 0)?;
    let type_id = CFString::from_static_str(match format {
        ImageFormat::Png => "public.png",
        ImageFormat::Jpeg => "public.jpeg",
    });
    // SAFETY: valid data object and UTType identifier.
    let destination = unsafe { CGImageDestination::with_data(&data, &type_id, 1, None) }?;
    let properties = match format {
        ImageFormat::Png => None,
        ImageFormat::Jpeg => {
            // SAFETY: reading an ImageIO constant.
            let key = unsafe { kCGImageDestinationLossyCompressionQuality };
            Some(CFDictionary::from_slices(
                &[key],
                &[&*CFNumber::new_f64(quality)],
            ))
        }
    };
    // SAFETY: the properties dictionary maps ImageIO keys to CF values.
    unsafe {
        destination.add_image(image, properties.as_deref().map(|p| p.as_opaque()));
        if !destination.finalize() {
            return None;
        }
    }
    Some(data.to_vec())
}

/// Decodes the first image in `bytes` (PNG, JPEG, HEIC, TIFF, ...).
pub fn decode(bytes: &[u8]) -> Option<CFRetained<CGImage>> {
    let data = CFData::from_bytes(bytes);
    // SAFETY: valid data object; no options.
    unsafe {
        let source = CGImageSource::with_data(&data, None)?;
        source.image_at_index(0, None)
    }
}

/// Loads the first image in a file.
pub fn load(path: &std::path::Path) -> Option<CFRetained<CGImage>> {
    let url = CFURL::from_file_path(path)?;
    // SAFETY: valid URL; no options.
    unsafe {
        let source = CGImageSource::with_url(&url, None)?;
        source.image_at_index(0, None)
    }
}

/// Pixels per point for an image file: 2 for `name@2x.ext` (the macOS and
/// CleanShot convention for Retina screenshots), otherwise 1.
pub fn scale_for_path(path: &std::path::Path) -> f64 {
    let retina = path
        .file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.ends_with("@2x"));
    if retina { 2.0 } else { 1.0 }
}
