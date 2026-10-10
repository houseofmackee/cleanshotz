//! Text recognition with Apple's Vision framework, running on device.

pub mod sensitive;

use std::ops::Range;

use dispatch2::DispatchQueue;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_core_foundation::CFRetained;

use crate::config::TextSettings;

use objc2::AllocAnyThread;
use objc2::rc::Retained;
use objc2_core_graphics::CGImage;
use objc2_foundation::{NSArray, NSDictionary, NSError, NSRange, NSString};
use objc2_vision::{
    VNImageRequestHandler, VNRecognizeTextRequest, VNRecognizedText, VNRectangleObservation,
    VNRequest, VNRequestTextRecognitionLevel,
};

use crate::capture::geometry::Rect;

// Process-wide: text is also recognized on background threads.
static SETTINGS: std::sync::Mutex<TextSettings> = std::sync::Mutex::new(TextSettings::DEFAULT);

pub fn configure(settings: TextSettings) {
    if let Ok(mut s) = SETTINGS.lock() {
        *s = settings;
    }
}

pub fn settings() -> TextSettings {
    SETTINGS.lock().map(|s| s.clone()).unwrap_or_default()
}

/// Recognizes the text in `image` on a background thread, copies it to the
/// clipboard and confirms with a toast.
pub fn copy_text(image: CFRetained<CGImage>) {
    let settings = settings();
    std::thread::spawn(move || {
        let result = recognize(&image, &settings, |_| Vec::new())
            .map(|lines| assemble(&lines, settings.keep_line_breaks));
        DispatchQueue::main().exec_async(move || {
            let mtm = MainThreadMarker::new().expect("the main queue runs on the main thread");
            match result {
                Ok(text) if text.is_empty() => crate::ui::toast::show(mtm, "No text found"),
                Ok(text) => {
                    copy_string(&text);
                    tracing::info!(characters = text.chars().count(), "text copied");
                    crate::ui::toast::show(mtm, &format!("Copied: {}", preview(&text)));
                }
                Err(e) => {
                    tracing::error!(error = %e, "text recognition failed");
                    crate::ui::toast::show(mtm, "Text recognition failed");
                }
            }
        });
    });
}

pub fn copy_string(text: &str) {
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    // SAFETY: reading an AppKit constant.
    let string_type = unsafe { NSPasteboardTypeString };
    pasteboard.setString_forType(&NSString::from_str(text), string_type);
}

/// The first line, shortened to about 40 characters.
fn preview(text: &str) -> String {
    let first = text.lines().next().unwrap_or_default();
    let mut short: String = first.chars().take(40).collect();
    if short.len() < first.len() || text.lines().nth(1).is_some() {
        short.push('…');
    }
    format!("“{short}”")
}

/// One recognized line. `bounds` is normalized to the image (0 to 1) with
/// a top-left origin.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub text: String,
    pub bounds: Rect,
    /// Boxes of the byte ranges requested via [`recognize`]'s `find`.
    pub matches: Vec<Rect>,
}

