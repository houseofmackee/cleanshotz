//! Pixel scans for the editor: the opaque body of a window capture (inside
//! its transparent shadow margin), margins of one plain color (for
//! auto-balance) and the span of one color around a point (for automatic
//! measuring). Pixels are 32-bit BGRA values as `image::to_pixels` gives.

/// Inclusive pixel bounds: left, top, right, bottom.
pub type Bounds = (usize, usize, usize, usize);

fn alpha(p: u32) -> u8 {
    (p >> 24) as u8
}

fn channels(p: u32) -> [u8; 4] {
    p.to_be_bytes()
}

/// Whether `a` and `b` differ by at most `tolerance` in every channel.
fn similar(a: u32, b: u32, tolerance: u8) -> bool {
    channels(a)
        .iter()
        .zip(channels(b))
        .all(|(x, y)| x.abs_diff(y) <= tolerance)
}

/// The bounds of the opaque pixels when the image has transparent corners
/// (a window captured with its shadow); `None` for images without them.
pub fn opaque_body(pixels: &[u32], width: usize, height: usize) -> Option<Bounds> {
    if width == 0 || height == 0 || pixels.len() != width * height {
        return None;
    }
    let corner = |x: usize, y: usize| alpha(pixels[y * width + x]);
    let transparent_corners = [
        corner(0, 0),
        corner(width - 1, 0),
        corner(0, height - 1),
        corner(width - 1, height - 1),
    ]
    .iter()
    .any(|a| *a < 128);
    if !transparent_corners {
        return None;
    }
    let mut bounds: Option<Bounds> = None;
    for y in 0..height {
        for x in 0..width {
            if alpha(pixels[y * width + x]) >= 250 {
                bounds = Some(match bounds {
                    None => (x, y, x, y),
                    Some((l, t, r, b)) => (l.min(x), t.min(y), r.max(x), b.max(y)),
                });
            }
        }
    }
    bounds
}

/// The bounds left after trimming rows and columns of one plain color
/// from each edge (the color of the corner they start from, within
/// `tolerance`). Never trims everything.
pub fn plain_margins(pixels: &[u32], width: usize, height: usize, tolerance: u8) -> Bounds {
    let full = (0, 0, width.saturating_sub(1), height.saturating_sub(1));
    if width < 3 || height < 3 || pixels.len() != width * height {
        return full;
    }
    let at = |x: usize, y: usize| pixels[y * width + x];
    let row_is = |y: usize, c: u32| (0..width).all(|x| similar(at(x, y), c, tolerance));
    let column_is = |x: usize, c: u32, top: usize, bottom: usize| {
        (top..=bottom).all(|y| similar(at(x, y), c, tolerance))
    };
    let (top_color, bottom_color) = (at(0, 0), at(width - 1, height - 1));
    let mut top = 0;
    while top < height - 1 && row_is(top, top_color) {
        top += 1;
    }
    let mut bottom = height - 1;
    while bottom > top && row_is(bottom, bottom_color) {
        bottom -= 1;
    }
    if top >= bottom {
        return full;
    }
    let (left_color, right_color) = (at(0, top), at(width - 1, bottom));
    let mut left = 0;
    while left < width - 1 && column_is(left, left_color, top, bottom) {
        left += 1;
    }
    let mut right = width - 1;
    while right > left && column_is(right, right_color, top, bottom) {
        right -= 1;
    }
    if left >= right {
        return full;
    }
    (left, top, right, bottom)
}

/// The run of pixels like the one at (`x`, `y`) along its row (or column
/// when `vertical`): the first and last pixel of that color, within
/// `tolerance`.
pub fn span(
    pixels: &[u32],
    width: usize,
    height: usize,
    (x, y): (usize, usize),
    vertical: bool,
    tolerance: u8,
) -> Option<(usize, usize)> {
    if x >= width || y >= height || pixels.len() != width * height {
        return None;
    }
    let at = |i: usize| {
        if vertical {
            pixels[i * width + x]
        } else {
            pixels[y * width + i]
        }
    };
    let (start, len) = if vertical { (y, height) } else { (x, width) };
    let color = at(start);
    let mut lo = start;
    while lo > 0 && similar(at(lo - 1), color, tolerance) {
        lo -= 1;
    }
    let mut hi = start;
    while hi + 1 < len && similar(at(hi + 1), color, tolerance) {
        hi += 1;
    }
    Some((lo, hi))
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHITE: u32 = 0xFFFF_FFFF;
    const BLUE: u32 = 0xFF20_40C0;
    const CLEAR: u32 = 0x0000_0000;
    const SHADOW: u32 = 0x4000_0000;

    /// A `w` x `h` image of `outside` with `inside` from (l, t) to (r, b).
    fn framed(w: usize, h: usize, outside: u32, inside: u32, (l, t, r, b): Bounds) -> Vec<u32> {
        (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .map(|(x, y)| {
                if (l..=r).contains(&x) && (t..=b).contains(&y) {
                    inside
                } else {
                    outside
                }
            })
            .collect()
    }

    #[test]
    fn finds_the_body_of_window_captures() {
        let mut pixels = framed(10, 8, CLEAR, BLUE, (2, 1, 7, 5));
        // A soft shadow below the window is not part of it.
        pixels[6 * 10 + 4] = SHADOW;
        assert_eq!(opaque_body(&pixels, 10, 8), Some((2, 1, 7, 5)));
        let opaque = framed(10, 8, WHITE, BLUE, (2, 1, 7, 5));
        assert_eq!(opaque_body(&opaque, 10, 8), None);
    }

    #[test]
    fn trims_plain_margins() {
        let pixels = framed(12, 9, WHITE, BLUE, (3, 2, 8, 6));
        assert_eq!(plain_margins(&pixels, 12, 9, 6), (3, 2, 8, 6));
        // Nearly white counts as white.
        let mut noisy = pixels.clone();
        noisy[0] = 0xFFFC_FDFF;
        assert_eq!(plain_margins(&noisy, 12, 9, 6), (3, 2, 8, 6));
        // An image of one color stays as it is.
        let plain = vec![WHITE; 12 * 9];
        assert_eq!(plain_margins(&plain, 12, 9, 6), (0, 0, 11, 8));
    }

    #[test]
    fn measures_spans_of_one_color() {
        let pixels = framed(12, 9, WHITE, BLUE, (3, 2, 8, 6));
        assert_eq!(span(&pixels, 12, 9, (5, 4), false, 6), Some((3, 8)));
        assert_eq!(span(&pixels, 12, 9, (5, 4), true, 6), Some((2, 6)));
        assert_eq!(span(&pixels, 12, 9, (0, 0), false, 6), Some((0, 11)));
        assert_eq!(span(&pixels, 12, 9, (20, 0), false, 6), None);
    }
}
