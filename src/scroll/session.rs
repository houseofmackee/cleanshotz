//! A scrolling capture: select an area, scroll it (by hand or with
//! Auto-scroll) while frames are captured and stitched in the background,
//! then click Done.

use std::cell::RefCell;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBox, NSBoxType, NSButton, NSColor, NSFont, NSPanel, NSScreen, NSTextField, NSTitlePosition,
};
use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{CGEvent, CGEventTapLocation, CGScrollEventUnit};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString, NSTimer};

use super::stitch::{Frame, Push, Stitcher};
use crate::capture::flow::{overlay_displays, screen_for};
use crate::capture::geometry::Rect;
use crate::capture::output::{self, Captured};
use crate::capture::overlay::{self, Mode, Selection};
use crate::capture::sck::{self, CaptureError, CaptureOptions, Content};
use crate::capture::{display_bounds, image};
use crate::config::CaptureSettings;
use crate::platform::permissions::{Permission, Status};
use crate::ui::{panels, toast};

/// Tallest stitched image, in pixels.
const MAX_ROWS: usize = 30_000;
/// Time between captured frames.
const FRAME_INTERVAL: Duration = Duration::from_millis(80);
/// Auto-scroll: points per step and steps per second.
const AUTO_STEP: i32 = 40;
const AUTO_INTERVAL: f64 = 0.1;
/// Auto-scroll finishes when nothing new appears for this long.
const AUTO_IDLE: Duration = Duration::from_millis(2500);

const RUN: u8 = 0;
const DONE: u8 = 1;
const CANCEL: u8 = 2;

enum State {
    Idle,
    Selecting,
    Active(Box<Active>),
}

struct Active {
    signal: Arc<AtomicU8>,
    border: Retained<NSPanel>,
    controls: Retained<NSPanel>,
    status: Retained<NSTextField>,
    auto_button: Retained<NSButton>,
    auto_timer: Option<Retained<NSTimer>>,
    /// Where Auto-scroll sends scroll events (global CoreGraphics points).
    center: CGPoint,
    last_added: Instant,
    height: usize,
}

/// Progress reports from the capture thread.
enum Report {
    Progress(Push, usize),
    Finished(
        Option<objc2_core_foundation::CFRetained<objc2_core_graphics::CGImage>>,
        f64,
    ),
    Failed(String),
}

thread_local! {
    static STATE: RefCell<State> = const { RefCell::new(State::Idle) };
    static TARGET: RefCell<Option<Retained<ControlTarget>>> = const { RefCell::new(None) };
}

pub fn start(mtm: MainThreadMarker, settings: CaptureSettings) -> Result<(), CaptureError> {
    if STATE.with(|s| !matches!(*s.borrow(), State::Idle)) {
        return Ok(());
    }
    let content = Content::fetch()?;
    let displays = overlay_displays(mtm, &content, &settings, settings.show_magnifier)?;
    let options = overlay::Options {
        mode: Mode::Area,
        magnifier: settings.show_magnifier,
        freeze: false,
        windows: Vec::new(),
        click_selects_display: false,
        hint: Some("Select the area that scrolls · Esc to cancel".into()),
    };
    STATE.with(|s| *s.borrow_mut() = State::Selecting);
    overlay::begin(mtm, displays, options, move |selection| match selection {
        Some(Selection::Area {
            display_id, rect, ..
        }) => begin(mtm, display_id, rect, settings),
        _ => STATE.with(|s| *s.borrow_mut() = State::Idle),
    });
    Ok(())
}

fn begin(mtm: MainThreadMarker, display_id: u32, rect: Rect, settings: CaptureSettings) {
    let Some(screen) = screen_for(mtm, display_id) else {
        STATE.with(|s| *s.borrow_mut() = State::Idle);
        return;
    };
    let f = screen.frame();
    let area = NSRect::new(
        NSPoint::new(
            f.origin.x + rect.x,
            f.origin.y + f.size.height - rect.y - rect.height,
        ),
        NSSize::new(rect.width, rect.height),
    );
    let bounds = display_bounds(display_id);
    let center = CGPoint::new(
        bounds.x + rect.x + rect.width / 2.0,
        bounds.y + rect.y + rect.height / 2.0,
    );

    let border = panels::border_panel(mtm, area);
    let (controls, status, auto_button) = control_bar(mtm, area, &screen);
    let signal = Arc::new(AtomicU8::new(RUN));
    STATE.with(|s| {
        *s.borrow_mut() = State::Active(Box::new(Active {
            signal: signal.clone(),
            border,
            controls,
            status,
            auto_button,
            auto_timer: None,
            center,
            last_added: Instant::now(),
            height: 0,
        }))
    });
    std::thread::spawn(move || capture_loop(display_id, rect, signal, settings));
}

