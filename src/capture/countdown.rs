//! The self-timer countdown: a small HUD over the selected area that counts
//! down, then runs the capture.

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSFont, NSScreen, NSScreenSaverWindowLevel, NSTextAlignment,
    NSTextField, NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString, NSTimer};

use super::geometry::Rect;

const SIZE: f64 = 120.0;

/// Shows the countdown centered on `rect` (display-local points on
/// `screen`) and calls `then` after `seconds`. The HUD belongs to this app,
/// so captures exclude it.
pub fn start(
    mtm: MainThreadMarker,
    screen: &NSScreen,
    rect: Rect,
    seconds: u32,
    then: impl FnOnce() + 'static,
) {
    let frame = screen.frame();
    let center = NSPoint::new(
        frame.origin.x + rect.x + rect.width / 2.0,
        frame.origin.y + frame.size.height - (rect.y + rect.height / 2.0),
    );
    let hud_frame = NSRect::new(
        NSPoint::new(center.x - SIZE / 2.0, center.y - SIZE / 2.0),
        NSSize::new(SIZE, SIZE),
    );
    // SAFETY: designated NSWindow initializer.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            hud_frame,
            NSWindowStyleMask::Borderless,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // SAFETY: we keep a strong reference until the countdown ends.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setLevel(NSScreenSaverWindowLevel);
    window.setOpaque(false);
    window.setBackgroundColor(Some(&NSColor::colorWithWhite_alpha(0.0, 0.7)));
    window.setIgnoresMouseEvents(true);
    window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::Stationary,
    );

    let label = NSTextField::labelWithString(&NSString::from_str(&seconds.to_string()), mtm);
    label.setFont(Some(&NSFont::boldSystemFontOfSize(64.0)));
    label.setTextColor(Some(&NSColor::whiteColor()));
    label.setAlignment(NSTextAlignment::Center);
    label.setFrame(NSRect::new(
        NSPoint::new(0.0, 22.0),
        NSSize::new(SIZE, 76.0),
    ));
    if let Some(content) = window.contentView() {
        content.addSubview(&label);
    }
    window.orderFrontRegardless();

    let remaining = Cell::new(seconds);
    let then: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(Some(Box::new(then)));
    let hud: Retained<NSWindow> = window;
    let block = RcBlock::new(move |timer: NonNull<NSTimer>| {
        let left = remaining.get().saturating_sub(1);
        remaining.set(left);
        if left > 0 {
            label.setStringValue(&NSString::from_str(&left.to_string()));
            return;
        }
        // SAFETY: the timer passed to its own block is valid.
        unsafe { timer.as_ref() }.invalidate();
        hud.orderOut(None);
        if let Some(then) = then.borrow_mut().take() {
            then();
        }
    });
    // SAFETY: the block has the expected signature; the timer is scheduled
    // on the main run loop, which owns it until it is invalidated.
    unsafe {
        NSTimer::scheduledTimerWithTimeInterval_repeats_block(1.0, true, &block);
    }
}
