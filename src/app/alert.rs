use objc2::MainThreadMarker;
use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSApplication};
use objc2_foundation::NSString;

use crate::i18n::tr;

/// Shows a modal alert and returns the index of the button that was clicked.
/// The texts are translated (see [`crate::i18n`]).
///
/// The app is activated first: as a menu bar (accessory) app it is usually
/// not frontmost, and the alert would otherwise open behind other windows.
pub fn show(mtm: MainThreadMarker, title: &str, message: &str, buttons: &[&str]) -> usize {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(&tr(title)));
    alert.setInformativeText(&NSString::from_str(&tr(message)));
    for button in buttons {
        alert.addButtonWithTitle(&NSString::from_str(&tr(button)));
    }
    NSApplication::sharedApplication(mtm).activate();
    let response = alert.runModal();
    usize::try_from(response - NSAlertFirstButtonReturn).unwrap_or(0)
}
