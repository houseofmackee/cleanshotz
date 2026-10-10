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
use objc2::{AllocAnyThread, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBox, NSBoxType, NSButton, NSColor, NSFont, NSImage, NSImageAlignment, NSImageScaling,
    NSImageView, NSPanel, NSScreen, NSTextField, NSTitlePosition,
};
use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{CGEvent, CGEventTapLocation, CGScrollEventUnit};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString, NSTimer};

use super::stitch::{Axis, Frame, Push, Scroller};
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
/// How often the live preview is redrawn at most.
const PREVIEW_INTERVAL: Duration = Duration::from_millis(300);
/// Live preview panel size limits, in points.
const PREVIEW_WIDTH: f64 = 180.0;
/// Height of the preview strip once the content scrolls sideways.
const PREVIEW_STRIP: f64 = 110.0;
const PREVIEW_GAP: f64 = 12.0;

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
    /// The scroll direction, once the content has scrolled.
    axis: Option<Axis>,
    /// The captured area, in Cocoa screen coordinates.
    area: NSRect,
    preview: Option<(Retained<NSPanel>, Retained<NSImageView>)>,
}

/// Progress reports from the capture thread.
enum Report {
    /// The latest push, the length captured so far and the direction.
    Progress(Push, usize, Option<Axis>),
    /// A small image of everything so far.
    Preview(objc2_core_foundation::CFRetained<objc2_core_graphics::CGImage>),
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

/// Starts a scrolling capture. With `area` (a display id and a
/// display-local rect) it starts there without asking; `autoscroll` turns
/// on Auto-scroll right away.
pub fn start(
    mtm: MainThreadMarker,
    settings: CaptureSettings,
    area: Option<(u32, Rect)>,
    autoscroll: bool,
) -> Result<(), CaptureError> {
    if STATE.with(|s| !matches!(*s.borrow(), State::Idle)) {
        return Ok(());
    }
    if let Some((display_id, rect)) = area {
        begin(mtm, display_id, rect, settings);
        if autoscroll {
            toggle_auto(mtm);
        }
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
        }) => {
            begin(mtm, display_id, rect, settings);
            if autoscroll {
                toggle_auto(mtm);
            }
        }
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
    let preview = preview_panel(mtm, area, &screen);
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
            axis: None,
            area,
            preview,
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
    let mut stitcher = Scroller::new(MAX_ROWS);
    // In pixels: twice the panel's points covers Retina. Sideways, the
    // preview is a strip as wide as the area.
    let tall_preview = ((PREVIEW_WIDTH * 2.0) as usize, (rect.height * 2.0) as usize);
    let wide_preview = ((rect.width * 2.0) as usize, (PREVIEW_STRIP * 2.0) as usize);
    let mut last_preview: Option<Instant> = None;
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
                report(
                    Report::Progress(push, stitcher.length(), stitcher.axis()),
                    &settings,
                );
            }
            let due = last_preview.is_none_or(|t| t.elapsed() >= PREVIEW_INTERVAL);
            if matches!(push, Push::First | Push::Added(_)) && due {
                let (w, h) = match stitcher.axis() {
                    Some(Axis::Horizontal) => wide_preview,
                    _ => tall_preview,
                };
                let image = stitcher
                    .preview(w, h)
                    .and_then(|(w, h, pixels)| image::from_pixels(w, h, &pixels));
                if let Some(image) = image {
                    report(Report::Preview(image), &settings);
                }
                last_preview = Some(Instant::now());
            }
        }
        std::thread::sleep(FRAME_INTERVAL.saturating_sub(started.elapsed()));
    }
    if signal.load(Ordering::Relaxed) == DONE {
        let (width, height) = stitcher.size();
        let image = image::from_pixels(width, height, &stitcher.pixels());
        report(Report::Finished(image, scale), &settings);
    }
}

