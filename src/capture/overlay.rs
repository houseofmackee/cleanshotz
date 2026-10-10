//! The full-screen selection overlay used by area and window captures.
//!
//! One borderless window covers each display. All windows share one
//! [`Session`] (kept in a thread-local, since AppKit calls into the views),
//! so a selection can start on any display.
//!
//! Controls, following macOS and CleanShot X:
//! - drag to select an area; release to capture
//! - Shift: square; Option: grow from the center; hold Space while dragging
//!   to move the selection
//! - Space (not dragging): toggle area and window mode
//! - arrow keys: move the pointer by 1 point (10 with Shift)
//! - Escape or right click: cancel

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::{
    AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationOptions, NSBackingStoreType, NSBezierPath, NSColor,
    NSCompositingOperation, NSCursor, NSEvent, NSEventModifierFlags, NSGraphicsContext, NSImage,
    NSImageInterpolation, NSResponder, NSRunningApplication, NSScreen, NSScreenSaverWindowLevel,
    NSTrackingArea, NSTrackingAreaOptions, NSView, NSWindow, NSWindowCollectionBehavior,
    NSWindowStyleMask, NSWorkspace,
};
use objc2_core_foundation::{CFRetained, CGPoint};
use objc2_core_graphics::{CGImage, CGWarpMouseCursorPosition};
use objc2_foundation::{NSPoint, NSRect, NSSize};

use super::geometry::{self, Point, Rect};
use super::sck::WindowInfo;
use crate::ui::draw::{draw_label, inset, ns_rect, stroke_rect};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Area,
    Window,
}

/// A display to cover, with an optional snapshot of its current contents
/// (needed for the magnifier and for frozen-screen mode).
pub struct OverlayDisplay {
    pub id: u32,
    pub screen: Retained<NSScreen>,
    /// Global top-left of the display in CoreGraphics points.
    pub cg_origin: Point,
    pub scale: f64,
    pub snapshot: Option<CFRetained<CGImage>>,
}

pub struct Options {
    pub mode: Mode,
    pub magnifier: bool,
    /// Show the snapshot instead of the live screen.
    pub freeze: bool,
    /// Pickable windows, front to back.
    pub windows: Vec<WindowInfo>,
    /// A click without a drag selects the whole display it is on.
    pub click_selects_display: bool,
    /// Instructions shown at the top of the screen.
    pub hint: Option<String>,
}

pub enum Selection {
    Area {
        display_id: u32,
        /// Display-local points, top-left origin.
        rect: Rect,
        /// In frozen mode, the snapshot to crop and its pixels per point.
        frozen: Option<(CFRetained<CGImage>, f64)>,
    },
    Window {
        window_id: u32,
    },
}

type Completion = Box<dyn FnOnce(Option<Selection>)>;

struct Surface {
    display: OverlayDisplay,
    /// Size of the display in points; the view's (flipped) bounds.
    bounds: Rect,
    window: Retained<OverlayWindow>,
    view: Retained<OverlayView>,
    image: Option<Retained<NSImage>>,
}

struct Drag {
    surface: usize,
    anchor: Point,
    current: Point,
}

struct Session {
    surfaces: Vec<Surface>,
    mode: Mode,
    magnifier: bool,
    freeze: bool,
    windows: Vec<WindowInfo>,
    click_selects_display: bool,
    hint: Option<String>,
    cursor: Option<(usize, Point)>,
    drag: Option<Drag>,
    /// Last pointer position while Space is held during a drag.
    space_anchor: Option<Point>,
    shift: bool,
    option: bool,
    hovered: Option<usize>,
    on_done: Option<Completion>,
    previous_app: Option<Retained<NSRunningApplication>>,
}

enum Outcome {
    Continue,
    Done(Option<Selection>),
}

thread_local! {
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
    /// Windows of finished sessions. AppKit may still be dispatching an
    /// event to them when the session ends, so they are released when the
    /// next session starts instead.
    static RETIRED: RefCell<Vec<Surface>> = const { RefCell::new(Vec::new()) };
}

pub fn is_active() -> bool {
    SESSION.with(|s| s.borrow().is_some())
}