/// Captures and stitches frames until told to stop. Runs off the main
/// thread; reports to it through the main queue.
fn capture_loop(display_id: u32, rect: Rect, signal: Arc<AtomicU8>, settings: CaptureSettings) {
    let report = move |r: Report, settings: &CaptureSettings| {
        let settings = settings.clone();
        let r = super::AssertSend(r);
        DispatchQueue::main().exec_async(move || on_report(r.into_inner(), &settings));
    };
    let setup = || -> Result<_, CaptureError> {
        let content = Content::fetch()?;
        let display = content
            .display(display_id)
            .ok_or(CaptureError::DisplayGone)?;
        let filter = sck::display_filter(&display, &content.excluded_windows(false));
        let scale = sck::pixel_scale(&filter);
        Ok((filter, scale))
    };
    let (filter, scale) = match setup() {
        Ok(x) => x,
        Err(e) => return report(Report::Failed(e.to_string()), &settings),
    };
    let options = CaptureOptions {
        source_rect: Some(rect),
        width: (rect.width * scale).round() as usize,
        height: (rect.height * scale).round() as usize,
        show_cursor: false,
        shadow: false,
    };
    let mut stitcher = Stitcher::new(MAX_ROWS);
    while signal.load(Ordering::Relaxed) == RUN {
        let started = Instant::now();
        let frame = sck::capture(&filter, &options)
            .ok()
            .and_then(|img| image::to_pixels(&img))
            .map(|(width, height, pixels)| Frame {
                width,
                height,
                pixels,
            });
        if let Some(frame) = frame {
            let push = stitcher.push(frame);
            if push != Push::Unchanged {
                report(Report::Progress(push, stitcher.height()), &settings);
            }
        }
        std::thread::sleep(FRAME_INTERVAL.saturating_sub(started.elapsed()));
    }
    if signal.load(Ordering::Relaxed) == DONE {
        let image = image::from_pixels(stitcher.width(), stitcher.height(), &stitcher.pixels());
        report(Report::Finished(image, scale), &settings);
    }
}

fn on_report(report: Report, settings: &CaptureSettings) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    match report {
        Report::Progress(push, height) => {
            let auto_finish = STATE.with(|s| {
                let mut state = s.borrow_mut();
                let State::Active(active) = &mut *state else {
                    return false;
                };
                active.height = height;
                let message = match push {
                    Push::Full => "Size limit reached · click Done".to_string(),
                    Push::NoMatch => "Scrolled too fast · scroll back up a little".to_string(),
                    _ => format!("{height} px captured · keep scrolling, then click Done"),
                };
                if matches!(push, Push::Added(_) | Push::First) {
                    active.last_added = Instant::now();
                }
                active.status.setStringValue(&NSString::from_str(&message));
                active.auto_timer.is_some() && push == Push::Full
            });
            if auto_finish {
                finish(mtm, DONE);
            }
        }
        Report::Finished(image, scale) => match image {
            Some(image) => output::deliver(
                mtm,
                Captured {
                    image,
                    scale,
                    source: None,
                },
                settings,
            ),
            None => toast::show(mtm, "Nothing was captured"),
        },
        Report::Failed(e) => {
            tracing::error!(error = %e, "scrolling capture failed");
            finish(mtm, CANCEL);
            toast::show(mtm, "Scrolling capture failed");
        }
    }
}

/// Ends the session: `DONE` stitches and delivers the image, `CANCEL`
/// discards it.
fn finish(mtm: MainThreadMarker, how: u8) {
    let _ = mtm;
    let state = STATE.with(|s| std::mem::replace(&mut *s.borrow_mut(), State::Idle));
    if let State::Active(active) = state {
        active.signal.store(how, Ordering::Relaxed);
        if let Some(timer) = &active.auto_timer {
            timer.invalidate();
        }
        active.border.orderOut(None);
        active.controls.orderOut(None);
    }
}

