//! Localization. User-facing text is looked up by its English wording in
//! the app bundle's `Localizable.strings` (`resources/<language>.lproj`),
//! falling back to English when there is no translation.
//!
//! Text passed to alerts, toasts, menu items and the Settings window is
//! translated where it is shown; mark any other user-facing literal with
//! [`t!`]. `cargo xtask strings` collects both kinds into
//! `resources/en.lproj/Localizable.strings`, the template for translators.
//! Text assembled at runtime (with `format!`) is not translated yet.

use objc2_foundation::{NSBundle, NSString};

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

/// Marks a user-facing string literal for translation and translates it.
#[macro_export]
macro_rules! t {
    ($text:literal) => {
        $crate::i18n::tr($text)
    };
}
