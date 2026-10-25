//! Filler words ("um", "uh", "erm"): finding them in the words captions
//! are made from, leaving them out of the caption text, and placing the
//! cuts that remove them from a recording.
//!
//! Only hesitation sounds count, never words that are fillers only
//! sometimes ("like", "you know"). Word times from the speech recognizer
//! run from one word to the next and are approximate: they include the
//! pauses around a filler, and a word's sound can run into the next word's
//! time. So the sound's loudness (RMS every 10 ms) is split into islands
//! of sound between silences, and the filler is the island that lies
//! mostly within its word's time.

use serde::{Deserialize, Serialize};

use crate::captions::Word;

/// A stretch of a recording, in seconds.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Span {
    pub start: f64,
    pub end: f64,
}

/// Loudness values per second.
pub const LEVELS_PER_SECOND: f64 = 100.0;
/// Cut this much before a filler's sound starts...
const LEAD: f64 = 0.03;
/// ...and stop this much before the next word, so its start stays whole.
const TAIL: f64 = 0.04;
/// Shorter cuts are not worth a join.
const MIN_CUT: f64 = 0.08;
/// Quiet stretches this short inside a word ("u-um") don't split it.
const BRIDGE: f64 = 0.06;
/// At most this much of the pause after a filler is cut; a longer pause
/// was meant.
const MAX_PAUSE: f64 = 0.4;
/// A filler's sound reaching this far into the next word's time (or back
/// into the last one's) was said without a pause: it is cut at the word
/// times there, to keep the other word whole.
const RUN_ON: f64 = 0.15;

/// The hesitation sounds of `locale`'s language (`en_US`), with repeated
/// letters collapsed ("ummm" is "um"). Languages without a list have no
/// filler words.
fn hesitations(locale: &str) -> &'static [&'static str] {
    match locale.split(['_', '-']).next().unwrap_or("") {
        "en" => &["um", "uhm", "uh", "er", "erm", "ehm", "eh", "ah", "hm", "m"],
        // "er" is "he" in German.
        "de" => &["äh", "ähm", "öhm", "ehm", "hm", "m"],
        "fr" => &["euh", "heu", "hm", "m"],
        "es" => &["eh", "ehm", "em", "hm", "m"],
        "it" => &["ehm", "eh", "hm", "m"],
        "nl" => &["eh", "ehm", "uh", "uhm", "hm", "m"],
        // "um" is "one" in Portuguese.
        "pt" => &["hum", "ãh", "hm", "m"],
        _ => &[],
    }
}

/// `text` lowercased, without punctuation, with runs of a letter made one
/// ("Ummm," is "um").
fn normalize(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars().filter(|c| c.is_alphabetic()) {
        for lower in c.to_lowercase() {
            if !out.ends_with(lower) {
                out.push(lower);
            }
        }
    }
    out
}

/// Whether `text` (one recognized word) is a filler word in `locale`.
pub fn is_filler(text: &str, locale: &str) -> bool {
    let word = normalize(text);
    !word.is_empty() && hesitations(locale).contains(&word.as_str())
}

/// `words` without the filler words, for caption text: a sentence a
/// filler started begins with a capital again, a sentence end on a filler
/// moves to the word before it, and the comma before a filler within a
/// sentence goes with it ("grew by, um, 18%" is "grew by 18%"; after a
/// sentence's first word the comma stays: "So, um, here" is "So, here").
pub fn without(words: &[Word], locale: &str) -> Vec<Word> {
    let mut kept: Vec<Word> = Vec::new();
    let mut capitalize = false;
    // Where the sentence being kept starts in `kept`.
    let mut sentence = 0;
    for word in words {
        let text = word.text.trim();
        if is_filler(text, locale) {
            if text.starts_with(char::is_uppercase) {
                capitalize = true;
            }
            if kept.len() > sentence + 1
                && let Some(last) = kept.last_mut()
            {
                last.text =
                    replace_end(&last.text, |end| end.strip_suffix(',').map(str::to_string));
            }
            if let Some(end) = text.chars().last().filter(|c| matches!(c, '.' | '?' | '!'))
                && let Some(last) = kept.last_mut()
                && !last.text.trim_end().ends_with(['.', '?', '!', '…'])
            {
                last.text = replace_end(&last.text, |text| {
                    Some(format!("{}{end}", text.trim_end_matches([',', ';', ':'])))
                });
                capitalize = true;
            }
            continue;
        }
        let mut word = word.clone();
        if capitalize {
            let lead = word.text.len() - word.text.trim_start().len();
            let mut chars = word.text[lead..].chars();
            if let Some(first) = chars.next() {
                word.text = format!(
                    "{}{}{}",
                    &word.text[..lead],
                    first.to_uppercase(),
                    chars.as_str()
                );
            }
            capitalize = false;
        }
        let ends = word.text.trim_end().ends_with(['.', '?', '!', '…']);
        kept.push(word);
        if ends {
            sentence = kept.len();
        }
    }
    kept
}

