//! A running step capture: a control bar, and a screenshot of the window
//! under the pointer at every click in another app.

use std::cell::RefCell;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameDarkAqua, NSBox, NSBoxType, NSButton,
    NSColor, NSEvent, NSEventMask, NSFont, NSPanel, NSScreen, NSTextField, NSTitlePosition,
};
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGContext, CGImage};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use super::Step;
use crate::capture::geometry::Point;
use crate::capture::{self, image, sck};
use crate::config::CaptureSettings;
use crate::ui::panels;

const BAR: NSSize = NSSize {
    width: 470.0,
    height: 44.0,
};
/// More steps than this make an unwieldy guide; later clicks are ignored.
const MAX_STEPS: usize = 100;

struct Active {
    settings: CaptureSettings,
    panel: Retained<NSPanel>,
    status: Retained<NSTextField>,
    monitor: Retained<AnyObject>,
    /// Steps by click order; `None` while being captured or if it failed.
    steps: Vec<Option<Step>>,
    pending: usize,
    /// Done was clicked; the guide opens once `pending` reaches 0.
    finishing: bool,
}

thread_local! {
    static ACTIVE: RefCell<Option<Active>> = const { RefCell::new(None) };
    static TARGET: RefCell<Option<Retained<BarTarget>>> = const { RefCell::new(None) };
}

/// Moves a captured image to the main thread (CGImages are immutable and
/// thread-safe).
struct AssertSend<T>(T);
// SAFETY: see the type's documentation.
unsafe impl<T> Send for AssertSend<T> {}

pub fn is_running() -> bool {
    ACTIVE.with(|a| a.borrow().is_some())
}

/// Starts a step capture, or finishes the running one.
pub fn toggle(mtm: MainThreadMarker, settings: CaptureSettings) {
    if is_running() {
        finish(mtm, true);
    } else {
        start(mtm, settings);
    }
}

fn start(mtm: MainThreadMarker, mut settings: CaptureSettings) {
    // Guides look cleaner without window shadows.
    settings.window_shadow = false;
    let (panel, status) = control_bar(mtm);
    let mask = NSEventMask::LeftMouseDown;
    // Clicks in other apps only: clicks in the control bar are not steps.
    let handler = RcBlock::new(|event: NonNull<NSEvent>| {
        // SAFETY: AppKit passes a valid event.
        let event = unsafe { event.as_ref() };
        if event.clickCount() <= 1
            && let Some(mtm) = MainThreadMarker::new()
        {
            clicked(mtm);
        }
    });
    let Some(monitor) = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(mask, &handler)
    else {
        panel.orderOut(None);
        crate::ui::toast::show(mtm, &t!("Step capture could not watch for clicks"));
        return;
    };
    ACTIVE.with(|a| {
        *a.borrow_mut() = Some(Active {
            settings,
            panel,
            status,
            monitor,
            steps: Vec::new(),
            pending: 0,
            finishing: false,
        });
    });
    crate::ui::access::announce(
        mtm,
        &t!("Step capture started. Each click captures the window under the pointer."),
    );
}

/// A click in another app: notes the clicked element now, before the
/// click changes it, and captures the window in the background.
fn clicked(mtm: MainThreadMarker) {
    let primary = NSScreen::screens(mtm)
        .firstObject()
        .map_or(0.0, |s| s.frame().size.height);
    let p = NSEvent::mouseLocation();
    let at = Point::new(p.x, primary - p.y);
    let Some(index) = ACTIVE.with(|a| {
        let mut a = a.borrow_mut();
        let active = a.as_mut().filter(|a| !a.finishing)?;
        if active.steps.len() >= MAX_STEPS {
            return None;
        }
        active.steps.push(None);
        active.pending += 1;
        Some(active.steps.len() - 1)
    }) else {
        return;
    };
    let settings = ACTIVE.with(|a| a.borrow().as_ref().map(|a| a.settings.clone()));
    let Some(settings) = settings else {
        return;
    };
    let window = sck::Content::fetch().ok().and_then(|c| {
        c.clickable_windows()
            .into_iter()
            .find(|w| w.frame.contains(at))
    });
    let control = window
        .as_ref()
        .and_then(|w| capture::elements::control_at(w.pid, at));
    let caption = super::default_caption(
        control.as_ref().map(|(c, name)| (*c, name.as_str())),
        window.as_ref().map_or("", |w| w.app_name.as_str()),
    );
    update_status();
    std::thread::spawn(move || {
        let step = window.and_then(|w| {
            let captured = capture::grab_window(w.id, &settings)
                .inspect_err(|e| tracing::warn!(error = %e, "step not captured"))
                .ok()?;
            let source = captured.source?;
            let click = Point::new(
                (at.x - source.x) * captured.scale,
                (at.y - source.y) * captured.scale,
            );
            let image = mark(&captured.image, click, captured.scale)?;
            Some(AssertSend((image, captured.scale)))
        });
        dispatch2::DispatchQueue::main().exec_async(move || {
            let step = step.map(|s| s.0).map(|(image, scale)| Step {
                image,
                scale,
                caption,
            });
            captured(index, step);
        });
    });
}