fn on_report(report: Report, settings: &CaptureSettings) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    match report {
        Report::Progress(push, length, axis) => {
            let auto_finish = STATE.with(|s| {
                let mut state = s.borrow_mut();
                let State::Active(active) = &mut *state else {
                    return false;
                };
                if axis == Some(Axis::Horizontal) && active.axis.is_none() {
                    place_strip_preview(active);
                }
                active.axis = axis;
                let message = match (push, axis) {
                    (Push::Full, _) => "Size limit reached · click Done".to_string(),
                    (Push::NoMatch, Some(Axis::Horizontal)) => {
                        "Scrolled too fast · scroll back a little".to_string()
                    }
                    (Push::NoMatch, _) => "Scrolled too fast · scroll back up a little".to_string(),
                    (_, Some(axis)) => {
                        let (dimension, direction) = match axis {
                            Axis::Vertical => ("tall", "scrolling"),
                            Axis::Horizontal => ("wide", "scrolling sideways"),
                        };
                        format!("{length} px {dimension} · keep {direction}, then click Done")
                    }
                    (_, None) => "Scroll down or sideways slowly, then click Done".to_string(),
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
        Report::Preview(image) => STATE.with(|s| {
            if let State::Active(active) = &*s.borrow()
                && let Some((_, view)) = &active.preview
            {
                let (w, h) = image::pixel_size(&image);
                // The preview pixels are twice the panel's points.
                let size = NSSize::new(w as f64 / 2.0, h as f64 / 2.0);
                let ns = NSImage::initWithCGImage_size(NSImage::alloc(), &image, size);
                view.setImage(Some(&ns));
            }
        }),
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
        if let Some((panel, _)) = &active.preview {
            panel.orderOut(None);
        }
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
        let (center, idle, axis) = STATE.with(|s| match &*s.borrow() {
            State::Active(active) => (
                Some(active.center),
                active.last_added.elapsed(),
                active.axis,
            ),
            _ => (None, Duration::ZERO, None),
        });
        let Some(center) = center else {
            return;
        };
        if idle > AUTO_IDLE {
            // Nothing new for a while: the end of the content.
            finish(mtm, DONE);
            return;
        }
        // Down, or right once the content has scrolled sideways.
        let (vertical, horizontal) = match axis {
            Some(Axis::Horizontal) => (0, -AUTO_STEP),
            _ => (-AUTO_STEP, 0),
        };
        if let Some(event) = CGEvent::new_scroll_wheel_event2(
            None,
            CGScrollEventUnit::Pixel,
            2,
            vertical,
            horizontal,
            0,
        ) {
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
    width: 580.0,
    height: 44.0,
};

/// A panel beside the area showing everything captured so far, or `None`
/// when there is no room on either side.
fn preview_panel(
    mtm: MainThreadMarker,
    area: NSRect,
    screen: &NSScreen,
) -> Option<(Retained<NSPanel>, Retained<NSImageView>)> {
    let visible = screen.visibleFrame();
    let right = area.origin.x + area.size.width + PREVIEW_GAP;
    let left = area.origin.x - PREVIEW_GAP - PREVIEW_WIDTH;
    let x = if right + PREVIEW_WIDTH <= visible.origin.x + visible.size.width {
        right
    } else if left >= visible.origin.x {
        left
    } else {
        return None;
    };
    let frame = NSRect::new(
        NSPoint::new(x, area.origin.y),
        NSSize::new(PREVIEW_WIDTH, area.size.height),
    );
    let panel = panels::floating_panel(mtm, frame);
    panel.setIgnoresMouseEvents(true);
    panel.setHasShadow(false);
    let view = NSImageView::initWithFrame(
        NSImageView::alloc(mtm),
        NSRect::new(NSPoint::ZERO, frame.size),
    );
    view.setImageScaling(NSImageScaling::ScaleProportionallyDown);
    // Top-aligned: the image grows downward (or sideways) as you scroll.
    view.setImageAlignment(NSImageAlignment::AlignTop);
    view.setWantsLayer(true);
    if let Some(layer) = view.layer() {
        layer.setShadowOpacity(0.35);
        layer.setShadowRadius(6.0);
    }
    panel.setContentView(Some(&view));
    panel.orderFrontRegardless();
    Some((panel, view))
}

/// Sideways, the preview becomes a strip above the area (or below the
/// control bar when the area reaches the top of the screen), left-aligned
/// so it grows to the right.
fn place_strip_preview(active: &Active) {
    let Some((panel, view)) = &active.preview else {
        return;
    };
    let area = active.area;
    let visible = panel.screen().map_or(area, |s| s.visibleFrame());
    let above = area.origin.y + area.size.height + PREVIEW_GAP;
    let y = if above + PREVIEW_STRIP <= visible.origin.y + visible.size.height {
        above
    } else {
        active.controls.frame().origin.y - PREVIEW_GAP - PREVIEW_STRIP
    };
    let frame = NSRect::new(
        NSPoint::new(area.origin.x, y),
        NSSize::new(area.size.width, PREVIEW_STRIP),
    );
    panel.setFrame_display(frame, true);
    view.setFrame(NSRect::new(NSPoint::ZERO, frame.size));
    view.setImageAlignment(NSImageAlignment::AlignLeft);
}

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
        &NSString::from_str("Scroll down or sideways slowly, then click Done"),
        mtm,
    );
    status.setTextColor(Some(&NSColor::whiteColor()));
    status.setFont(Some(&NSFont::systemFontOfSize(12.0)));
    status.setFrame(NSRect::new(
        NSPoint::new(14.0, 13.0),
        NSSize::new(316.0, 18.0),
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
    let auto = button("Auto-scroll", sel!(autoScroll:), 334.0, 96.0);
    button("Done", sel!(done:), 434.0, 64.0);
    button("Cancel", sel!(cancel:), 502.0, 66.0);
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