/// Recognizes text in `image`. `languages` are BCP 47 codes such as
/// `"en-US"`; empty means automatic detection. For every line, `find`
/// returns byte ranges whose boxes should be located (for redaction).
pub fn recognize(
    image: &CGImage,
    settings: &TextSettings,
    find: impl Fn(&str) -> Vec<Range<usize>>,
) -> Result<Vec<Line>, String> {
    let languages = &settings.languages;
    let request = VNRecognizeTextRequest::new();
    request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
    // Correction helps with blurry photos but on crisp screen text mostly
    // "fixes" codes and identifiers (it splits AKIA...7EXAMPLE in two).
    request.setUsesLanguageCorrection(settings.language_correction);
    // Without configured languages, use the system's preferred languages
    // that Vision supports. Automatic detection is the last resort: on
    // small or blurry text it can pick look-alike letters from another
    // script (a Cyrillic "р" for a Latin "p"), which breaks copied code.
    let languages = if languages.is_empty() {
        // SAFETY: a plain query on a configured request.
        let supported: Vec<String> =
            unsafe { request.supportedRecognitionLanguagesAndReturnError() }
                .map(|list| list.iter().map(|l| l.to_string()).collect())
                .unwrap_or_default();
        let preferred: Vec<String> = objc2_foundation::NSLocale::preferredLanguages()
            .iter()
            .map(|l| l.to_string())
            .collect();
        match_languages(&preferred, &supported)
    } else {
        languages.to_vec()
    };
    if languages.is_empty() {
        request.setAutomaticallyDetectsLanguage(true);
    } else {
        let languages: Vec<Retained<NSString>> =
            languages.iter().map(|l| NSString::from_str(l)).collect();
        request.setRecognitionLanguages(&NSArray::from_retained_slice(&languages));
    }

    // SAFETY: valid image and an empty options dictionary.
    let handler = unsafe {
        VNImageRequestHandler::initWithCGImage_options(
            VNImageRequestHandler::alloc(),
            image,
            &NSDictionary::new(),
        )
    };
    let requests: Retained<NSArray<VNRequest>> =
        NSArray::from_retained_slice(&[Retained::into_super(Retained::into_super(
            request.clone(),
        ))]);
    handler
        .performRequests_error(&requests)
        .map_err(|e| e.localizedDescription().to_string())?;

    let mut lines = Vec::new();
    for observation in request.results().unwrap_or_default().iter() {
        let Some(candidate) = observation.topCandidates(1).firstObject() else {
            continue;
        };
        let raw = candidate.string().to_string();
        let text = fix_homoglyphs(&raw);
        // SAFETY: plain property read.
        let bounds = to_top_left(unsafe { observation.boundingBox() });
        let matches = find(&text)
            .into_iter()
            .map(|range| {
                // Same characters, possibly different byte lengths: map
                // the range back onto Vision's string.
                let range = same_chars(&text, &raw, range);
                let text = &raw;
                // Vision measures whole words, so measure the word around
                // the match and narrow it down to the match.
                let word = word_around(text, range.clone());
                match substring_box(&candidate, text, word.clone()) {
                    Some(word_box) => estimate_box(
                        &text[word.clone()],
                        range.start - word.start..range.end - word.start,
                        word_box,
                    ),
                    None => estimate_box(text, range, bounds),
                }
            })
            .collect();
        lines.push(Line {
            text,
            bounds,
            matches,
        });
    }
    Ok(lines)
}

/// Replaces Cyrillic and Greek letters that look like Latin ones, but only
/// in words that also contain ASCII letters: OCR sometimes reads "p" as
/// Cyrillic "р" in code, while a word written in Cyrillic stays intact.
/// Keeps the number of characters.
pub fn fix_homoglyphs(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for word in text.split_inclusive(char::is_whitespace) {
        let has_ascii_letter = word.chars().any(|c| c.is_ascii_alphabetic());
        for c in word.chars() {
            out.push(if has_ascii_letter {
                latin_lookalike(c).unwrap_or(c)
            } else {
                c
            });
        }
    }
    out
}

fn latin_lookalike(c: char) -> Option<char> {
    Some(match c {
        // Cyrillic
        'А' => 'A',
        'В' => 'B',
        'Е' => 'E',
        'К' => 'K',
        'М' => 'M',
        'Н' => 'H',
        'О' => 'O',
        'Р' => 'P',
        'С' => 'C',
        'Т' => 'T',
        'У' => 'Y',
        'Х' => 'X',
        'а' => 'a',
        'е' => 'e',
        'о' => 'o',
        'р' => 'p',
        'с' => 'c',
        'у' => 'y',
        'х' => 'x',
        'і' => 'i',
        'ј' => 'j',
        'ѕ' => 's',
        'І' => 'I',
        'Ј' => 'J',
        'Ѕ' => 'S',
        // Greek
        'Α' => 'A',
        'Β' => 'B',
        'Ε' => 'E',
        'Ζ' => 'Z',
        'Η' => 'H',
        'Ι' => 'I',
        'Κ' => 'K',
        'Μ' => 'M',
        'Ν' => 'N',
        'Ο' => 'O',
        'Ρ' => 'P',
        'Τ' => 'T',
        'Υ' => 'Y',
        'Χ' => 'X',
        'ο' => 'o',
        'ν' => 'v',
        _ => return None,
    })
}

