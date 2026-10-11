pub mod hotkeys;
pub mod login_item;
pub mod permissions;

use objc2_app_kit::NSWorkspace;
use objc2_foundation::{NSString, NSURL};

/// Opens `url` in its default app (a web link in the browser).
pub fn open_url(url: &str) {
    if let Some(url) = NSURL::URLWithString(&NSString::from_str(url)) {
        NSWorkspace::sharedWorkspace().openURL(&url);
    }
}
