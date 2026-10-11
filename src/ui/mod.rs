pub mod access;
pub mod draw;
pub mod panels;
pub mod print;
pub mod toast;

use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSWindow};

/// Shows `window` in front and makes it key. macOS may decline to make
/// CleanShotZ the active app when nothing the user just did asked for it
/// (a recording finishing, a command from the terminal), so the window is
/// also ordered in front regardless: it shows above the active app's
/// windows and becomes key with the first click.
pub fn bring_forward(mtm: MainThreadMarker, window: &NSWindow) {
    NSApplication::sharedApplication(mtm).activate();
    window.makeKeyAndOrderFront(None);
    window.orderFrontRegardless();
}
