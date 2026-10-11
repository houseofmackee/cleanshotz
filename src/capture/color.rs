//! Colors picked from the screen: read in sRGB (so the value matches what
//! other apps and the web show) and written as hex or CSS `rgb()`.

use objc2_core_graphics::CGImage;

use super::geometry::Rect;

pub type Rgb = [u8; 3];

/// The average sRGB color of `rect` (points) in `image`, which covers a
/// display at `scale` pixels per point. A rect smaller than one pixel reads
/// the pixel at its origin.
pub fn sample(image: &CGImage, rect: Rect, scale: f64) -> Option<Rgb> {
    let px = Rect::new(
        (rect.x * scale).floor(),
        (rect.y * scale).floor(),
        (rect.width * scale).round().max(1.0),
        (rect.height * scale).round().max(1.0),
    );
    let part = super::image::crop(image, px, 1.0)?;
    let (_, _, rgba) = super::image::to_srgb_rgba(&part)?;
    average(&rgba)
}

/// The average of straight RGBA bytes, ignoring transparent pixels.
pub fn average(rgba: &[u8]) -> Option<Rgb> {
    let mut sum = [0u64; 3];
    let mut count = 0u64;
    for p in rgba.as_chunks::<4>().0 {
        if p[3] == 0 {
            continue;
        }
        for (s, c) in sum.iter_mut().zip(p) {
            *s += u64::from(*c);
        }
        count += 1;
    }
    (count > 0).then(|| sum.map(|s| ((s + count / 2) / count) as u8))
}

/// `#3A7BD5`
pub fn hex(c: Rgb) -> String {
    format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

/// `rgb(58, 123, 213)`
pub fn css_rgb(c: Rgb) -> String {
    format!("rgb({}, {}, {})", c[0], c[1], c[2])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_colors() {
        assert_eq!(hex([58, 123, 213]), "#3A7BD5");
        assert_eq!(hex([0, 0, 0]), "#000000");
        assert_eq!(css_rgb([58, 123, 213]), "rgb(58, 123, 213)");
    }

    #[test]
    fn averages_opaque_pixels() {
        let rgba = [255, 0, 0, 255, 0, 0, 255, 255, 9, 9, 9, 0];
        assert_eq!(average(&rgba), Some([128, 0, 128]));
        assert_eq!(average(&[1, 2, 3, 0]), None);
    }

    #[test]
    fn samples_in_srgb() {
        // BGRA in memory: alpha, red, green, blue from the high byte down.
        let pixels = vec![0xFF3A_7BD5u32; 16];
        let image = crate::capture::image::from_pixels(4, 4, &pixels).unwrap();
        let c = sample(&image, Rect::new(1.0, 1.0, 0.0, 0.0), 1.0).unwrap();
        assert_eq!(hex(c), "#3A7BD5");
        let c = sample(&image, Rect::new(0.0, 0.0, 2.0, 2.0), 2.0).unwrap();
        assert_eq!(hex(c), "#3A7BD5");
    }
}