/// `text` with `change` applied to it without its trailing spaces (the
/// recognizer puts a word's space before or after it); unchanged when
/// `change` gives `None`.
fn replace_end(text: &str, change: impl Fn(&str) -> Option<String>) -> String {
    let trimmed = text.trim_end();
    match change(trimmed) {
        Some(changed) => format!("{changed}{}", &text[trimmed.len()..]),
        None => text.to_string(),
    }
}

/// The quiet floor is taken as at least this loud (decibels): about a
/// quiet room through a microphone. Digital silence (a recording that
/// starts silent) would otherwise put the threshold below what audio
/// compression leaves between words.
const MIN_FLOOR: f32 = -60.0;

/// The loudness above which a 10 ms stretch is speech: a third of the way
/// from the quiet floor (the 10th percentile, at least [`MIN_FLOOR`]) to
/// the speech level (the 90th), in decibels.
fn threshold(levels: &[f32]) -> f32 {
    let mut db: Vec<f32> = levels.iter().map(|l| 20.0 * l.max(1e-6).log10()).collect();
    db.sort_by(f32::total_cmp);
    let at = |q: f64| db[((db.len() - 1) as f64 * q) as usize];
    let (floor, speech) = (at(0.1).max(MIN_FLOOR), at(0.9));
    let db = floor + (speech - floor).max(12.0) / 3.0;
    10f32.powf(db / 20.0)
}

fn time(i: usize) -> f64 {
    i as f64 / LEVELS_PER_SECOND
}

/// The stretches of sound in `levels`: runs of loud 10 ms stretches,
/// joined across quiet ones shorter than [`BRIDGE`].
fn islands(levels: &[f32], loud: f32) -> Vec<Span> {
    let bridge = (BRIDGE * LEVELS_PER_SECOND) as usize;
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for (i, level) in levels.iter().enumerate() {
        if *level < loud {
            continue;
        }
        match runs.last_mut() {
            Some((_, end)) if i - *end <= bridge + 1 => *end = i,
            _ => runs.push((i, i)),
        }
    }
    runs.into_iter()
        .map(|(s, e)| Span {
            start: time(s),
            end: time(e + 1),
        })
        .collect()
}

/// The island that is `word`'s sound: the one with the largest share of
/// itself inside the word's time (then the most overlap), if that share is
/// at least half; its index.
fn island_of(islands: &[Span], word: &Word) -> Option<usize> {
    let mut best: Option<(usize, f64, f64)> = None;
    for (i, island) in islands.iter().enumerate() {
        let overlap = island.end.min(word.end) - island.start.max(word.start);
        if overlap <= 0.0 {
            continue;
        }
        let share = overlap / (island.end - island.start);
        if best.is_none_or(|(_, s, o)| share > s + 1e-9 || (share > s - 1e-9 && overlap > o)) {
            best = Some((i, share, overlap));
        }
    }
    best.filter(|(_, share, _)| *share >= 0.5).map(|(i, ..)| i)
}