/// `range` (bytes in `from`) as bytes in `to`, where both strings have the
/// same characters count and positions.
fn same_chars(from: &str, to: &str, range: Range<usize>) -> Range<usize> {
    let start_char = from[..range.start].chars().count();
    let len_chars = from[range].chars().count();
    let byte = |n: usize| to.char_indices().nth(n).map_or(to.len(), |(i, _)| i);
    byte(start_char)..byte(start_char + len_chars)
}

/// For each preferred language (like `en-GB`), the supported recognition
/// language with the same code, or else the first with the same base
/// language (`en-US`). Keeps preference order, without duplicates.
fn match_languages(preferred: &[String], supported: &[String]) -> Vec<String> {
    let base = |code: &str| {
        code.split(['-', '_'])
            .next()
            .unwrap_or(code)
            .to_ascii_lowercase()
    };
    let mut matched: Vec<String> = Vec::new();
    for want in preferred {
        let found = supported
            .iter()
            .find(|s| s.eq_ignore_ascii_case(want))
            .or_else(|| supported.iter().find(|s| base(s) == base(want)));
        if let Some(found) = found
            && !matched.contains(found)
        {
            matched.push(found.clone());
        }
    }
    matched
}

/// Vision's normalized rects have a bottom-left origin.
fn to_top_left(r: objc2_core_foundation::CGRect) -> Rect {
    Rect::new(
        r.origin.x,
        1.0 - (r.origin.y + r.size.height),
        r.size.width,
        r.size.height,
    )
}

/// The box of a byte range of `text`, as Vision measured it.
fn substring_box(candidate: &VNRecognizedText, text: &str, range: Range<usize>) -> Option<Rect> {
    let start = text[..range.start].encode_utf16().count();
    let length = text[range].encode_utf16().count();
    let ns_range = NSRange::new(start, length);
    // SAFETY: `boundingBoxForRange:error:` takes an NSRange within the
    // candidate's string and returns a rectangle observation or an error.
    let result: Result<Retained<VNRectangleObservation>, Retained<NSError>> =
        unsafe { objc2::msg_send![candidate, boundingBoxForRange: ns_range, error: _] };
    // SAFETY: plain property read.
    result.ok().map(|o| to_top_left(unsafe { o.boundingBox() }))
}

/// `range` widened to the whitespace-separated word(s) containing it.
fn word_around(text: &str, range: Range<usize>) -> Range<usize> {
    let start = text[..range.start]
        .rfind(char::is_whitespace)
        .map_or(0, |i| {
            i + text[i..].chars().next().map_or(1, char::len_utf8)
        });
    let end = text[range.end..]
        .find(char::is_whitespace)
        .map_or(text.len(), |i| range.end + i);
    start..end
}

/// A horizontal slice of `outer` (the box of `text`) proportional to the
/// characters in `range`.
fn estimate_box(text: &str, range: Range<usize>, line: Rect) -> Rect {
    let total = text.chars().count().max(1) as f64;
    let before = text[..range.start].chars().count() as f64;
    let len = text[range].chars().count() as f64;
    Rect::new(
        line.x + line.width * before / total,
        line.y,
        line.width * len / total,
        line.height,
    )
}