/// Covers every display in `displays` and calls `on_done` with the
/// selection, or `None` if the user cancelled.
pub fn begin(
    mtm: MainThreadMarker,
    displays: Vec<OverlayDisplay>,
    options: Options,
    on_done: impl FnOnce(Option<Selection>) + 'static,
) {
    RETIRED.with(|r| r.borrow_mut().clear());
    let previous_app = NSWorkspace::sharedWorkspace().frontmostApplication();

    let surfaces: Vec<Surface> = displays
        .into_iter()
        .enumerate()
        .map(|(index, display)| make_surface(mtm, index, display, options.freeze))
        .collect();

    let mut session = Session {
        surfaces,
        mode: options.mode,
        magnifier: options.magnifier,
        freeze: options.freeze,
        windows: options.windows,
        click_selects_display: options.click_selects_display,
        hint: options.hint,
        cursor: None,
        drag: None,
        space_anchor: None,
        shift: false,
        option: false,
        hovered: None,
        on_done: Some(Box::new(on_done)),
        previous_app,
    };
    session.cursor = session.locate_mouse();
    if let Some((surface, p)) = session.cursor {
        session.hovered = session.window_at(surface, p);
    }

    let app = NSApplication::sharedApplication(mtm);
    app.activate();
    let key = session.cursor.map_or(0, |(s, _)| s);
    for (i, surface) in session.surfaces.iter().enumerate() {
        if i == key {
            surface.window.makeKeyAndOrderFront(None);
        } else {
            surface.window.orderFrontRegardless();
        }
        surface.window.makeFirstResponder(Some(&surface.view));
    }
    NSCursor::crosshairCursor().set();
    let instructions = session.hint.clone().unwrap_or_else(|| {
        match session.mode {
            Mode::Window => {
                "Move the pointer over a window and click to capture it. Escape cancels."
            }
            _ => "Drag to select an area. Space picks a window instead. Escape cancels.",
        }
        .to_string()
    });
    for surface in &session.surfaces {
        crate::ui::access::describe(
            &surface.view,
            // SAFETY: reading an AppKit constant.
            unsafe { objc2_app_kit::NSAccessibilityGroupRole },
            "Screen selection",
            Some(&instructions),
        );
    }
    SESSION.with(|s| *s.borrow_mut() = Some(session));
    crate::ui::access::announce(mtm, &instructions);
}

fn make_surface(
    mtm: MainThreadMarker,
    index: usize,
    display: OverlayDisplay,
    freeze: bool,
) -> Surface {
    let frame = display.screen.frame();
    let bounds = Rect::new(0.0, 0.0, frame.size.width, frame.size.height);
    // SAFETY: designated NSWindow initializer on our subclass.
    let window: Retained<OverlayWindow> = unsafe {
        msg_send![
            OverlayWindow::alloc(mtm),
            initWithContentRect: frame,
            styleMask: NSWindowStyleMask::Borderless,
            backing: NSBackingStoreType::Buffered,
            defer: false
        ]
    };
    // SAFETY: we keep our own strong reference to the window.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setFrame_display(frame, false);
    window.setLevel(NSScreenSaverWindowLevel);
    window.setOpaque(freeze);
    window.setBackgroundColor(Some(&NSColor::clearColor()));
    window.setHasShadow(false);
    window.setAcceptsMouseMovedEvents(true);
    window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );

    let view = OverlayView::new(mtm, index, NSRect::new(NSPoint::ZERO, frame.size));
    window.setContentView(Some(&view));
    // Mouse-moved events for every display, not just the key window.
    // SAFETY: the view owns the tracking area and outlives it.
    let tracking = unsafe {
        NSTrackingArea::initWithRect_options_owner_userInfo(
            NSTrackingArea::alloc(),
            NSRect::ZERO,
            NSTrackingAreaOptions::MouseMoved
                | NSTrackingAreaOptions::MouseEnteredAndExited
                | NSTrackingAreaOptions::ActiveAlways
                | NSTrackingAreaOptions::InVisibleRect,
            Some(&view),
            None,
        )
    };
    view.addTrackingArea(&tracking);

    let image = display
        .snapshot
        .as_ref()
        .map(|snapshot| NSImage::initWithCGImage_size(NSImage::alloc(), snapshot, frame.size));
    Surface {
        display,
        bounds,
        window,
        view,
        image,
    }
}

