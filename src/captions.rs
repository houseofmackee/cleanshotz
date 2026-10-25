//! Captions: speech in a recording turned into timed text, made on the Mac
//! by Apple's SpeechAnalyzer (macOS 26 and later, see `swift/Captions.swift`).
//! The words come back with their times and are grouped here into cues of
//! at most two short lines, which can be edited, shown in exports and
//! saved as SRT or WebVTT subtitles.

use serde::{Deserialize, Serialize};

/// A recognized word and when it was said, in seconds. Its text may start
/// with a space and end with punctuation.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Word {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

/// One caption on screen from `start` to `end` (seconds).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cue {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// Characters per line, as subtitle guidelines suggest.
pub const LINE: usize = 42;
/// A cue is at most two lines...
const MAX_CHARS: usize = LINE * 2;
/// ...and this long.
const MAX_SECONDS: f64 = 6.0;
/// A pause this long starts a new cue.
const PAUSE: f64 = 1.2;
/// Short cues stay up this long when the next one leaves room.
const MIN_SECONDS: f64 = 1.0;

/// Groups `words` into cues: a new cue after a sentence ends, after a
/// pause, or when the cue would grow past two lines or six seconds.
pub fn make_cues(words: &[Word]) -> Vec<Cue> {
    let mut cues: Vec<Cue> = Vec::new();
    let mut current: Option<Cue> = None;
    for word in words {
        let piece = word.text.trim();
        if piece.is_empty() {
            continue;
        }
        if let Some(cue) = &current {
            let longer = cue.text.chars().count() + 1 + piece.chars().count();
            let ended = cue.text.ends_with(['.', '?', '!', '…']);
            if ended
                || longer > MAX_CHARS
                || word.end - cue.start > MAX_SECONDS
                || word.start - cue.end > PAUSE
            {
                cues.extend(current.take());
            }
        }
        match &mut current {
            Some(cue) => {
                cue.text.push(' ');
                cue.text.push_str(piece);
                cue.end = word.end.max(cue.end);
            }
            None => {
                current = Some(Cue {
                    start: word.start,
                    end: word.end.max(word.start),
                    text: piece.to_string(),
                })
            }
        }
    }
    cues.extend(current);
    // Keep short cues up a little longer, without running into the next.
    for i in 0..cues.len() {
        let next = cues.get(i + 1).map(|c| c.start);
        let cue = &mut cues[i];
        if cue.end - cue.start < MIN_SECONDS {
            let wanted = cue.start + MIN_SECONDS;
            cue.end = next.map_or(wanted, |n| wanted.min(n)).max(cue.end);
        }
    }
    cues
}

/// `text` in at most two lines of about [`LINE`] characters, broken at
/// spaces and balanced so the lines are about as long.
pub fn lines(text: &str) -> Vec<String> {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= LINE {
        return vec![text];
    }
    // The space closest to the middle.
    let middle = text.chars().count() / 2;
    let spaces: Vec<usize> = text
        .char_indices()
        .filter(|(_, c)| *c == ' ')
        .map(|(i, _)| i)
        .collect();
    let Some(&split) = spaces
        .iter()
        .min_by_key(|&&i| (text[..i].chars().count() as isize - middle as isize).abs())
    else {
        return vec![text];
    };
    vec![text[..split].to_string(), text[split + 1..].to_string()]
}