/// Joins recognized lines into text in reading order. Lines whose vertical
/// centers are within half a line height are on the same row and joined
/// with a space; rows are joined with newlines, or spaces when
/// `keep_line_breaks` is off.
pub fn assemble(lines: &[Line], keep_line_breaks: bool) -> String {
    let mut sorted: Vec<&Line> = lines.iter().filter(|l| !l.text.trim().is_empty()).collect();
    sorted.sort_by(|a, b| a.bounds.y.total_cmp(&b.bounds.y));
    let mut rows: Vec<Vec<&Line>> = Vec::new();
    for line in sorted {
        let center = line.bounds.y + line.bounds.height / 2.0;
        match rows.last_mut() {
            Some(row)
                if {
                    let first = row[0];
                    let row_center = first.bounds.y + first.bounds.height / 2.0;
                    (center - row_center).abs() < first.bounds.height.min(line.bounds.height) / 2.0
                } =>
            {
                row.push(line)
            }
            _ => rows.push(vec![line]),
        }
    }
    let separator = if keep_line_breaks { "\n" } else { " " };
    rows.into_iter()
        .map(|mut row| {
            row.sort_by(|a, b| a.bounds.x.total_cmp(&b.bounds.x));
            row.iter()
                .map(|l| l.text.trim())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join(separator)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, x: f64, y: f64) -> Line {
        Line {
            text: text.into(),
            bounds: Rect::new(x, y, 0.2, 0.04),
            matches: Vec::new(),
        }
    }

    #[test]
    fn assembles_rows_in_reading_order() {
        let lines = [
            line("second line", 0.1, 0.20),
            line("right", 0.6, 0.105),
            line("first", 0.1, 0.10),
        ];
        assert_eq!(assemble(&lines, true), "first right\nsecond line");
        assert_eq!(assemble(&lines, false), "first right second line");
    }

    #[test]
    fn previews_the_first_line() {
        assert_eq!(preview("short"), "“short”");
        assert_eq!(preview("one\ntwo"), "“one…”");
        assert_eq!(preview(&"x".repeat(50)), format!("“{}…”", "x".repeat(40)));
    }

    #[test]
    fn matches_preferred_languages_to_supported_ones() {
        let supported: Vec<String> = ["en-US", "fr-FR", "zh-Hans", "pt-BR"]
            .map(String::from)
            .to_vec();
        let preferred: Vec<String> = ["en-GB", "en-US", "zh-Hans-CN", "de-DE"]
            .map(String::from)
            .to_vec();
        assert_eq!(
            match_languages(&preferred, &supported),
            vec!["en-US", "zh-Hans"]
        );
        assert!(match_languages(&["ja-JP".into()], &supported).is_empty());
    }

    #[test]
    fn fixes_lookalike_letters_in_latin_words_only() {
        assert_eq!(fix_homoglyphs("GITHUB_ТOKEN=ghр_x9"), "GITHUB_TOKEN=ghp_x9");
        assert_eq!(fix_homoglyphs("Привет мир"), "Привет мир");
        let fixed = fix_homoglyphs("ghр_abc");
        assert_eq!(fixed.chars().count(), "ghр_abc".chars().count());
        assert_eq!(&"ghр_abc"[same_chars(&fixed, "ghр_abc", 0..4)], "ghр_");
    }

    #[test]
    fn widens_matches_to_whole_words() {
        let text = "key OWNER=jane@x.io here";
        assert_eq!(&text[word_around(text, 10..19)], "OWNER=jane@x.io");
        assert_eq!(word_around("abc", 0..3), 0..3);
    }

    #[test]
    fn estimates_substring_boxes_proportionally() {
        let b = estimate_box("abcdefghij", 2..4, Rect::new(0.0, 0.5, 1.0, 0.1));
        assert!((b.x - 0.2).abs() < 1e-9 && (b.width - 0.2).abs() < 1e-9);
        assert_eq!((b.y, b.height), (0.5, 0.1));
    }
}
