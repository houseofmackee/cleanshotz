//! Cuts in a Studio recording: what is left of the recording, piece by
//! piece, and the mapping between the recording's time (which the events,
//! zooms and captions keep) and the time of the edited result (which the
//! preview plays and the export writes).

use crate::fillers::Span;

/// Times are kept on this grid, AVFoundation's usual time scale, so pieces
/// line up without gaps in a composition.
const SCALE: f64 = 600.0;
/// Kept pieces shorter than this are cut too (not worth two joins).
const MIN_PIECE: f64 = 0.1;

fn snap(t: f64) -> f64 {
    (t * SCALE).round() / SCALE
}

/// A kept part of the recording: `start..end` there, at `at` in the result.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Piece {
    pub start: f64,
    pub end: f64,
    pub at: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Timeline {
    pieces: Vec<Piece>,
    /// The recording's length.
    duration: f64,
}

impl Timeline {
    /// A recording of `duration` seconds without `cuts` (which may overlap
    /// or be out of order).
    pub fn new(duration: f64, cuts: &[Span]) -> Self {
        let duration = snap(duration.max(0.0));
        let mut cuts: Vec<(f64, f64)> = cuts
            .iter()
            .map(|c| (snap(c.start.max(0.0)), snap(c.end.min(duration))))
            .filter(|(a, b)| b > a)
            .collect();
        cuts.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut pieces = Vec::new();
        let mut from = 0.0;
        let mut at = 0.0;
        for (a, b) in cuts {
            if a - from >= MIN_PIECE {
                pieces.push(Piece {
                    start: from,
                    end: a,
                    at,
                });
                at = snap(at + a - from);
            }
            from = from.max(b);
        }
        if duration - from >= MIN_PIECE {
            pieces.push(Piece {
                start: from,
                end: duration,
                at,
            });
        }
        // Nothing left: nothing is cut.
        if pieces.is_empty() {
            pieces.push(Piece {
                start: 0.0,
                end: duration,
                at: 0.0,
            });
        }
        Self { pieces, duration }
    }

    pub fn pieces(&self) -> &[Piece] {
        &self.pieces
    }

    /// Whether anything is cut.
    pub fn is_cut(&self) -> bool {
        self.pieces.len() != 1 || self.pieces[0].start > 0.0 || self.pieces[0].end < self.duration
    }

    /// The parts cut from the recording, in its time.
    pub fn cuts(&self) -> Vec<Span> {
        let mut cuts = Vec::new();
        let mut from = 0.0;
        for p in &self.pieces {
            if p.start > from {
                cuts.push(Span {
                    start: from,
                    end: p.start,
                });
            }
            from = p.end;
        }
        if self.duration > from {
            cuts.push(Span {
                start: from,
                end: self.duration,
            });
        }
        cuts
    }

    /// The result's length.
    pub fn output_duration(&self) -> f64 {
        self.pieces.last().map_or(0.0, |p| p.at + (p.end - p.start))
    }

    /// Where the pieces meet in the result.
    pub fn joins(&self) -> Vec<f64> {
        self.pieces.iter().skip(1).map(|p| p.at).collect()
    }

    /// The recording's time at `t` in the result.
    pub fn to_recording(&self, t: f64) -> f64 {
        let piece = self
            .pieces
            .iter()
            .rev()
            .find(|p| t >= p.at)
            .unwrap_or(&self.pieces[0]);
        (piece.start + (t - piece.at)).clamp(piece.start, piece.end)
    }

    /// The result's time at `t` in the recording; in a cut, where the
    /// result goes on after it.
    pub fn to_output(&self, t: f64) -> f64 {
        for p in &self.pieces {
            if t < p.end {
                return p.at + (t - p.start).max(0.0);
            }
        }
        self.output_duration()
    }

    /// `start..end` of the recording in the result's time, or `None` when
    /// all of it is cut.
    pub fn span_to_output(&self, start: f64, end: f64) -> Option<(f64, f64)> {
        let (a, b) = (self.to_output(start), self.to_output(end));
        (b - a > 0.01).then_some((a, b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(start: f64, end: f64) -> Span {
        Span { start, end }
    }

    #[test]
    fn without_cuts_is_the_recording() {
        let t = Timeline::new(10.0, &[]);
        assert!(!t.is_cut());
        assert_eq!(t.output_duration(), 10.0);
        assert_eq!(t.to_recording(4.0), 4.0);
        assert_eq!(t.to_output(4.0), 4.0);
        assert!(t.joins().is_empty());
        assert!(t.cuts().is_empty());
    }

    #[test]
    fn maps_times_around_cuts() {
        // Cuts out of order and overlapping.
        let t = Timeline::new(10.0, &[span(6.0, 7.0), span(2.0, 3.0), span(2.5, 3.5)]);
        assert!(t.is_cut());
        assert_eq!(t.output_duration(), 7.5);
        assert_eq!(t.joins(), vec![2.0, 4.5]);
        assert_eq!(t.cuts(), vec![span(2.0, 3.5), span(6.0, 7.0)]);
        assert_eq!(t.to_recording(1.0), 1.0);
        assert_eq!(t.to_recording(2.0), 3.5);
        assert_eq!(t.to_recording(5.0), 7.5);
        assert_eq!(t.to_output(1.0), 1.0);
        assert_eq!(t.to_output(4.0), 2.5);
        // In a cut: where the result goes on.
        assert_eq!(t.to_output(3.0), 2.0);
        assert_eq!(t.to_output(6.5), 4.5);
        assert_eq!(t.span_to_output(2.2, 3.0), None);
        assert_eq!(t.span_to_output(1.0, 4.0), Some((1.0, 2.5)));
    }

    #[test]
    fn cuts_tiny_pieces_and_the_ends() {
        // The 0.05 s between the cuts goes too; so do cuts at both ends.
        let t = Timeline::new(
            5.0,
            &[
                span(0.0, 0.5),
                span(1.0, 1.5),
                span(1.55, 2.0),
                span(4.8, 9.0),
            ],
        );
        assert_eq!(
            t.pieces(),
            &[
                Piece {
                    start: 0.5,
                    end: 1.0,
                    at: 0.0
                },
                Piece {
                    start: 2.0,
                    end: 4.8,
                    at: 0.5
                }
            ]
        );
        assert_eq!(t.to_recording(0.0), 0.5);
        assert_eq!(t.to_output(0.2), 0.0);
        assert!((t.to_output(5.0) - 3.3).abs() < 1e-9);
    }

    #[test]
    fn cuts_nothing_when_everything_would_go() {
        let t = Timeline::new(3.0, &[span(0.0, 2.95)]);
        assert!(!t.is_cut());
        assert_eq!(t.output_duration(), 3.0);
    }
}
