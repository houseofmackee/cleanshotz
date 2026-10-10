//! Stitches frames of vertically scrolling content into one tall image.
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

/// Minimum rows that must overlap between consecutive frames.
const MIN_OVERLAP: usize = 12;
/// Fraction of informative overlapping rows that must match.
const MIN_MATCH: f64 = 0.9;

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
        let Some(shift) = find_shift(
            &last_hashes[header..header + body],
            &hashes[header..header + body],
        ) else {
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
}
