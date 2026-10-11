//! Localization. User-facing text is looked up by its English wording in
//! the app bundle's `Localizable.strings` (`resources/<language>.lproj`),
//! falling back to English when there is no translation.
//!
//! Text passed to alerts, toasts, menu items and the Settings window is
//! translated where it is shown; mark any other user-facing literal with
//! [`t!`]. `cargo xtask strings` collects both kinds into
//! `resources/en.lproj/Localizable.strings`, the template for translators.
//!
//! Text assembled at runtime uses `t!` with named placeholders:
//! `t!("{n} captures", n)` or `t!("Copied: {text}", text = preview(&s))`.
//! The template is translated first and the values filled in after, so a
//! translation can move the placeholders. Numbers in the values are not
//! localized; dates and file sizes come from [`date`] and [`bytes`].
//! Error details from command parsing, files and uploads stay in English
//! (they also go to the command line and the log).

use objc2_foundation::{
    NSBundle, NSByteCountFormatter, NSByteCountFormatterCountStyle, NSDate, NSDateFormatter,
    NSDateFormatterStyle, NSString,
};

/// `text` in the user's language, if the bundle has a translation.
pub fn tr(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let key = NSString::from_str(text);
    NSBundle::mainBundle()
        .localizedStringForKey_value_table(&key, Some(&key), None)
        .to_string()
}

/// Replaces each `{name}` in `template` with its value; `{{` and `}}` are
/// literal braces. Unknown placeholders are left as they are.
pub fn fill(template: &str, values: &[(&str, String)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(i) = rest.find(['{', '}']) {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        if tail.starts_with("{{") || tail.starts_with("}}") {
            out.push_str(&tail[..1]);
            rest = &tail[2..];
            continue;
        }
        let value = tail.strip_prefix('{').and_then(|t| {
            let end = t.find('}')?;
            let (_, value) = values.iter().find(|(name, _)| *name == &t[..end])?;
            Some((value, end + 2))
        });
        match value {
            Some((value, len)) => {
                out.push_str(value);
                rest = &tail[len..];
            }
            None => {
                out.push_str(&tail[..1]);
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// A date and time in the user's language and format: `short` for labels
/// ("Oct 10, 17:48"), otherwise in full for VoiceOver.
pub fn date(timestamp: i64, short: bool) -> String {
    let formatter = NSDateFormatter::new();
    if short {
        // `j`: the locale's preferred hour format.
        formatter.setLocalizedDateFormatFromTemplate(&NSString::from_str("MMMdjmm"));
    } else {
        formatter.setDateStyle(NSDateFormatterStyle::LongStyle);
        formatter.setTimeStyle(NSDateFormatterStyle::ShortStyle);
    }
    let date = NSDate::dateWithTimeIntervalSince1970(timestamp as f64);
    formatter.stringFromDate(&date).to_string()
}

/// A file size in the user's language, such as "840 KB" or "12.4 MB".
pub fn bytes(bytes: u64) -> String {
    NSByteCountFormatter::stringFromByteCount_countStyle(
        bytes.min(i64::MAX as u64) as i64,
        NSByteCountFormatterCountStyle::File,
    )
    .to_string()
}

/// Translates a list of titles (menu items, pop-up choices). `cargo xtask
/// strings` collects the literals passed to it.
pub fn labels(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| tr(item)).collect()
}

/// Marks a user-facing string literal for translation and translates it.
/// Named placeholders take values after the template: `t!("{n} items", n)`
/// uses the variable `n`, `t!("{n} items", n = list.len())` an expression.
#[macro_export]
macro_rules! t {
    (@value $name:ident) => {
        $name
    };
    (@value $name:ident = $value:expr) => {
        $value
    };
    ($text:literal) => {
        $crate::i18n::tr($text)
    };
    ($text:literal, $($name:ident $(= $value:expr)?),+ $(,)?) => {{
        let values = [$((stringify!($name), $crate::t!(@value $name $(= $value)?).to_string())),+];
        #[cfg(debug_assertions)]
        for (name, _) in &values {
            debug_assert!(
                $text.contains(&format!("{{{name}}}")),
                "t!: {:?} has no {{{name}}}",
                $text
            );
        }
        $crate::i18n::fill(&$crate::i18n::tr($text), &values)
    }};
}

#[cfg(test)]
mod tests {
    use super::fill;

    #[test]
    fn fills_named_placeholders() {
        let values = [("n", "3".to_string()), ("what", "items".to_string())];
        assert_eq!(fill("{n} {what}", &values), "3 items");
        assert_eq!(fill("{what}: {n}, {n}", &values), "items: 3, 3");
        assert_eq!(fill("{{n}} {n}", &values), "{n} 3");
        assert_eq!(fill("{other} {n}", &values), "{other} 3");
        assert_eq!(fill("open { brace", &values), "open { brace");
        assert_eq!(fill("“{what}”", &values), "“items”");
    }

    #[test]
    fn formats_sizes_and_dates() {
        // The digits stay the same in every language.
        assert!(super::bytes(840_000).contains("840"));
        assert!(super::date(0, false).contains("19"));
    }

    #[test]
    fn macro_takes_variables_and_expressions() {
        let n = 2;
        assert_eq!(t!("{n} captures", n), "2 captures");
        assert_eq!(t!("{a} by {b}", a = 3 * 2, b = "x"), "6 by x");
    }
}
