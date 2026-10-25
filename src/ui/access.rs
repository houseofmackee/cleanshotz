//! VoiceOver support for custom-drawn views: roles, labels, named actions
//! (VoiceOver's Actions rotor, VO+Command+Space) and spoken announcements.
//! Standard AppKit controls describe themselves.

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool};
use objc2::{AllocAnyThread, MainThreadMarker};
use objc2_app_kit::{
    NSAccessibility, NSAccessibilityAnnouncementKey,
    NSAccessibilityAnnouncementRequestedNotification, NSAccessibilityCustomAction,
    NSAccessibilityPostNotificationWithUserInfo, NSAccessibilityPriorityKey, NSAccessibilityRole,
    NSApplication, NSView,
};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};

/// Makes `view` one accessibility element with `role`, `label` and,
/// optionally, `help` (read after a pause).
pub fn describe(view: &NSView, role: &NSAccessibilityRole, label: &str, help: Option<&str>) {
    view.setAccessibilityElement(true);
    view.setAccessibilityRole(Some(role));
    view.setAccessibilityLabel(Some(&NSString::from_str(label)));
    view.setAccessibilityHelp(help.map(NSString::from_str).as_deref());
}

/// A named action (translated: `t!`) and what it does.
pub type Action = (String, Box<dyn Fn()>);

/// Named actions VoiceOver offers for `view`, each running `f` on the main
/// thread (where accessibility requests arrive).
pub fn actions(view: &NSView, actions: Vec<Action>) {
    let list: Vec<Retained<NSAccessibilityCustomAction>> = actions
        .into_iter()
        .map(|(name, f)| {
            let block = RcBlock::new(move || {
                f();
                Bool::YES
            });
            NSAccessibilityCustomAction::initWithName_handler(
                NSAccessibilityCustomAction::alloc(),
                &NSString::from_str(&name),
                Some(&block),
            )
        })
        .collect();
    view.setAccessibilityCustomActions(Some(&NSArray::from_retained_slice(&list)));
}

/// Has VoiceOver speak `text` (when VoiceOver is running; otherwise
/// nothing happens).
pub fn announce(mtm: MainThreadMarker, text: &str) {
    let app = NSApplication::sharedApplication(mtm);
    // SAFETY: reading AppKit constants; the user info maps the documented
    // keys to a string and a number.
    unsafe {
        let info = NSDictionary::<NSString, AnyObject>::from_retained_objects(
            &[NSAccessibilityAnnouncementKey, NSAccessibilityPriorityKey],
            &[
                NSString::from_str(text).into(),
                // NSAccessibilityPriorityHigh
                NSNumber::new_isize(90).into(),
            ],
        );
        NSAccessibilityPostNotificationWithUserInfo(
            &app,
            NSAccessibilityAnnouncementRequestedNotification,
            Some(&info),
        );
    }
}