/// Runs `f` on the active session, then redraws or finishes.
fn dispatch(f: impl FnOnce(&mut Session) -> Outcome) {
    let outcome = SESSION.with(|cell| match cell.borrow_mut().as_mut() {
        Some(session) => f(session),
        None => Outcome::Continue,
    });
    match outcome {
        Outcome::Continue => SESSION.with(|cell| {
            if let Some(session) = cell.borrow().as_ref() {
                for surface in &session.surfaces {
                    surface.view.setNeedsDisplay(true);
                }
            }
        }),
        Outcome::Done(selection) => finish(selection),
    }
}

fn finish(selection: Option<Selection>) {
    let Some(mut session) = SESSION.with(|cell| cell.borrow_mut().take()) else {
        return;
    };
    for surface in &session.surfaces {
        surface.window.orderOut(None);
    }
    NSCursor::arrowCursor().set();
    let on_done = session.on_done.take();
    let previous_app = session.previous_app.take();
    RETIRED.with(|r| r.borrow_mut().extend(session.surfaces));
    if let Some(on_done) = on_done {
        on_done(selection);
    }
    // Give focus back to the app that was active before the overlay.
    if let Some(app) = previous_app {
        app.activateWithOptions(NSApplicationActivationOptions::empty());
    }
}

impl Session {
    fn locate_mouse(&self) -> Option<(usize, Point)> {
        let mouse = NSEvent::mouseLocation();
        self.surfaces.iter().enumerate().find_map(|(i, s)| {
            let f = s.display.screen.frame();
            let p = Point::new(mouse.x - f.origin.x, f.origin.y + f.size.height - mouse.y);
            s.bounds.contains(p).then_some((i, p))
        })
    }

    fn window_at(&self, surface: usize, p: Point) -> Option<usize> {
        let o = self.surfaces[surface].display.cg_origin;
        let global = Point::new(o.x + p.x, o.y + p.y);
        self.windows.iter().position(|w| w.frame.contains(global))
    }

    fn window_rect_on(&self, window: usize, surface: usize) -> Rect {
        let s = &self.surfaces[surface];
        self.windows[window]
            .frame
            .offset(-s.display.cg_origin.x, -s.display.cg_origin.y)
            .intersection(&s.bounds)
    }

    fn selection(&self) -> Option<(usize, Rect)> {
        let drag = self.drag.as_ref()?;
        let bounds = self.surfaces[drag.surface].bounds;
        let rect =
            geometry::selection_rect(drag.anchor, drag.current, self.shift, self.option, bounds);
        Some((drag.surface, rect))
    }

    fn pointer_moved(&mut self, surface: usize, p: Point) {
        self.cursor = Some((surface, p));
        if let Some(last) = self.space_anchor {
            // Space held: move the whole selection.
            if let Some((s, rect)) = self.selection() {
                let bounds = self.surfaces[s].bounds;
                let moved = geometry::move_within(rect, p.x - last.x, p.y - last.y, bounds);
                let (dx, dy) = (moved.x - rect.x, moved.y - rect.y);
                if let Some(drag) = self.drag.as_mut() {
                    drag.anchor = Point::new(drag.anchor.x + dx, drag.anchor.y + dy);
                    drag.current = Point::new(drag.current.x + dx, drag.current.y + dy);
                }
            }
            self.space_anchor = Some(p);
        } else if let Some(drag) = self.drag.as_mut()
            && drag.surface == surface
        {
            drag.current = p;
        }
        if self.mode == Mode::Window {
            self.hovered = self.window_at(surface, p);
        }
    }

    fn mouse_down(&mut self, surface: usize, p: Point) -> Outcome {
        self.cursor = Some((surface, p));
        match self.mode {
            Mode::Area => {
                self.drag = Some(Drag {
                    surface,
                    anchor: p,
                    current: p,
                });
                Outcome::Continue
            }
            Mode::Window => match self.window_at(surface, p) {
                Some(w) => Outcome::Done(Some(Selection::Window {
                    window_id: self.windows[w].id,
                })),
                None => Outcome::Continue,
            },
        }
    }

    fn mouse_up(&mut self, surface: usize, p: Point) -> Outcome {
        self.pointer_moved(surface, p);
        let selection = self.selection();
        self.drag = None;
        self.space_anchor = None;
        match selection {
            Some((s, rect)) if rect.is_usable() => {
                let display = &self.surfaces[s].display;
                let frozen = if self.freeze {
                    display.snapshot.clone().map(|img| (img, display.scale))
                } else {
                    None
                };
                Outcome::Done(Some(Selection::Area {
                    display_id: display.id,
                    rect,
                    frozen,
                }))
            }
            // A click without a drag selects the display, or keeps selecting.
            _ if self.click_selects_display => {
                let display = &self.surfaces[surface].display;
                Outcome::Done(Some(Selection::Area {
                    display_id: display.id,
                    rect: self.surfaces[surface].bounds,
                    frozen: None,
                }))
            }
            _ => Outcome::Continue,
        }
    }