fn captured(index: usize, step: Option<Step>) {
    let done = ACTIVE.with(|a| {
        let mut a = a.borrow_mut();
        let active = a.as_mut()?;
        if let Some(slot) = active.steps.get_mut(index) {
            *slot = step;
        }
        active.pending = active.pending.saturating_sub(1);
        Some(active.finishing && active.pending == 0)
    });
    update_status();
    if done == Some(true)
        && let Some(mtm) = MainThreadMarker::new()
    {
        open_guide(mtm);
    }
}

fn update_status() {
    ACTIVE.with(|a| {
        if let Some(active) = a.borrow().as_ref() {
            let n = active.steps.len();
            let text = match n {
                0 => t!("Click through the task; each click is a step."),
                1 => t!("1 step. Keep clicking, then click Done."),
                n => t!("{n} steps. Keep clicking, then click Done.", n),
            };
            active.status.setStringValue(&NSString::from_str(&text));
        }
    });
}

/// Stops watching clicks; with `keep`, opens the guide once the last
/// captures are in.
fn finish(mtm: MainThreadMarker, keep: bool) {
    let ready = ACTIVE.with(|a| {
        let mut a = a.borrow_mut();
        let active = a.as_mut()?;
        // SAFETY: a monitor this module added.
        unsafe { NSEvent::removeMonitor(&active.monitor) };
        active.panel.orderOut(None);
        active.finishing = true;
        Some(active.pending == 0)
    });
    match ready {
        Some(true) if keep => open_guide(mtm),
        Some(_) if !keep => {
            ACTIVE.with(|a| a.borrow_mut().take());
        }
        _ => {}
    }
}

fn open_guide(mtm: MainThreadMarker) {
    let Some(active) = ACTIVE.with(|a| a.borrow_mut().take()) else {
        return;
    };
    let steps: Vec<Step> = active.steps.into_iter().flatten().collect();
    if steps.is_empty() {
        crate::ui::toast::show(mtm, &t!("No steps were captured"));
        return;
    }
    super::window::open(mtm, steps);
}

/// `image` with a ring around `click` (pixels from the top-left).
fn mark(image: &CGImage, click: Point, scale: f64) -> Option<CFRetained<CGImage>> {
    let (width, height) = image::pixel_size(image);
    let space = CGImage::color_space(Some(image));
    let context = image::bitmap_context(width, height, space.as_deref())?;
    CGContext::draw_image(
        Some(&context),
        CGRect::new(
            CGPoint::new(0.0, 0.0),
            CGSize::new(width as f64, height as f64),
        ),
        Some(image),
    );
    let radius = 18.0 * scale;
    let ring = CGRect::new(
        CGPoint::new(click.x - radius, height as f64 - click.y - radius),
        CGSize::new(radius * 2.0, radius * 2.0),
    );
    CGContext::set_rgb_fill_color(Some(&context), 1.0, 0.23, 0.19, 0.22);
    CGContext::fill_ellipse_in_rect(Some(&context), ring);
    CGContext::set_rgb_stroke_color(Some(&context), 1.0, 1.0, 1.0, 0.9);
    CGContext::set_line_width(Some(&context), 6.0 * scale);
    CGContext::stroke_ellipse_in_rect(Some(&context), ring);
    CGContext::set_rgb_stroke_color(Some(&context), 1.0, 0.23, 0.19, 1.0);
    CGContext::set_line_width(Some(&context), 3.0 * scale);
    CGContext::stroke_ellipse_in_rect(Some(&context), ring);
    objc2_core_graphics::CGBitmapContextCreateImage(Some(&context))
}