/// The cuts that remove the filler words in `words` (in `locale`) from a
/// recording whose sound has `levels` ([`LEVELS_PER_SECOND`]): each from
/// just before the filler's sound to just before the next sound, so the
/// pause before the filler stays and (most of) the one after it goes.
pub fn cuts(words: &[Word], levels: &[f32], locale: &str) -> Vec<Span> {
    let islands = if levels.is_empty() {
        Vec::new()
    } else {
        islands(levels, threshold(levels))
    };
    let mut cuts: Vec<Span> = Vec::new();
    // The word the last cut was for.
    let mut last_filler = None;
    for (i, word) in words.iter().enumerate() {
        if !is_filler(&word.text, locale) {
            continue;
        }
        let next_word = words.get(i + 1).map_or(word.end, |w| w.start.max(word.end));
        let (start, end) = if islands.is_empty() {
            (word.start, next_word - TAIL)
        } else {
            let Some(at) = island_of(&islands, word) else {
                // No sound of its own: misheard.
                continue;
            };
            let sound = islands[at];
            let before = at.checked_sub(1).map_or(0.0, |j| islands[j].end);
            let after = islands.get(at + 1).map_or(f64::MAX, |n| n.start);
            let start = if sound.start < word.start - RUN_ON {
                word.start
            } else {
                (sound.start - LEAD).max(before)
            };
            let end = if sound.end > next_word + RUN_ON {
                next_word - TAIL
            } else {
                (after - TAIL).min(sound.end + MAX_PAUSE).max(sound.end)
            };
            (start, end)
        };
        if end - start < MIN_CUT {
            continue;
        }
        // Fillers in a row make one cut.
        match cuts.last_mut() {
            Some(last) if last_filler.is_some_and(|l| l + 1 == i) || start <= last.end + 0.05 => {
                last.end = last.end.max(end)
            }
            _ => cuts.push(Span { start, end }),
        }
        last_filler = Some(i);
    }
    cuts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(spec: &[(&str, f64, f64)]) -> Vec<Word> {
        spec.iter()
            .map(|(text, start, end)| Word {
                text: text.to_string(),
                start: *start,
                end: *end,
            })
            .collect()
    }

    #[test]
    fn knows_hesitations_per_language() {
        assert!(is_filler(" Um,", "en_US"));
        assert!(is_filler("ummm", "en_GB"));
        assert!(is_filler("Uhh.", "en_AU"));
        assert!(is_filler("Hmm", "en_US"));
        assert!(!is_filler("I'm", "en_US"));
        assert!(!is_filler("like", "en_US"));
        assert!(is_filler("ähm", "de_DE"));
        assert!(!is_filler("er", "de_DE"));
        assert!(!is_filler("um", "pt_BR"));
        assert!(is_filler("euh", "fr_FR"));
        assert!(!is_filler("um", "ja_JP"));
    }

    #[test]
    fn leaves_fillers_out_of_captions() {
        let text = |w: Vec<Word>| w.iter().map(|w| w.text.as_str()).collect::<String>();
        let spoken = words(&[
            ("So,", 0.0, 0.5),
            (" um,", 0.5, 1.1),
            (" here", 1.1, 1.5),
            (" is", 1.5, 1.8),
            (" the", 1.8, 2.0),
            (" uh,", 2.0, 2.8),
            (" report.", 2.8, 3.9),
            (" Um,", 4.0, 5.1),
            (" revenue", 5.1, 5.9),
            (" grew,", 5.9, 6.5),
            (" uh.", 6.5, 7.0),
            (" churn", 7.0, 7.5),
            (" fell.", 7.5, 8.0),
        ]);
        assert_eq!(
            text(without(&spoken, "en_US")),
            "So, here is the report. Revenue grew. Churn fell."
        );
        // Commas around a filler within a sentence go with it.
        let spoken = words(&[
            ("Revenue", 0.0, 0.5),
            (" grew", 0.5, 0.8),
            (" by,", 0.8, 1.0),
            (" um,", 1.0, 1.5),
            (" 18%.", 1.5, 2.0),
            (" And", 2.0, 2.2),
            (" churn,", 2.2, 2.6),
            (" hmm,", 2.6, 3.0),
            (" went", 3.0, 3.2),
            (" down.", 3.2, 3.5),
        ]);
        assert_eq!(
            text(without(&spoken, "en_US")),
            "Revenue grew by 18%. And churn went down."
        );
        // The same with each word's space after it, as some languages'
        // models give them.
        let spoken = words(&[
            ("Uh, ", 0.0, 0.4),
            ("revenue ", 0.4, 0.8),
            ("grew ", 0.8, 1.0),
            ("by, ", 1.0, 1.2),
            ("um, ", 1.2, 1.5),
            ("18%, ", 1.5, 2.0),
            ("and ", 2.0, 2.2),
            ("churn, ", 2.2, 2.6),
            ("hmm. ", 2.6, 3.0),
            ("Done.", 3.0, 3.5),
        ]);
        assert_eq!(
            text(without(&spoken, "en_AU")),
            "Revenue grew by 18%, and churn. Done."
        );
    }

    /// Loudness at 100 per second: `loud` stretches (seconds) at 0.3,
    /// quiet 0.001 elsewhere.
    fn levels(seconds: f64, loud: &[(f64, f64)]) -> Vec<f32> {
        (0..(seconds * 100.0) as usize)
            .map(|i| {
                let t = i as f64 / 100.0;
                if loud.iter().any(|(a, b)| t >= *a && t < *b) {
                    0.3
                } else {
                    0.001
                }
            })
            .collect()
    }

    #[test]
    fn cuts_the_filler_and_the_pause_after_it() {
        // "So, [pause] um [pause] here": word times run into each other.
        let spoken = words(&[("So,", 0.0, 0.5), (" um,", 0.5, 1.2), (" here", 1.2, 1.6)]);
        let sound = levels(2.0, &[(0.05, 0.4), (0.7, 0.9), (1.25, 1.55)]);
        let cuts = cuts(&spoken, &sound, "en_US");
        assert_eq!(cuts.len(), 1);
        let cut = cuts[0];
        // From just before the um (the pause before it stays)...
        assert!((cut.start - 0.67).abs() < 0.011, "{cut:?}");
        // ...to just before "here" starts.
        assert!((cut.end - 1.21).abs() < 0.011, "{cut:?}");
    }

    #[test]
    fn joins_fillers_in_a_row_and_skips_silent_ones() {
        let spoken = words(&[
            ("Well", 0.0, 0.4),
            (" uh", 0.4, 0.9),
            (" um", 0.9, 1.5),
            (" yes.", 1.5, 2.0),
            (" Hmm", 2.0, 3.0),
            (" no", 3.0, 3.5),
        ]);
        // The hmm made no sound (a misheard breath).
        let sound = levels(
            4.0,
            &[(0.0, 0.35), (0.5, 0.8), (1.0, 1.3), (1.5, 1.9), (3.0, 3.4)],
        );
        let cuts = cuts(&spoken, &sound, "en_US");
        assert_eq!(cuts.len(), 1, "{cuts:?}");
        assert!(cuts[0].start < 0.5 && cuts[0].end > 1.4 && cuts[0].end < 1.5);
    }

    #[test]
    fn finds_the_filler_when_the_word_before_runs_into_its_time() {
        // Measured: "and" sounds until 10.17 though its time ends at 9.84;
        // the hmm is 10.45 to 10.78.
        let spoken = words(&[
            (" and", 9.66, 9.84),
            (" hmm,", 9.84, 10.98),
            (" Churn", 10.98, 11.4),
        ]);
        let sound = levels(12.0, &[(9.66, 10.17), (10.45, 10.78), (11.0, 11.4)]);
        let cuts = cuts(&spoken, &sound, "en_US");
        assert_eq!(cuts.len(), 1);
        assert!((cuts[0].start - 10.42).abs() < 0.011, "{cuts:?}");
        assert!((cuts[0].end - 10.96).abs() < 0.011, "{cuts:?}");
    }

    #[test]
    fn keeps_long_pauses_and_run_on_words() {
        // A two-second pause after the um: only 0.4 s of it goes.
        let spoken = words(&[("Ok", 0.0, 0.3), (" um", 0.3, 2.9), (" next", 2.9, 3.3)]);
        let sound = levels(4.0, &[(0.0, 0.3), (0.5, 0.8), (2.9, 3.3)]);
        let cut = cuts(&spoken, &sound, "en_US")[0];
        assert!((cut.start - 0.47).abs() < 0.011 && (cut.end - 1.2).abs() < 0.011);
        // "um so" without a pause: cut where "so" starts.
        let spoken = words(&[("Ok", 0.0, 0.3), (" um", 0.4, 0.9), (" so", 0.9, 1.1)]);
        let sound = levels(2.0, &[(0.0, 0.3), (0.45, 1.1)]);
        let cut = cuts(&spoken, &sound, "en_US")[0];
        assert!(
            (cut.start - 0.42).abs() < 0.011 && (cut.end - 0.86).abs() < 0.011,
            "{cut:?}"
        );
    }

    #[test]
    fn finds_word_gaps_in_compressed_sound_after_digital_silence() {
        // A silent start (digital zero) and gaps left at -85 dB by audio
        // compression, with small bumps: still three separate islands.
        let db = |d: f32| 10f32.powf(d / 20.0);
        let mut sound = vec![1e-7f32; 100];
        for (i, (len, loud)) in [(40, true), (25, false), (30, true), (25, false), (40, true)]
            .iter()
            .enumerate()
        {
            for k in 0..*len {
                sound.push(if *loud {
                    db(-20.0)
                } else {
                    db(if (k + i) % 4 == 0 { -82.0 } else { -88.0 })
                });
            }
        }
        let spoken = words(&[
            (" by,", 1.0, 1.4),
            (" um,", 1.4, 2.0),
            (" eighteen", 2.0, 2.6),
        ]);
        let cuts = cuts(&spoken, &sound, "en_US");
        assert_eq!(cuts.len(), 1, "{cuts:?}");
        assert!(
            (cuts[0].start - 1.62).abs() < 0.011 && (cuts[0].end - 2.16).abs() < 0.011,
            "{cuts:?}"
        );
    }

    #[test]
    fn cuts_by_word_times_without_sound_levels() {
        let spoken = words(&[("Hi", 0.0, 0.3), (" um", 0.3, 0.9), (" there", 0.9, 1.2)]);
        assert_eq!(
            cuts(&spoken, &[], "en_US"),
            vec![Span {
                start: 0.3,
                end: 0.9 - TAIL
            }]
        );
        assert!(cuts(&spoken, &[], "pt_BR").is_empty());
    }
}