    fn key_down(&mut self, key_code: u16, shift: bool) -> Outcome {
        const ESCAPE: u16 = 53;
        const SPACE: u16 = 49;
        const LEFT: u16 = 123;
        const RIGHT: u16 = 124;
        const DOWN: u16 = 125;
        const UP: u16 = 126;
        match key_code {
            ESCAPE => return Outcome::Done(None),
            SPACE if self.drag.is_some() => {
                if self.space_anchor.is_none() {
                    self.space_anchor = self.cursor.map(|(_, p)| p);
                }
            }
            SPACE if !self.windows.is_empty() => {
                self.mode = match self.mode {
                    Mode::Area => Mode::Window,
                    Mode::Window => Mode::Area,
                };
                self.hovered = self.cursor.and_then(|(s, p)| self.window_at(s, p));
            }
            LEFT | RIGHT | UP | DOWN => {
                let step = if shift { 10.0 } else { 1.0 };
                let (dx, dy) = match key_code {
                    LEFT => (-step, 0.0),
                    RIGHT => (step, 0.0),
                    UP => (0.0, -step),
                    _ => (0.0, step),
                };
                if let Some((s, p)) = self.cursor {
                    let b = self.surfaces[s].bounds;
                    let moved = Point::new(
                        (p.x + dx).clamp(b.x, b.max_x() - 1.0),
                        (p.y + dy).clamp(b.y, b.max_y() - 1.0),
                    );
                    let o = self.surfaces[s].display.cg_origin;
                    let _ = CGWarpMouseCursorPosition(CGPoint::new(o.x + moved.x, o.y + moved.y));
                    self.pointer_moved(s, moved);
                }
            }
            _ => {}
        }
        Outcome::Continue
    }

    fn draw(&self, index: usize) {
        let surface = &self.surfaces[index];
        let bounds = surface.bounds;

        if self.freeze
            && let Some(image) = &surface.image
        {
            // SAFETY: drawing inside the view's drawRect:.
            unsafe {
                image.drawInRect_fromRect_operation_fraction_respectFlipped_hints(
                    ns_rect(bounds),
                    NSRect::ZERO,
                    NSCompositingOperation::Copy,
                    1.0,
                    true,
                    None,
                );
            }
        }

        let hole = match self.mode {
            Mode::Area => self
                .selection()
                .filter(|(s, _)| *s == index)
                .map(|(_, r)| r),
            Mode::Window => self.hovered.map(|w| self.window_rect_on(w, index)),
        };
        fill_except(bounds, hole, &NSColor::colorWithWhite_alpha(0.0, 0.35));

        if let Some(hole) = hole.filter(|h| h.width > 0.0 && h.height > 0.0) {
            match self.mode {
                Mode::Area => {
                    NSColor::whiteColor().setStroke();
                    stroke_rect(inset(hole, -0.5), 1.0);
                    let label = format!("{} × {}", hole.width, hole.height);
                    draw_label(&label, label_origin(hole, bounds), 12.0);
                }
                Mode::Window => {
                    NSColor::colorWithSRGBRed_green_blue_alpha(0.2, 0.5, 1.0, 0.25).setFill();
                    NSBezierPath::fillRect(ns_rect(hole));
                    NSColor::colorWithSRGBRed_green_blue_alpha(0.2, 0.5, 1.0, 0.9).setStroke();
                    stroke_rect(inset(hole, 1.0), 2.0);
                    if let Some(w) = self.hovered {
                        let info = &self.windows[w];
                        let name = if info.title.is_empty() {
                            info.app_name.clone()
                        } else {
                            format!("{}: {}", info.app_name, info.title)
                        };
                        let label = format!("{name}  {} × {}", info.frame.width, info.frame.height);
                        draw_label(&label, label_origin(hole, bounds), 12.0);
                    }
                }
            }
        }

        if let Some(hint) = &self.hint
            && self.cursor.map_or(index == 0, |(s, _)| s == index)
            && self.drag.is_none()
        {
            let size = crate::ui::draw::text_size(hint, 13.0, crate::ui::draw::Weight::Medium);
            let x = ((bounds.width - size.width) / 2.0 - 6.0).round();
            draw_label(hint, Point::new(x, 48.0), 13.0);
        }

        if let Some((s, p)) = self.cursor
            && s == index
            && self.mode == Mode::Area
        {
            if self.drag.is_none() {
                draw_crosshair(bounds, p);
            }
            if self.magnifier
                && let Some(image) = &surface.image
            {
                draw_magnifier(image, surface.display.scale, bounds, p);
            }
        }
    }
}