fn control_bar(mtm: MainThreadMarker) -> (Retained<NSPanel>, Retained<NSTextField>) {
    let visible = NSScreen::mainScreen(mtm).map_or(
        NSRect::new(NSPoint::ZERO, NSSize::new(1440.0, 900.0)),
        |s| s.visibleFrame(),
    );
    let origin = NSPoint::new(
        (visible.origin.x + (visible.size.width - BAR.width) / 2.0).round(),
        visible.origin.y + 24.0,
    );
    let panel = panels::floating_panel(mtm, NSRect::new(origin, BAR));
    panel.setHasShadow(true);
    // SAFETY: reading an AppKit constant.
    if let Some(dark) = NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }) {
        panel.setAppearance(Some(&dark));
    }
    let background = NSBox::new(mtm);
    background.setBoxType(NSBoxType::Custom);
    background.setTitlePosition(NSTitlePosition::NoTitle);
    background.setBorderWidth(0.0);
    background.setCornerRadius(12.0);
    background.setFillColor(&NSColor::colorWithWhite_alpha(0.1, 0.92));
    background.setFrame(NSRect::new(NSPoint::ZERO, BAR));
    panel.setContentView(Some(&background));

    let dot = NSTextField::labelWithString(&NSString::from_str("●"), mtm);
    dot.setTextColor(Some(&NSColor::systemRedColor()));
    dot.setFrame(NSRect::new(
        NSPoint::new(14.0, 13.0),
        NSSize::new(14.0, 18.0),
    ));
    background.addSubview(&dot);
    let status = NSTextField::labelWithString(&NSString::from_str(""), mtm);
    status.setTextColor(Some(&NSColor::whiteColor()));
    status.setFont(Some(&NSFont::systemFontOfSize(12.0)));
    status.setFrame(NSRect::new(
        NSPoint::new(32.0, 13.0),
        NSSize::new(290.0, 18.0),
    ));
    background.addSubview(&status);

    let target = BarTarget::shared(mtm);
    let target: &AnyObject = target.as_ref();
    let button = |title: &str, action, x: f64, w: f64| {
        // SAFETY: the action exists on BarTarget, which lives for the whole
        // process.
        let b = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str(title),
                Some(target),
                Some(action),
                mtm,
            )
        };
        b.setFrame(NSRect::new(NSPoint::new(x, 8.0), NSSize::new(w, 28.0)));
        background.addSubview(&b);
    };
    button(&t!("Done"), sel!(done:), 326.0, 64.0);
    button(&t!("Cancel"), sel!(cancel:), 394.0, 66.0);
    panel.orderFrontRegardless();
    let text = t!("Click through the task; each click is a step.");
    status.setStringValue(&NSString::from_str(&text));
    (panel, status)
}

define_class!(
    /// Target for the control bar's buttons.
    // SAFETY: NSObject has no subclassing requirements; no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    struct BarTarget;

    impl BarTarget {
        #[unsafe(method(done:))]
        fn done(&self, _sender: &AnyObject) {
            finish(self.mtm(), true);
        }

        #[unsafe(method(cancel:))]
        fn cancel(&self, _sender: &AnyObject) {
            finish(self.mtm(), false);
        }
    }
);

impl BarTarget {
    fn shared(mtm: MainThreadMarker) -> Retained<Self> {
        TARGET.with(|c| {
            c.borrow_mut()
                .get_or_insert_with(|| {
                    let this = Self::alloc(mtm).set_ivars(());
                    // SAFETY: NSObject's designated initializer.
                    unsafe { msg_send![super(this), init] }
                })
                .clone()
        })
    }
}
