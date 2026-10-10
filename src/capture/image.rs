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

/// Encodes `image` as PNG, JPEG or WebP. `quality` runs from 0.0 to 1.0;
/// PNG ignores it, and for WebP 1.0 means lossless.
pub fn encode(image: &CGImage, format: ImageFormat, quality: f64) -> Option<Vec<u8>> {
    let data = CFMutableData::new(None, 0)?;
    let type_id = CFString::from_static_str(match format {
        ImageFormat::Png => "public.png",
        ImageFormat::Jpeg => "public.jpeg",
        // ImageIO reads WebP but cannot write it.
        ImageFormat::Webp => return encode_webp(image, quality),
    });
    // SAFETY: valid data object and UTType identifier.
    let destination = unsafe { CGImageDestination::with_data(&data, &type_id, 1, None) }?;
    let properties = match format {
        ImageFormat::Png | ImageFormat::Webp => None,
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

/// Encodes with libwebp, after converting to sRGB (WebP files carry no
/// color profile here, and viewers assume sRGB).
fn encode_webp(image: &CGImage, quality: f64) -> Option<Vec<u8>> {
    let (width, height, rgba) = to_srgb_rgba(image)?;
    let (w, h) = (i32::try_from(width).ok()?, i32::try_from(height).ok()?);
    // WebP's limit.
    if w > 16383 || h > 16383 {
        return None;
    }
    let mut out: *mut u8 = ptr::null_mut();
    // SAFETY: `rgba` holds `width * height` tightly packed RGBA pixels;
    // libwebp allocates `out`, which is freed below with WebPFree.
    let size = unsafe {
        if quality >= 1.0 {
            libwebp_sys::WebPEncodeLosslessRGBA(rgba.as_ptr(), w, h, w * 4, &mut out)
        } else {
            let q = (quality.clamp(0.0, 1.0) * 100.0) as f32;
            libwebp_sys::WebPEncodeRGBA(rgba.as_ptr(), w, h, w * 4, q, &mut out)
        }
    };
    if out.is_null() {
        return None;
    }
    // SAFETY: libwebp wrote `size` bytes at `out`.
    let bytes = (size > 0).then(|| unsafe { std::slice::from_raw_parts(out, size) }.to_vec());
    // SAFETY: `out` came from libwebp.
    unsafe { libwebp_sys::WebPFree(out.cast()) };
    bytes
}

/// The image in sRGB as straight (not premultiplied) RGBA bytes.
fn to_srgb_rgba(image: &CGImage) -> Option<(usize, usize, Vec<u8>)> {
    let (width, height) = pixel_size(image);
    let context = bitmap_context(width, height, None)?;
    CGContext::draw_image(
        Some(&context),
        CGRect::new(
            CGPoint::new(0.0, 0.0),
            CGSize::new(width as f64, height as f64),
        ),
        Some(image),
    );
    let data = CGBitmapContextGetData(Some(&context)) as *const u8;
    let stride = CGBitmapContextGetBytesPerRow(Some(&context));
    if data.is_null() {
        return None;
    }
    let mut rgba = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        // SAFETY: each row has at least `width * 4` bytes, alive as long
        // as `context`.
        let row = unsafe { std::slice::from_raw_parts(data.add(y * stride), width * 4) };
        for &[b, g, r, a] in row.as_chunks::<4>().0 {
            rgba.extend_from_slice(&unpremultiply([r, g, b], a));
            rgba.push(a);
        }
    }
    Some((width, height, rgba))
}

fn unpremultiply(rgb: [u8; 3], a: u8) -> [u8; 3] {
    if a == 0 {
        return [0, 0, 0];
    }
    rgb.map(|c| ((u32::from(c) * 255 + u32::from(a) / 2) / u32::from(a)).min(255) as u8)
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

/// The image's pixels as 32-bit BGRA values (premultiplied), row by row,
/// top row first. Returns width, height and pixels.
pub fn to_pixels(image: &CGImage) -> Option<(usize, usize, Vec<u32>)> {
    let (width, height) = pixel_size(image);
    let context = draw_into_bitmap(image, width, height)?;
    let data = CGBitmapContextGetData(Some(&context)) as *const u8;
    let stride = CGBitmapContextGetBytesPerRow(Some(&context));
    if data.is_null() {
        return None;
    }
    let mut pixels = Vec::with_capacity(width * height);
    for y in 0..height {
        // SAFETY: each row has at least `width * 4` bytes, alive as long
        // as `context`; reads are unaligned-safe.
        let row = unsafe { std::slice::from_raw_parts(data.add(y * stride), width * 4) };
        pixels.extend(
            row.as_chunks::<4>()
                .0
                .iter()
                .map(|p| u32::from_ne_bytes([p[0], p[1], p[2], p[3]])),
        );
    }
    Some((width, height, pixels))
}

/// An image from pixels in the layout [`to_pixels`] produces.
pub fn from_pixels(width: usize, height: usize, pixels: &[u32]) -> Option<CFRetained<CGImage>> {
    if width == 0 || height == 0 || pixels.len() != width * height {
        return None;
    }
    let context = bitmap_context(width, height, None)?;
    let data = CGBitmapContextGetData(Some(&context)) as *mut u8;
    let stride = CGBitmapContextGetBytesPerRow(Some(&context));
    if data.is_null() {
        return None;
    }
    for (y, row) in pixels.chunks_exact(width).enumerate() {
        // SAFETY: each destination row has room for `width * 4` bytes.
        let dest = unsafe { std::slice::from_raw_parts_mut(data.add(y * stride), width * 4) };
        for (d, p) in dest.as_chunks_mut::<4>().0.iter_mut().zip(row) {
            d.copy_from_slice(&p.to_ne_bytes());
        }
    }
    CGBitmapContextCreateImage(Some(&context))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A gradient with a transparent corner, in the layout `to_pixels`
    /// uses (premultiplied BGRA).
    fn sample() -> CFRetained<CGImage> {
        let (w, h) = (64, 48);
        let pixels: Vec<u32> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let a: u32 = if x < 8 && y < 8 { 0 } else { 255 };
                let r = (x * 4) as u32 * a / 255;
                let g = (y * 5) as u32 * a / 255;
                let b = 128 * a / 255;
                u32::from_ne_bytes([b as u8, g as u8, r as u8, a as u8])
            })
            .collect();
        from_pixels(w, h, &pixels).unwrap()
    }

    #[test]
    fn lossless_webp_round_trips() {
        let image = sample();
        let webp = encode(&image, ImageFormat::Webp, 1.0).unwrap();
        assert_eq!(&webp[..4], b"RIFF");
        assert_eq!(&webp[8..12], b"WEBP");
        let decoded = decode(&webp).unwrap();
        assert_eq!(pixel_size(&decoded), (64, 48));
        let (_, _, a) = to_srgb_rgba(&image).unwrap();
        let (_, _, b) = to_srgb_rgba(&decoded).unwrap();
        let worst = a.iter().zip(&b).map(|(x, y)| x.abs_diff(*y)).max().unwrap();
        assert!(worst <= 1, "pixels differ by up to {worst}");
    }

    #[test]
    fn lossy_webp_is_smaller() {
        // Noise, like a photo; a smooth gradient is smaller lossless.
        let mut seed = 1u32;
        let pixels: Vec<u32> = (0..128 * 128)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 8) | 0xff00_0000
            })
            .collect();
        let image = from_pixels(128, 128, &pixels).unwrap();
        let lossless = encode(&image, ImageFormat::Webp, 1.0).unwrap();
        let lossy = encode(&image, ImageFormat::Webp, 0.5).unwrap();
        assert!(lossy.len() < lossless.len());
        assert!(decode(&lossy).is_some());
    }

    #[test]
    fn unpremultiplies() {
        assert_eq!(unpremultiply([0, 0, 0], 0), [0, 0, 0]);
        assert_eq!(unpremultiply([64, 128, 255], 255), [64, 128, 255]);
        assert_eq!(unpremultiply([64, 32, 0], 128), [128, 64, 0]);
    }
}
