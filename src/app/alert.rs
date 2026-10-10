use objc2::MainThreadMarker;
use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSApplication};
use objc2_foundation::NSString;

/// Shows a modal alert and returns the index of the button that was clicked.
///
/// The app is activated first: as a menu bar (accessory) app it is usually
/// not frontmost, and the alert would otherwise open behind other windows.
pub fn show(mtm: MainThreadMarker, title: &str, message: &str, buttons: &[&str]) -> usize {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(title));
    alert.setInformativeText(&NSString::from_str(message));
    for button in buttons {
        alert.addButtonWithTitle(&NSString::from_str(button));
    }
    NSApplication::sharedApplication(mtm).activate();
    let response = alert.runModal();
    usize::try_from(response - NSAlertFirstButtonReturn).unwrap_or(0)
}
