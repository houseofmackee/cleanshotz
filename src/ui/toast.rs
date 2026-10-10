//! A brief message near the bottom of the screen, like the macOS volume
//! HUD. It never takes focus and ignores the mouse.

use std::cell::RefCell;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSBox, NSBoxType, NSColor, NSFont, NSPanel, NSStatusWindowLevel,
    NSTextAlignment, NSTextField, NSTitlePosition, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString, NSTimer};

use crate::capture::screen_under_mouse;

thread_local! {
    static CURRENT: RefCell<Option<Retained<NSPanel>>> = const { RefCell::new(None) };
}

/// Shows `message` for about two seconds, replacing any toast on screen.
pub fn show(mtm: MainThreadMarker, message: &str) {
    CURRENT.with(|c| {
        if let Some(old) = c.borrow_mut().take() {
            old.orderOut(None);
        }
    });
    let label = NSTextField::labelWithString(&NSString::from_str(message), mtm);
    label.setFont(Some(&NSFont::systemFontOfSize(15.0)));
    label.setTextColor(Some(&NSColor::whiteColor()));
    label.setAlignment(NSTextAlignment::Center);
    label.sizeToFit();
    let text = label.frame().size;
    let size = NSSize::new((text.width + 40.0).max(160.0), text.height + 24.0);

    let screen = screen_under_mouse(mtm).map_or(NSRect::ZERO, |s| s.visibleFrame());
    let origin = NSPoint::new(
        screen.origin.x + (screen.size.width - size.width) / 2.0,
        screen.origin.y + 80.0,
    );
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        NSRect::new(origin, size),
        NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
        NSBackingStoreType::Buffered,
        false,
    );
    // SAFETY: CURRENT keeps a strong reference while the toast is shown.
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setLevel(NSStatusWindowLevel);
    panel.setOpaque(false);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    panel.setIgnoresMouseEvents(true);
    panel.setHasShadow(true);
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::Stationary,
    );
    let background = NSBox::new(mtm);
    background.setBoxType(NSBoxType::Custom);
    background.setTitlePosition(NSTitlePosition::NoTitle);
    background.setBorderWidth(0.0);
    background.setCornerRadius(size.height / 2.0);
    background.setFillColor(&NSColor::colorWithWhite_alpha(0.1, 0.85));
    background.setFrame(NSRect::new(NSPoint::ZERO, size));
    label.setFrame(NSRect::new(
        NSPoint::new(0.0, (size.height - text.height) / 2.0),
        NSSize::new(size.width, text.height),
    ));
    panel.setContentView(Some(&background));
    background.addSubview(&label);
    panel.orderFrontRegardless();
    CURRENT.with(|c| *c.borrow_mut() = Some(panel.clone()));

    let block = RcBlock::new(move |_timer: NonNull<NSTimer>| {
        panel.orderOut(None);
        CURRENT.with(|c| {
            let mut current = c.borrow_mut();
            if current.as_ref().is_some_and(|p| *p == panel) {
                *current = None;
            }
        });
    });
    // SAFETY: the block has the expected signature; the run loop owns the
    // one-shot timer.
    unsafe {
        NSTimer::scheduledTimerWithTimeInterval_repeats_block(2.0, false, &block);
    }
}