/// SubRip subtitles (`.srt`).
pub fn srt(cues: &[Cue]) -> String {
    let time = |t: f64| {
        let ms = (t.max(0.0) * 1000.0).round() as u64;
        format!(
            "{:02}:{:02}:{:02},{:03}",
            ms / 3_600_000,
            ms / 60_000 % 60,
            ms / 1000 % 60,
            ms % 1000
        )
    };
    cues.iter()
        .enumerate()
        .map(|(i, c)| {
            format!(
                "{}\n{} --> {}\n{}\n",
                i + 1,
                time(c.start),
                time(c.end),
                lines(&c.text).join("\n")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// WebVTT subtitles (`.vtt`), for the web.
pub fn vtt(cues: &[Cue]) -> String {
    let time = |t: f64| {
        let ms = (t.max(0.0) * 1000.0).round() as u64;
        format!(
            "{:02}:{:02}:{:02}.{:03}",
            ms / 3_600_000,
            ms / 60_000 % 60,
            ms / 1000 % 60,
            ms % 1000
        )
    };
    let mut out = String::from("WEBVTT\n");
    for c in cues {
        out.push_str(&format!(
            "\n{} --> {}\n{}\n",
            time(c.start),
            time(c.end),
            lines(&c.text).join("\n")
        ));
    }
    out
}

/// The languages captions can be made in, and those whose model is already
/// on the Mac (locale identifiers such as `en_US`).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Locales {
    pub supported: Vec<String>,
    pub installed: Vec<String>,
}

#[cfg(captions)]
mod speech {
    use std::ffi::{CStr, CString, c_char, c_void};
    use std::path::Path;

    use super::{Locales, Word};

    type Progress = extern "C" fn(*mut c_void, f64);

    unsafe extern "C" {
        fn cleanshotz_captions_available() -> bool;
        fn cleanshotz_captions_locales() -> *mut c_char;
        fn cleanshotz_captions_transcribe(
            path: *const c_char,
            locale: *const c_char,
            progress: Option<Progress>,
            context: *mut c_void,
        ) -> *mut c_char;
        fn free(pointer: *mut c_void);
    }

    /// A string from Swift (strdup), freed after reading.
    fn take(pointer: *mut c_char) -> String {
        if pointer.is_null() {
            return String::new();
        }
        // SAFETY: a NUL-terminated string from strdup, freed once here.
        unsafe {
            let text = CStr::from_ptr(pointer).to_string_lossy().into_owned();
            free(pointer.cast());
            text
        }
    }

    pub fn available() -> bool {
        // SAFETY: no arguments; returns a plain value.
        unsafe { cleanshotz_captions_available() }
    }

    pub fn locales() -> Locales {
        // SAFETY: returns a JSON string for `take`.
        let json = take(unsafe { cleanshotz_captions_locales() });
        serde_json::from_str(&json).unwrap_or_default()
    }

    extern "C" fn report(context: *mut c_void, fraction: f64) {
        // SAFETY: `context` points at the closure passed to `transcribe`,
        // alive while it runs.
        let progress = unsafe { &*(context as *const &dyn Fn(f64)) };
        progress(fraction);
    }

    pub fn transcribe(
        path: &Path,
        locale: &str,
        progress: &dyn Fn(f64),
    ) -> Result<Vec<Word>, String> {
        #[derive(serde::Deserialize)]
        struct Outcome {
            #[serde(default)]
            words: Vec<Word>,
            error: Option<String>,
        }
        let path = CString::new(path.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
        let locale = CString::new(locale).map_err(|e| e.to_string())?;
        let context: *const &dyn Fn(f64) = &progress;
        // SAFETY: valid C strings; the progress context outlives the call,
        // which blocks until done.
        let json = take(unsafe {
            cleanshotz_captions_transcribe(
                path.as_ptr(),
                locale.as_ptr(),
                Some(report),
                context as *mut c_void,
            )
        });
        let outcome: Outcome =
            serde_json::from_str(&json).map_err(|e| format!("unexpected transcription: {e}"))?;
        match outcome.error {
            Some(e) => Err(e),
            None => Ok(outcome.words),
        }
    }
}

#[cfg(not(captions))]
mod speech {
    use std::path::Path;

    use super::{Locales, Word};

    pub fn available() -> bool {
        false
    }

    pub fn locales() -> Locales {
        Locales::default()
    }

    pub fn transcribe(
        _path: &Path,
        _locale: &str,
        _progress: &dyn Fn(f64),
    ) -> Result<Vec<Word>, String> {
        Err("this build of CleanShotZ cannot make captions".into())
    }
}

/// Whether captions can be made on this Mac (macOS 26 and later).
pub fn available() -> bool {
    speech::available()
}

/// The languages captions can be made in. Blocks; call it off the main
/// thread.
pub fn locales() -> Locales {
    speech::locales()
}

/// The words said in the audio of `path` (a recording), in `locale`
/// (`en_US`). Downloads the language's model the first time, reporting
/// `progress` from 0 to 1. Blocks; call it off the main thread.
pub fn transcribe(
    path: &std::path::Path,
    locale: &str,
    progress: &dyn Fn(f64),
) -> Result<Vec<Word>, String> {
    speech::transcribe(path, locale, progress)
}

/// The best language for captions: the Mac's own, or a variant of it,
/// else English, else the first one.
pub fn preferred(locales: &Locales, current: &str) -> Option<String> {
    let language = |id: &str| id.split(['_', '-']).next().unwrap_or("").to_string();
    let supported = &locales.supported;
    supported
        .iter()
        .find(|l| *l == current)
        .or_else(|| supported.iter().find(|l| language(l) == language(current)))
        .or_else(|| supported.iter().find(|l| *l == "en_US"))
        .or_else(|| supported.first())
        .cloned()
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
    fn groups_words_into_cues_at_sentences_and_pauses() {
        let cues = make_cues(&words(&[
            ("Welcome", 0.0, 0.48),
            (" to", 0.48, 0.6),
            (" CleanShotZ.", 0.6, 1.44),
            (" First,", 1.5, 1.9),
            (" open", 1.9, 2.3),
            (" the", 2.3, 2.5),
            (" tab", 2.5, 2.9),
            // A long pause: a new cue.
            (" Then", 5.0, 5.3),
            (" export.", 5.3, 5.5),
        ]));
        assert_eq!(
            cues,
            vec![
                Cue {
                    start: 0.0,
                    end: 1.44,
                    text: "Welcome to CleanShotZ.".into()
                },
                Cue {
                    start: 1.5,
                    end: 2.9,
                    text: "First, open the tab".into()
                },
                // Short: kept up for a second.
                Cue {
                    start: 5.0,
                    end: 6.0,
                    text: "Then export.".into()
                },
            ]
        );
    }

    #[test]
    fn keeps_cues_to_two_lines_and_six_seconds() {
        let many: Vec<(String, f64, f64)> = (0..30)
            .map(|i| (format!(" word{i}"), i as f64 * 0.4, i as f64 * 0.4 + 0.3))
            .collect();
        let spec: Vec<(&str, f64, f64)> =
            many.iter().map(|(t, s, e)| (t.as_str(), *s, *e)).collect();
        let cues = make_cues(&words(&spec));
        assert!(cues.len() > 2);
        for c in &cues {
            assert!(c.text.chars().count() <= MAX_CHARS, "{c:?}");
            assert!(c.end - c.start <= MAX_SECONDS + 0.5, "{c:?}");
        }
        // Every word is used once.
        let joined: Vec<&str> = cues.iter().flat_map(|c| c.text.split(' ')).collect();
        assert_eq!(joined.len(), 30);
    }

    #[test]
    fn wraps_into_two_balanced_lines() {
        assert_eq!(lines("Short one"), vec!["Short one"]);
        let long = "In this video I'll show you how to export a quarterly report quickly";
        let two = lines(long);
        assert_eq!(two.len(), 2);
        assert_eq!(two.join(" "), long);
        let (a, b) = (two[0].len() as isize, two[1].len() as isize);
        assert!((a - b).abs() < 12, "{two:?}");
    }

    #[test]
    fn writes_srt_and_vtt() {
        let cues = vec![
            Cue {
                start: 0.5,
                end: 2.25,
                text: "Hello there.".into(),
            },
            Cue {
                start: 3661.0,
                end: 3662.5,
                text: "An hour later.".into(),
            },
        ];
        assert_eq!(
            srt(&cues),
            "1\n00:00:00,500 --> 00:00:02,250\nHello there.\n\n2\n01:01:01,000 --> 01:01:02,500\nAn hour later.\n"
        );
        assert_eq!(
            vtt(&cues),
            "WEBVTT\n\n00:00:00.500 --> 00:00:02.250\nHello there.\n\n01:01:01.000 --> 01:01:02.500\nAn hour later.\n"
        );
    }

    #[test]
    fn prefers_the_macs_language() {
        let locales = Locales {
            supported: vec![
                "de_DE".into(),
                "en_GB".into(),
                "en_US".into(),
                "fr_FR".into(),
            ],
            installed: vec![],
        };
        assert_eq!(preferred(&locales, "fr_FR").as_deref(), Some("fr_FR"));
        assert_eq!(preferred(&locales, "de_AT").as_deref(), Some("de_DE"));
        assert_eq!(preferred(&locales, "ja_JP").as_deref(), Some("en_US"));
        assert_eq!(preferred(&Locales::default(), "en_US"), None);
    }
}