const MAGNIFIER_PIXELS: f64 = 15.0;
const MAGNIFIER_SIZE: f64 = 135.0;

/// A zoomed view of the pixels around the pointer, with a pixel grid and
/// the pointer's pixel outlined.
fn draw_magnifier(image: &NSImage, scale: f64, bounds: Rect, p: Point) {
    let origin = geometry::panel_origin(p, MAGNIFIER_SIZE, 24.0, bounds);
    let dest = Rect::new(origin.x, origin.y, MAGNIFIER_SIZE, MAGNIFIER_SIZE);
    let cell = MAGNIFIER_SIZE / MAGNIFIER_PIXELS;
    let half = (MAGNIFIER_PIXELS / 2.0).floor();

    // Source in image points with a bottom-left origin (NSImage space),
    // aligned to whole pixels.
    let px = (p.x * scale).floor() - half;
    let py = (p.y * scale).floor() - half;
    let side = MAGNIFIER_PIXELS / scale;
    let source = NSRect::new(
        NSPoint::new(px / scale, bounds.height - (py / scale + side)),
        NSSize::new(side, side),
    );

    let Some(context) = NSGraphicsContext::currentContext() else {
        return;
    };
    context.saveGraphicsState();
    let clip = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(ns_rect(dest), 8.0, 8.0);
    clip.addClip();
    NSColor::blackColor().setFill();
    NSBezierPath::fillRect(ns_rect(dest));
    context.setImageInterpolation(NSImageInterpolation::None);
    // SAFETY: drawing inside the view's drawRect:.
    unsafe {
        image.drawInRect_fromRect_operation_fraction_respectFlipped_hints(
            ns_rect(dest),
            source,
            NSCompositingOperation::Copy,
            1.0,
            true,
            None,
        );
    }
    NSColor::colorWithWhite_alpha(1.0, 0.12).setFill();
    for i in 1..MAGNIFIER_PIXELS as usize {
        let offset = i as f64 * cell;
        NSBezierPath::fillRect(ns_rect(Rect::new(
            dest.x + offset,
            dest.y,
            0.5,
            dest.height,
        )));
        NSBezierPath::fillRect(ns_rect(Rect::new(dest.x, dest.y + offset, dest.width, 0.5)));
    }
    let center = Rect::new(dest.x + half * cell, dest.y + half * cell, cell, cell);
    NSColor::blackColor().setStroke();
    stroke_rect(inset(center, -0.5), 1.0);
    NSColor::whiteColor().setStroke();
    stroke_rect(inset(center, 0.5), 1.0);
    context.restoreGraphicsState();

    NSColor::colorWithWhite_alpha(1.0, 0.8).setStroke();
    let border = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
        ns_rect(inset(dest, 0.5)),
        8.0,
        8.0,
    );
    border.setLineWidth(1.0);
    border.stroke();

    let label = format!("{}, {}", p.x.floor(), p.y.floor());
    let below = Point::new(dest.x, dest.max_y() + 4.0);
    let above = Point::new(dest.x, dest.y - 24.0);
    let at = if below.y + 20.0 <= bounds.max_y() {
        below
    } else {
        above
    };
    draw_label(&label, at, 11.0);
}

fn draw_crosshair(bounds: Rect, p: Point) {
    let x = p.x.floor() + 0.5;
    let y = p.y.floor() + 0.5;
    NSColor::colorWithWhite_alpha(1.0, 0.5).setFill();
    NSBezierPath::fillRect(ns_rect(Rect::new(x - 0.5, bounds.y, 1.0, bounds.height)));
    NSBezierPath::fillRect(ns_rect(Rect::new(bounds.x, y - 0.5, bounds.width, 1.0)));
}