fn toggle_auto(mtm: MainThreadMarker) {
    let running = STATE.with(|s| match &*s.borrow() {
        State::Active(active) => active.auto_timer.is_some(),
        _ => false,
    });
    if running {
        STATE.with(|s| {
            if let State::Active(active) = &mut *s.borrow_mut() {
                if let Some(timer) = active.auto_timer.take() {
                    timer.invalidate();
                }
                active
                    .auto_button
                    .setTitle(&NSString::from_str("Auto-scroll"));
            }
        });
        return;
    }
    // Sending scroll events to other apps needs Accessibility.
    if Permission::Accessibility.status() != Status::Granted {
        Permission::Accessibility.request();
        toast::show(mtm, "Allow Accessibility for CleanShotZ to use Auto-scroll");
        return;
    }
    let block = RcBlock::new(move |_timer: NonNull<NSTimer>| {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let (center, idle) = STATE.with(|s| match &*s.borrow() {
            State::Active(active) => (Some(active.center), active.last_added.elapsed()),
            _ => (None, Duration::ZERO),
        });
        let Some(center) = center else {
            return;
        };
        if idle > AUTO_IDLE {
            // Nothing new for a while: the end of the content.
            finish(mtm, DONE);
            return;
        }
        if let Some(event) =
            CGEvent::new_scroll_wheel_event2(None, CGScrollEventUnit::Pixel, 1, -AUTO_STEP, 0, 0)
        {
            CGEvent::set_location(Some(&event), center);
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
        }
    });
    // SAFETY: the block has the expected signature; the timer is
    // invalidated when Auto-scroll stops or the session ends.
    let timer = unsafe {
        NSTimer::scheduledTimerWithTimeInterval_repeats_block(AUTO_INTERVAL, true, &block)
    };
    STATE.with(|s| {
        if let State::Active(active) = &mut *s.borrow_mut() {
            active.last_added = Instant::now();
            active.auto_timer = Some(timer);
            active.auto_button.setTitle(&NSString::from_str("Stop"));
        }
    });
}

const BAR: NSSize = NSSize {
    width: 460.0,
    height: 44.0,
};

fn control_bar(
    mtm: MainThreadMarker,
    area: NSRect,
    screen: &NSScreen,
) -> (Retained<NSPanel>, Retained<NSTextField>, Retained<NSButton>) {
    let origin = panels::control_origin(area, screen.visibleFrame(), BAR);
    let panel = panels::floating_panel(mtm, NSRect::new(origin, BAR));
    panel.setHasShadow(true);
    let background = NSBox::new(mtm);
    background.setBoxType(NSBoxType::Custom);
    background.setTitlePosition(NSTitlePosition::NoTitle);
    background.setBorderWidth(0.0);
    background.setCornerRadius(12.0);
    background.setFillColor(&NSColor::colorWithWhite_alpha(0.1, 0.92));
    background.setFrame(NSRect::new(NSPoint::ZERO, BAR));
    panel.setContentView(Some(&background));

    let status = NSTextField::labelWithString(
        &NSString::from_str("Scroll down slowly, then click Done"),
        mtm,
    );
    status.setTextColor(Some(&NSColor::whiteColor()));
    status.setFont(Some(&NSFont::systemFontOfSize(12.0)));
    status.setFrame(NSRect::new(
        NSPoint::new(14.0, 13.0),
        NSSize::new(196.0, 18.0),
    ));
    background.addSubview(&status);

    let target = ControlTarget::shared(mtm);
    let target: &AnyObject = target.as_ref();
    let button = |title: &str, action, x: f64, w: f64| {
        // SAFETY: the action exists on ControlTarget, which lives for the
        // whole process.
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
        b
    };
    let auto = button("Auto-scroll", sel!(autoScroll:), 214.0, 96.0);
    button("Done", sel!(done:), 314.0, 64.0);
    button("Cancel", sel!(cancel:), 382.0, 66.0);
    panel.orderFrontRegardless();
    (panel, status, auto)
}

define_class!(
    /// Target for the control bar's buttons.
    // SAFETY: NSObject has no subclassing requirements; no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    struct ControlTarget;

    impl ControlTarget {
        #[unsafe(method(autoScroll:))]
        fn auto_scroll(&self, _sender: &AnyObject) {
            toggle_auto(self.mtm());
        }

        #[unsafe(method(done:))]
        fn done(&self, _sender: &AnyObject) {
            finish(self.mtm(), DONE);
        }

        #[unsafe(method(cancel:))]
        fn cancel(&self, _sender: &AnyObject) {
            finish(self.mtm(), CANCEL);
        }
    }
);

impl ControlTarget {
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
