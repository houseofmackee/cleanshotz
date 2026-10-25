//! A brief message near the bottom of the screen, like the macOS volume
//! HUD. It never takes focus; it ignores the mouse unless it offers an
//! action ([`show_with_action`]), which a click runs.

use std::cell::RefCell;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSBox, NSBoxType, NSColor, NSEvent, NSFont, NSPanel, NSStatusWindowLevel,
    NSTextAlignment, NSTextField, NSTitlePosition, NSView, NSWindowCollectionBehavior,
    NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString, NSTimer};

use crate::capture::screen_under_mouse;

thread_local! {
    static CURRENT: RefCell<Option<Retained<NSPanel>>> = const { RefCell::new(None) };
}

pub struct ClickIvars {
    action: Box<dyn Fn()>,
}

define_class!(
    // SAFETY: NSView can be subclassed; no Drop implementation.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ClickIvars]
    struct ClickView;

    impl ClickView {
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            // Hiding releases the panel and this view; keep it until done.
            let this = self.retain();
            hide();
            (this.ivars().action)();
        }
    }
);

fn hide() {
    CURRENT.with(|c| {
        if let Some(old) = c.borrow_mut().take() {
            old.orderOut(None);
        }
    });
}

/// Shows `message` for about two seconds, replacing any toast on screen.
pub fn show(mtm: MainThreadMarker, message: &str) {
    present(mtm, message, None, None);
}

/// Shows `message` next to a swatch of `color` (sRGB).
pub fn show_color(mtm: MainThreadMarker, message: &str, color: [u8; 3]) {
    present(mtm, message, None, Some(color));
}

/// Shows `message` with `hint` (such as "Click to open") for about four
/// seconds; clicking the toast, or VoiceOver's `action_name`, runs `action`.
pub fn show_with_action(
    mtm: MainThreadMarker,
    message: &str,
    hint: &str,
    action_name: &str,
    action: impl Fn() + 'static,
) {
    let text = format!("{}\n{}", crate::i18n::tr(message), crate::i18n::tr(hint));
    present(
        mtm,
        &text,
        Some((crate::i18n::tr(action_name), Box::new(action))),
        None,
    );
}

const SWATCH: f64 = 18.0;

fn present(
    mtm: MainThreadMarker,
    message: &str,
    action: Option<(String, Box<dyn Fn()>)>,
    swatch: Option<[u8; 3]>,
) {
    let message = &crate::i18n::tr(message);
    // VoiceOver users cannot see the toast.
    super::access::announce(mtm, message);
    hide();
    let label = NSTextField::labelWithString(&NSString::from_str(message), mtm);
    label.setFont(Some(&NSFont::systemFontOfSize(15.0)));
    label.setTextColor(Some(&NSColor::whiteColor()));
    label.setAlignment(NSTextAlignment::Center);
    label.sizeToFit();
    let text = label.frame().size;
    let extra = if swatch.is_some() { SWATCH + 8.0 } else { 0.0 };
    let size = NSSize::new((text.width + extra + 40.0).max(160.0), text.height + 24.0);

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
    panel.setIgnoresMouseEvents(action.is_none());
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
    // The swatch and the text, centered together.
    let left = ((size.width - extra - text.width) / 2.0).round();
    label.setFrame(NSRect::new(
        NSPoint::new(left + extra, (size.height - text.height) / 2.0),
        NSSize::new(text.width, text.height),
    ));
    background.addSubview(&label);
    if let Some([r, g, b]) = swatch {
        let chip = NSBox::new(mtm);
        chip.setBoxType(NSBoxType::Custom);
        chip.setTitlePosition(NSTitlePosition::NoTitle);
        chip.setCornerRadius(4.0);
        chip.setBorderWidth(1.0);
        chip.setBorderColor(&NSColor::colorWithWhite_alpha(1.0, 0.6));
        chip.setFillColor(&NSColor::colorWithSRGBRed_green_blue_alpha(
            f64::from(r) / 255.0,
            f64::from(g) / 255.0,
            f64::from(b) / 255.0,
            1.0,
        ));
        chip.setFrame(NSRect::new(
            NSPoint::new(left, ((size.height - SWATCH) / 2.0).round()),
            NSSize::new(SWATCH, SWATCH),
        ));
        background.addSubview(&chip);
    }
    let seconds = match action {
        Some((name, action)) => {
            let view = ClickView::alloc(mtm).set_ivars(ClickIvars {
                action: Box::new(action),
            });
            // SAFETY: NSView's designated initializer.
            let view: Retained<ClickView> =
                unsafe { msg_send![super(view), initWithFrame: NSRect::new(NSPoint::ZERO, size)] };
            view.addSubview(&background);
            // Weak: the view keeps its accessibility actions alive.
            let run = objc2::rc::Weak::from_retained(&view);
            super::access::actions(
                &view,
                vec![(
                    name.clone(),
                    Box::new(move || {
                        hide();
                        if let Some(view) = run.load() {
                            (view.ivars().action)();
                        }
                    }),
                )],
            );
            panel.setContentView(Some(&view));
            4.0
        }
        None => {
            panel.setContentView(Some(&background));
            2.0
        }
    };
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
        NSTimer::scheduledTimerWithTimeInterval_repeats_block(seconds, false, &block);
    }
}