/// Fills `bounds` with `color`, leaving `hole` clear.
fn fill_except(bounds: Rect, hole: Option<Rect>, color: &NSColor) {
    color.setFill();
    let Some(h) = hole.filter(|h| h.width > 0.0 && h.height > 0.0) else {
        NSBezierPath::fillRect(ns_rect(bounds));
        return;
    };
    for r in [
        Rect::new(bounds.x, bounds.y, bounds.width, h.y - bounds.y),
        Rect::new(
            bounds.x,
            h.max_y(),
            bounds.width,
            bounds.max_y() - h.max_y(),
        ),
        Rect::new(bounds.x, h.y, h.x - bounds.x, h.height),
        Rect::new(h.max_x(), h.y, bounds.max_x() - h.max_x(), h.height),
    ] {
        if r.width > 0.0 && r.height > 0.0 {
            NSBezierPath::fillRect(ns_rect(r));
        }
    }
}

/// Below the rect if there is room, otherwise inside its top edge.
fn label_origin(rect: Rect, bounds: Rect) -> Point {
    let x = rect.x.max(bounds.x + 4.0);
    if rect.max_y() + 28.0 <= bounds.max_y() {
        Point::new(x, rect.max_y() + 6.0)
    } else {
        Point::new(x + 6.0, rect.y + 6.0)
    }
}

define_class!(
    /// Borderless windows cannot become key by default; overlays must, to
    /// receive key events.
    // SAFETY: NSWindow can be subclassed; no Drop implementation.
    #[unsafe(super(NSWindow, NSResponder, objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    struct OverlayWindow;

    impl OverlayWindow {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool {
            true
        }
    }
);

define_class!(
    // SAFETY: NSView can be subclassed; no Drop implementation.
    #[unsafe(super(NSView, NSResponder, objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = usize]
    struct OverlayView;

    impl OverlayView {
        /// Top-left origin, matching ScreenCaptureKit's coordinates.
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let index = *self.ivars();
            SESSION.with(|cell| {
                if let Ok(session) = cell.try_borrow()
                    && let Some(session) = session.as_ref()
                {
                    session.draw(index);
                }
            });
        }

        #[unsafe(method(resetCursorRects))]
        fn reset_cursor_rects(&self) {
            self.addCursorRect_cursor(self.bounds(), &NSCursor::crosshairCursor());
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            let (index, p) = (*self.ivars(), self.point(event));
            NSCursor::crosshairCursor().set();
            dispatch(|s| {
                s.pointer_moved(index, p);
                Outcome::Continue
            });
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            let index = *self.ivars();
            dispatch(|s| {
                if s.cursor.is_some_and(|(i, _)| i == index) && s.drag.is_none() {
                    s.cursor = None;
                }
                Outcome::Continue
            });
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let (index, p) = (*self.ivars(), self.point(event));
            dispatch(|s| s.mouse_down(index, p));
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let (index, p) = (*self.ivars(), self.point(event));
            dispatch(|s| {
                s.pointer_moved(index, p);
                Outcome::Continue
            });
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            let (index, p) = (*self.ivars(), self.point(event));
            dispatch(|s| s.mouse_up(index, p));
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, _event: &NSEvent) {
            dispatch(|_| Outcome::Done(None));
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let shift = event.modifierFlags().contains(NSEventModifierFlags::Shift);
            let key_code = event.keyCode();
            dispatch(|s| s.key_down(key_code, shift));
        }

        #[unsafe(method(keyUp:))]
        fn key_up(&self, event: &NSEvent) {
            const SPACE: u16 = 49;
            if event.keyCode() == SPACE {
                dispatch(|s| {
                    s.space_anchor = None;
                    Outcome::Continue
                });
            }
        }

        #[unsafe(method(flagsChanged:))]
        fn flags_changed(&self, event: &NSEvent) {
            let flags = event.modifierFlags();
            dispatch(|s| {
                s.shift = flags.contains(NSEventModifierFlags::Shift);
                s.option = flags.contains(NSEventModifierFlags::Option);
                Outcome::Continue
            });
        }
    }
);

impl OverlayView {
    fn new(mtm: MainThreadMarker, index: usize, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(index);
        // SAFETY: NSView's designated initializer.
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    /// Event location in this (flipped) view's coordinates.
    fn point(&self, event: &NSEvent) -> Point {
        let p = self.convertPoint_fromView(event.locationInWindow(), None);
        Point::new(p.x, p.y)
    }
}
