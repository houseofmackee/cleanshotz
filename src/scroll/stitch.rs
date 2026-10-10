//! Stitches frames of scrolling content into one tall (or wide) image.
//!
//! [`Scroller`] works out the direction from the first scroll: sideways
//! scrolling is stitched as vertical scrolling of the transposed frames,
//! so columns play the part of rows below, and a fixed sidebar is a
//! sticky "header".
//!
//! Each frame's pixel rows are hashed (leaving out the side margins, where
//! scroll bars change). Rows that stay in place between frames at the top
//! and bottom are a sticky header and footer, kept once. In between, the
//! scroll distance is the shift at which the rows of the new frame match
//! the previous frame best; only the newly revealed rows are appended.
//! Uniform rows (blank space) carry no information and are not counted,
//! since they match at any shift.

/// One captured frame: `width * height` pixels, row by row, top first.
#[derive(Clone)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u32>,
}

impl Frame {
    fn row(&self, y: usize) -> &[u32] {
        &self.pixels[y * self.width..(y + 1) * self.width]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Push {
    /// The first frame.
    First,
    /// Nothing scrolled.
    Unchanged,
    /// This many new rows were added.
    Added(usize),
    /// Scrolled too far between frames (or up, or sideways) to match.
    NoMatch,
    /// The image reached the size limit; further frames are ignored.
    Full,
}

#[derive(Clone, Copy)]
struct RowHash {
    hash: u64,
    /// Not a single color.
    informative: bool,
}

pub struct Stitcher {
    width: usize,
    height: usize,
    max_rows: usize,
    /// Header and body rows so far (without the footer).
    rows: Vec<u32>,
    /// The latest frame's footer rows.
    footer: Vec<u32>,
    /// Sticky header and footer heights, fixed once the first scroll is matched.
    edges: Option<(usize, usize)>,
    last: Option<(Frame, Vec<RowHash>)>,
    /// Edge columns that change between matched frames (a scroll bar
    /// appearing and moving). They are filled from stable neighbors.
    unstable: Vec<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Vertical,
    Horizontal,
}

impl Frame {
    /// Columns become rows.
    fn transposed(&self) -> Frame {
        let (w, h) = (self.width, self.height);
        let mut pixels = vec![0; w * h];
        for y in 0..h {
            for x in 0..w {
                pixels[x * h + y] = self.pixels[y * w + x];
            }
        }
        Frame {
            width: h,
            height: w,
            pixels,
        }
    }
}

/// A stitcher for either direction. Until the content first scrolls,
/// frames go to a vertical and a horizontal stitcher; the one that
/// matches the scroll decides the direction.
pub struct Scroller {
    vertical: Option<Stitcher>,
    horizontal: Option<Stitcher>,
    axis: Option<Axis>,
}

impl Scroller {
    pub fn new(max_length: usize) -> Self {
        Self {
            vertical: Some(Stitcher::new(max_length)),
            horizontal: Some(Stitcher::new(max_length)),
            axis: None,
        }
    }

    /// The scroll direction, once known.
    pub fn axis(&self) -> Option<Axis> {
        self.axis
    }

    fn stitcher(&self) -> Option<&Stitcher> {
        match self.axis {
            Some(Axis::Horizontal) => self.horizontal.as_ref(),
            _ => self.vertical.as_ref(),
        }
    }

    /// Pixels captured along the scroll direction.
    pub fn length(&self) -> usize {
        self.stitcher().map_or(0, Stitcher::height)
    }

    /// Size of the stitched image in pixels.
    pub fn size(&self) -> (usize, usize) {
        let Some(s) = self.stitcher() else {
            return (0, 0);
        };
        match self.axis {
            Some(Axis::Horizontal) => (s.height(), s.width()),
            _ => (s.width(), s.height()),
        }
    }

    pub fn push(&mut self, frame: Frame) -> Push {
        match self.axis {
            Some(Axis::Vertical) => {
                return self
                    .vertical
                    .as_mut()
                    .map_or(Push::NoMatch, |s| s.push(frame));
            }
            Some(Axis::Horizontal) => {
                let frame = frame.transposed();
                return self
                    .horizontal
                    .as_mut()
                    .map_or(Push::NoMatch, |s| s.push(frame));
            }
            None => {}
        }
        let sideways = frame.transposed();
        let down = self
            .vertical
            .as_mut()
            .map_or(Push::NoMatch, |s| s.push(frame));
        let across = self
            .horizontal
            .as_mut()
            .map_or(Push::NoMatch, |s| s.push(sideways));
        match (down, across) {
            // Both could mean a diagonal scroll; vertical pages are the
            // common case.
            (Push::Added(_) | Push::Full, _) => {
                self.axis = Some(Axis::Vertical);
                self.horizontal = None;
                down
            }
            (_, Push::Added(_) | Push::Full) => {
                self.axis = Some(Axis::Horizontal);
                self.vertical = None;
                across
            }
            (Push::First, _) => Push::First,
            (Push::Unchanged, _) | (_, Push::Unchanged) => Push::Unchanged,
            _ => Push::NoMatch,
        }
    }

    /// The stitched image, row by row.
    pub fn pixels(&self) -> Vec<u32> {
        let Some(s) = self.stitcher() else {
            return Vec::new();
        };
        let pixels = s.pixels();
        if self.axis != Some(Axis::Horizontal) {
            return pixels;
        }
        Frame {
            width: s.width(),
            height: s.height(),
            pixels,
        }
        .transposed()
        .pixels
    }

    /// A small copy of the image so far, at most `max_w` x `max_h`
    /// pixels, for a live preview. Cheap: it samples the stitched rows
    /// rather than scaling the whole image.
    pub fn preview(&self, max_w: usize, max_h: usize) -> Option<(usize, usize, Vec<u32>)> {
        let s = self.stitcher()?;
        if self.axis == Some(Axis::Horizontal) {
            let (w, h, pixels) = s.preview(max_h, max_w)?;
            let t = Frame {
                width: w,
                height: h,
                pixels,
            }
            .transposed();
            return Some((t.width, t.height, t.pixels));
        }
        s.preview(max_w, max_h)
    }
}

/// Minimum rows that must overlap between consecutive frames.
const MIN_OVERLAP: usize = 12;
/// Fraction of informative overlapping rows that must match.
const MIN_MATCH: f64 = 0.9;
/// When no shift matches that well (text drawn at a subpixel offset
/// renders slightly differently each time, which sideways scrolling causes
/// often), the best shifts with at least this many exact matches are
/// checked pixel by pixel instead.
const MIN_FUZZY_MATCH: f64 = 0.25;
/// Pixel-by-pixel check: channels may differ by this much...
const PIXEL_TOLERANCE: u8 = 40;
/// ...and this fraction of pixels may differ by more (anti-aliased edges).
const MAX_DIFFERENT: f64 = 0.02;

impl Stitcher {
    pub fn new(max_rows: usize) -> Self {
        Self {
            width: 0,
            height: 0,
            max_rows,
            rows: Vec::new(),
            footer: Vec::new(),
            edges: None,
            last: None,
            unstable: Vec::new(),
        }
    }

    /// Total height of the stitched image in rows.
    pub fn height(&self) -> usize {
        (self.rows.len() + self.footer.len())
            .checked_div(self.width)
            .unwrap_or(0)
    }

    pub fn width(&self) -> usize {
        self.width
    }

    /// The stitched image, row by row, with changing edge columns (scroll
    /// bars) replaced by their nearest stable neighbor column.
    pub fn pixels(&self) -> Vec<u32> {
        let mut out = Vec::with_capacity(self.rows.len() + self.footer.len());
        out.extend_from_slice(&self.rows);
        out.extend_from_slice(&self.footer);
        if self.unstable.iter().any(|&u| u) {
            let w = self.width;
            // For each unstable column, the nearest stable one inward.
            let source: Vec<Option<usize>> = (0..w)
                .map(|c| {
                    if !self.unstable[c] {
                        return None;
                    }
                    if c >= w / 2 {
                        (0..c).rev().find(|&x| !self.unstable[x])
                    } else {
                        (c + 1..w).find(|&x| !self.unstable[x])
                    }
                })
                .collect();
            for row in out.chunks_exact_mut(w) {
                for (c, from) in source.iter().enumerate() {
                    if let Some(from) = from {
                        row[c] = row[*from];
                    }
                }
            }
        }
        out
    }

    /// A copy of the image so far, scaled down to fit `max_w` x `max_h`,
    /// averaging a few samples per output pixel.
    pub fn preview(&self, max_w: usize, max_h: usize) -> Option<(usize, usize, Vec<u32>)> {
        let (w, h) = (self.width, self.height());
        if w == 0 || h == 0 || max_w == 0 || max_h == 0 {
            return None;
        }
        let scale = (max_w as f64 / w as f64)
            .min(max_h as f64 / h as f64)
            .min(1.0);
        let (pw, ph) = (
            ((w as f64 * scale).round() as usize).max(1),
            ((h as f64 * scale).round() as usize).max(1),
        );
        let body_rows = self.rows.len() / w;
        let pixel = |x: usize, y: usize| {
            let x = x.min(w - 1);
            if y < body_rows {
                self.rows[y * w + x]
            } else {
                let fy = (y - body_rows).min(self.footer.len() / w - 1);
                self.footer[fy * w + x]
            }
        };
        let mut out = Vec::with_capacity(pw * ph);
        for py in 0..ph {
            for px in 0..pw {
                // Average a 2 x 2 grid of samples inside the source area.
                let mut sum = [0u32; 4];
                for (dx, dy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                    let sx = ((px as f64 + dx) / scale) as usize;
                    let sy = (((py as f64 + dy) / scale) as usize).min(h - 1);
                    for (c, byte) in pixel(sx, sy).to_ne_bytes().iter().enumerate() {
                        sum[c] += u32::from(*byte);
                    }
                }
                out.push(u32::from_ne_bytes(sum.map(|v| (v / 4) as u8)));
            }
        }
        Some((pw, ph, out))
    }

    pub fn push(&mut self, frame: Frame) -> Push {
        let hashes = row_hashes(&frame);
        let Some((last, last_hashes)) = self.last.take() else {
            self.width = frame.width;
            self.height = frame.height;
            self.rows = frame.pixels.clone();
            self.last = Some((frame, hashes));
            return Push::First;
        };
        if frame.width != self.width || frame.height != self.height {
            self.last = Some((last, last_hashes));
            return Push::NoMatch;
        }
        if hashes
            .iter()
            .zip(&last_hashes)
            .all(|(a, b)| a.hash == b.hash)
        {
            self.last = Some((frame, hashes));
            return Push::Unchanged;
        }

        let (header, footer) = self
            .edges
            .unwrap_or_else(|| sticky_edges(&last_hashes, &hashes));
        let body = self.height.saturating_sub(header + footer);
        let (prev_body, next_body) = (
            &last_hashes[header..header + body],
            &hashes[header..header + body],
        );
        let shift = find_shift(prev_body, next_body)
            .or_else(|| fuzzy_shift(prev_body, next_body, &last, &frame, header, body));
        let Some(shift) = shift else {
            // Keep comparing with the last frame that did match, so a
            // single bad frame does not break the chain.
            self.last = Some((last, last_hashes));
            return Push::NoMatch;
        };
        if shift == 0 {
            self.last = Some((frame, hashes));
            return Push::Unchanged;
        }

        self.mark_unstable_edges(&last, &last_hashes, &frame, &hashes, header, body, shift);
        if self.edges.is_none() {
            // The first frame's bottom rows were its footer, not body.
            self.rows.truncate(self.rows.len() - footer * self.width);
            self.edges = Some((header, footer));
        }
        let room = self.max_rows.saturating_sub(self.height());
        if room == 0 {
            self.last = Some((frame, hashes));
            return Push::Full;
        }
        let added = shift.min(room);
        let from = header + body - shift;
        for y in from..from + added {
            self.rows.extend_from_slice(frame.row(y));
        }
        self.footer.clear();
        for y in self.height - footer..self.height {
            self.footer.extend_from_slice(frame.row(y));
        }
        self.last = Some((frame, hashes));
        Push::Added(added)
    }
}

impl Stitcher {
    /// Marks edge columns whose pixels differ between rows with identical
    /// content (same hash), one frame apart. Rows that did not match
    /// exactly are skipped: their edges may differ for real.
    #[allow(clippy::too_many_arguments)]
    fn mark_unstable_edges(
        &mut self,
        prev: &Frame,
        prev_hashes: &[RowHash],
        next: &Frame,
        next_hashes: &[RowHash],
        header: usize,
        body: usize,
        shift: usize,
    ) {
        let w = self.width;
        if self.unstable.len() != w {
            self.unstable = vec![false; w];
        }
        let margin = edge_margin(w);
        let columns: Vec<usize> = (0..margin).chain(w - margin..w).collect();
        // Skip rows near the top and bottom of the body, where window
        // corners and edges sit still while content scrolls behind them.
        let skip = (body / 10).clamp(8, 48);
        for i in 0..body - shift {
            let (p, n) = (header + i + shift, header + i);
            if i < skip || i + shift + skip >= body {
                continue;
            }
            if prev_hashes[p].hash != next_hashes[n].hash || !next_hashes[n].informative {
                continue;
            }
            let (a, b) = (prev.row(p), next.row(n));
            for &c in &columns {
                if a[c] != b[c] {
                    self.unstable[c] = true;
                }
            }
        }
    }
}

/// Width of the side bands left out of row hashes.
fn edge_margin(width: usize) -> usize {
    (width / 25).clamp(1, 48).min(width / 3)
}

fn row_hashes(frame: &Frame) -> Vec<RowHash> {
    // Leave out the edges, where scroll bars appear and move.
    let margin = edge_margin(frame.width);
    (0..frame.height)
        .map(|y| {
            let row = &frame.row(y)[margin..frame.width - margin];
            // FNV-1a over the pixels.
            let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
            for &p in row {
                hash ^= p as u64;
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
            let first = row.first().copied().unwrap_or(0);
            RowHash {
                hash,
                informative: row.iter().any(|&p| p != first),
            }
        })
        .collect()
}

/// Rows that are identical at the same position at the top and bottom of
/// two frames (sticky header and footer). Leaves at least half the frame
/// as scrolling body.
fn sticky_edges(a: &[RowHash], b: &[RowHash]) -> (usize, usize) {
    let same = |i: usize| a[i].hash == b[i].hash;
    let n = a.len();
    let mut header = (0..n).take_while(|&i| same(i)).count();
    let mut footer = (0..n).rev().take_while(|&i| same(i)).count();
    let limit = n / 2;
    if header + footer > limit {
        // Ambiguous (for example blank space at both ends): trust neither.
        header = header.min(limit / 2);
        footer = footer.min(limit / 2);
    }
    (header, footer)
}

/// The downward scroll distance between two body regions: the shift `s`
/// for which `next[i]` matches `prev[i + s]` best, counting informative
/// rows only. Prefers larger overlaps (smaller shifts) among equal scores.
fn find_shift(prev: &[RowHash], next: &[RowHash]) -> Option<usize> {
    let n = prev.len();
    if n <= MIN_OVERLAP {
        return None;
    }
    let mut best: Option<(usize, usize)> = None; // (matches, shift)
    for shift in 0..=n - MIN_OVERLAP {
        let overlap = n - shift;
        let mut informative = 0;
        let mut matches = 0;
        for i in 0..overlap {
            let (p, q) = (prev[i + shift], next[i]);
            if p.informative || q.informative {
                informative += 1;
                if p.hash == q.hash {
                    matches += 1;
                }
            }
        }
        if informative < MIN_OVERLAP / 2 {
            continue;
        }
        let ratio = matches as f64 / informative as f64;
        if ratio >= MIN_MATCH && best.is_none_or(|(m, _)| matches > m) {
            best = Some((matches, shift));
        }
    }
    best.map(|(_, shift)| shift)
}

/// Shifts with at least `MIN_FUZZY_MATCH` of informative rows matching
/// exactly, best first.
fn fuzzy_candidates(prev: &[RowHash], next: &[RowHash]) -> Vec<usize> {
    let n = prev.len();
    if n <= MIN_OVERLAP {
        return Vec::new();
    }
    let mut scored: Vec<(f64, usize)> = (0..=n - MIN_OVERLAP)
        .filter_map(|shift| {
            let (mut informative, mut matches) = (0, 0);
            for i in 0..n - shift {
                let (p, q) = (prev[i + shift], next[i]);
                if p.informative || q.informative {
                    informative += 1;
                    if p.hash == q.hash {
                        matches += 1;
                    }
                }
            }
            if informative < MIN_OVERLAP / 2 {
                return None;
            }
            let ratio = matches as f64 / informative as f64;
            (ratio >= MIN_FUZZY_MATCH).then_some((ratio, shift))
        })
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, shift)| shift).collect()
}

/// How far to look around the best candidate: when content scrolls by a
/// fraction of a pixel, plain areas match exactly at several nearby shifts
/// and only the pixel comparison tells them apart.
const FUZZY_REACH: usize = 6;

/// The shift at which `next` matches `prev` best pixel by pixel, among
/// the best exact-hash candidates and their neighbors, if it is close
/// enough (`MAX_DIFFERENT`).
fn fuzzy_shift(
    prev_hashes: &[RowHash],
    next_hashes: &[RowHash],
    prev: &Frame,
    next: &Frame,
    header: usize,
    body: usize,
) -> Option<usize> {
    let mut tried = std::collections::BTreeSet::new();
    for candidate in fuzzy_candidates(prev_hashes, next_hashes)
        .into_iter()
        .take(3)
    {
        tried.extend(candidate.saturating_sub(FUZZY_REACH)..=candidate + FUZZY_REACH);
    }
    tried
        .into_iter()
        .filter_map(|shift| difference(prev, next, header, body, shift).map(|d| (d, shift)))
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .filter(|(d, _)| *d <= MAX_DIFFERENT)
        .map(|(_, shift)| shift)
}

/// The fraction of pixels that differ noticeably between the body of
/// `next` and `prev` moved up by `shift`, or `None` if they overlap too
/// little. Samples every other row and column, leaving out the edge
/// margins.
fn difference(prev: &Frame, next: &Frame, header: usize, body: usize, shift: usize) -> Option<f64> {
    let overlap = body.checked_sub(shift)?;
    if overlap < MIN_OVERLAP {
        return None;
    }
    let margin = edge_margin(next.width);
    let close = |a: u32, b: u32| {
        a.to_ne_bytes()
            .iter()
            .zip(b.to_ne_bytes())
            .all(|(x, y)| x.abs_diff(y) <= PIXEL_TOLERANCE)
    };
    let (mut compared, mut different) = (0usize, 0usize);
    for i in (0..overlap).step_by(2) {
        let (a, b) = (prev.row(header + i + shift), next.row(header + i));
        for x in (margin..next.width - margin).step_by(2) {
            compared += 1;
            if !close(a[x], b[x]) {
                different += 1;
            }
        }
    }
    (compared > 0).then(|| different as f64 / compared as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page of distinct rows: row `y` is a gradient seeded by `y`, with
    /// some blank rows sprinkled in.
    fn page(width: usize, height: usize) -> Vec<u32> {
        let mut pixels = Vec::with_capacity(width * height);
        for y in 0..height {
            for x in 0..width {
                let blank = y % 37 < 3;
                let seed = (y as u32).wrapping_mul(2_654_435_761);
                pixels.push(if blank {
                    0xFFFF_FFFF
                } else {
                    seed ^ (x as u32 * 7919)
                });
            }
        }
        pixels
    }

    /// A view of `page` starting at `offset`, with fixed header and footer
    /// rows and a scroll bar column that changes per frame.
    fn frame(
        page: &[u32],
        width: usize,
        offset: usize,
        view: usize,
        header: usize,
        footer: usize,
        tick: u32,
    ) -> Frame {
        let mut pixels = Vec::with_capacity(width * view);
        for y in 0..view {
            for x in 0..width {
                let p = if y < header {
                    0xAA00_0000 | (y as u32 * 31 + x as u32)
                } else if y >= view - footer {
                    0xBB00_0000 | (y as u32 * 17 + x as u32)
                } else {
                    page[(offset + y - header) * width + x]
                };
                // The right edge flickers like a scroll bar.
                pixels.push(if x == width - 1 { tick } else { p });
            }
        }
        Frame {
            width,
            height: view,
            pixels,
        }
    }

    fn expected(
        page: &[u32],
        width: usize,
        last_offset: usize,
        view: usize,
        header: usize,
        footer: usize,
        last_tick: u32,
    ) -> Vec<u32> {
        let body = view - header - footer;
        let total = header + last_offset + body + footer;
        let first = frame(page, width, 0, view, header, footer, 0);
        let last = frame(page, width, last_offset, view, header, footer, last_tick);
        let mut out = Vec::with_capacity(width * total);
        out.extend_from_slice(&first.pixels[..width * header]);
        // Body rows: from the frame that revealed them. Compare without the
        // flickering column.
        for y in 0..last_offset + body {
            out.extend_from_slice(&page[y * width..(y + 1) * width]);
        }
        out.extend_from_slice(&last.pixels[width * (view - footer)..]);
        out
    }

    fn without_edge_column(pixels: &[u32], width: usize) -> Vec<u32> {
        pixels
            .chunks(width)
            .flat_map(|row| row[..width - 1].to_vec())
            .collect()
    }

    #[test]
    fn stitches_scrolls_with_sticky_header_and_footer() {
        let (width, view, header, footer) = (120, 200, 24, 16);
        let page = page(width, 2000);
        let mut stitcher = Stitcher::new(100_000);
        // Steps up to 129 rows; the scrolling body is 160 rows tall.
        let offsets = [0, 13, 60, 61, 150, 230, 231, 360, 480, 600, 700];
        for (tick, &offset) in offsets.iter().enumerate() {
            stitcher.push(frame(
                &page,
                width,
                offset,
                view,
                header,
                footer,
                tick as u32,
            ));
        }
        let last = *offsets.last().unwrap();
        let want = expected(
            &page,
            width,
            last,
            view,
            header,
            footer,
            offsets.len() as u32 - 1,
        );
        assert_eq!(stitcher.height(), want.len() / width);
        let got = stitcher.pixels();
        assert_eq!(
            without_edge_column(&got, width),
            without_edge_column(&want, width)
        );
        // The flickering scroll bar column is replaced by its neighbor;
        // the stable left edge is untouched.
        for row in got.chunks(width) {
            assert_eq!(row[width - 1], row[width - 2]);
        }
        assert_eq!(got[..width - 1], want[..width - 1]);
    }

    #[test]
    fn reports_unchanged_and_unmatched_frames() {
        let (width, view) = (80, 120);
        let page = page(width, 1000);
        let mut stitcher = Stitcher::new(100_000);
        assert_eq!(
            stitcher.push(frame(&page, width, 0, view, 0, 0, 0)),
            Push::First
        );
        assert_eq!(
            stitcher.push(frame(&page, width, 0, view, 0, 0, 0)),
            Push::Unchanged
        );
        // Scrolled further than one frame: nothing overlaps.
        assert_eq!(
            stitcher.push(frame(&page, width, 500, view, 0, 0, 0)),
            Push::NoMatch
        );
        // The chain continues from the last good frame.
        assert_eq!(
            stitcher.push(frame(&page, width, 40, view, 0, 0, 0)),
            Push::Added(40)
        );
        assert_eq!(stitcher.height(), 160);
    }

    #[test]
    fn stops_at_the_size_limit() {
        let (width, view) = (60, 100);
        let page = page(width, 1000);
        let mut stitcher = Stitcher::new(150);
        stitcher.push(frame(&page, width, 0, view, 0, 0, 0));
        assert_eq!(
            stitcher.push(frame(&page, width, 80, view, 0, 0, 0)),
            Push::Added(50)
        );
        assert_eq!(
            stitcher.push(frame(&page, width, 90, view, 0, 0, 0)),
            Push::Full
        );
        assert_eq!(stitcher.height(), 150);
    }

    /// The same content scrolled sideways: frames are columns `offset..`
    /// of a wide page (the transpose of `page`).
    fn wide_frame(page_w: usize, page_h: usize, view_w: usize, offset: usize) -> Frame {
        let tall = page(page_h, page_w);
        let mut pixels = Vec::with_capacity(view_w * page_h);
        for y in 0..page_h {
            for x in 0..view_w {
                pixels.push(tall[(offset + x) * page_h + y]);
            }
        }
        Frame {
            width: view_w,
            height: page_h,
            pixels,
        }
    }

    #[test]
    fn detects_and_stitches_sideways_scrolling() {
        let (page_w, page_h, view_w) = (900, 120, 300);
        let mut scroller = Scroller::new(30_000);
        assert_eq!(
            scroller.push(wide_frame(page_w, page_h, view_w, 0)),
            Push::First
        );
        assert_eq!(scroller.axis(), None);
        assert_eq!(
            scroller.push(wide_frame(page_w, page_h, view_w, 80)),
            Push::Added(80)
        );
        assert_eq!(scroller.axis(), Some(Axis::Horizontal));
        for offset in [160, 240, 330, 420, 600] {
            assert!(matches!(
                scroller.push(wide_frame(page_w, page_h, view_w, offset)),
                Push::Added(_)
            ));
        }
        assert_eq!(scroller.size(), (900, 120));
        // The whole wide page, in order.
        assert_eq!(
            scroller.pixels(),
            wide_frame(page_w, page_h, page_w, 0).pixels
        );
    }

    #[test]
    fn detects_vertical_scrolling() {
        let (width, page_h, view_h) = (200, 1000, 300);
        let all = page(width, page_h);
        let view = |offset: usize| Frame {
            width,
            height: view_h,
            pixels: all[offset * width..(offset + view_h) * width].to_vec(),
        };
        let mut scroller = Scroller::new(30_000);
        scroller.push(view(0));
        assert_eq!(scroller.push(view(50)), Push::Added(50));
        assert_eq!(scroller.axis(), Some(Axis::Vertical));
        assert_eq!(scroller.size(), (200, 350));
        assert_eq!(scroller.pixels(), all[..350 * width].to_vec());
    }

    #[test]
    fn previews_fit_and_follow_the_direction() {
        let mut scroller = Scroller::new(30_000);
        scroller.push(wide_frame(900, 120, 300, 0));
        scroller.push(wide_frame(900, 120, 300, 100));
        let (w, h, pixels) = scroller.preview(160, 400).unwrap();
        // 400 x 120 so far, fitted into 160 wide.
        assert_eq!((w, h), (160, 48));
        assert_eq!(pixels.len(), w * h);
        // A uniform image previews as the same color.
        let mut flat = Stitcher::new(1000);
        flat.push(Frame {
            width: 64,
            height: 64,
            pixels: vec![0x8040_2010; 64 * 64],
        });
        let (_, _, pixels) = flat.preview(16, 16).unwrap();
        assert!(pixels.iter().all(|&p| p == 0x8040_2010));
    }

    #[test]
    fn tolerates_antialiasing_noise_but_not_wrong_content() {
        let (width, page_h, view_h) = (200, 1000, 300);
        let all = page(width, page_h);
        // Each frame renders some pixels slightly differently (like text
        // drawn at a subpixel offset): one pixel in eight rows nudged.
        let view = |offset: usize, seed: u32| {
            let mut pixels = all[offset * width..(offset + view_h) * width].to_vec();
            for (i, p) in pixels.iter_mut().enumerate() {
                if (i / width) % 8 == (seed as usize % 8) && i % width == 100 {
                    *p ^= 0x0000_0010;
                }
            }
            Frame {
                width,
                height: view_h,
                pixels,
            }
        };
        let mut s = Stitcher::new(30_000);
        s.push(view(0, 1));
        assert_eq!(s.push(view(60, 2)), Push::Added(60));
        assert_eq!(s.push(view(130, 3)), Push::Added(70));
        // Unrelated content still does not match.
        let other = page(width, 2000)[1500 * width..1800 * width].to_vec();
        let other = Frame {
            width,
            height: view_h,
            pixels: other.iter().map(|p| p.rotate_left(7)).collect(),
        };
        assert_eq!(s.push(other), Push::NoMatch);
    }
}
