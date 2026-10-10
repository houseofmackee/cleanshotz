//! Pinned screenshots: captures floating above all windows.
//!
//! A pin opens where the capture was taken. Drag it to move it, or drag a
//! corner to resize it; pinch, Cmd+= and Cmd+- zoom; Cmd+0 returns to
//! actual size; Cmd+C copies; Escape or Cmd+W closes. Right click for
//! opacity, lock (clicks pass through to the windows below), Arrange,
//! Save As and more. Locked pins are unlocked from the menu bar.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{
    AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel,
};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSColor, NSCompositingOperation, NSControlStateValueOff,
    NSControlStateValueOn, NSEvent, NSEventModifierFlags, NSFloatingWindowLevel, NSGraphicsContext,
    NSImage, NSImageInterpolation, NSMenu, NSMenuItem, NSModalResponseOK, NSPanel, NSResponder,
    NSSavePanel, NSScreen, NSTrackingArea, NSTrackingAreaOptions, NSView, NSWindow,
    NSWindowCollectionBehavior, NSWindowStyleMask, NSWorkspace,
};
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::CGImage;
use objc2_foundation::{NSArray, NSPoint, NSRect, NSSize, NSString, NSURL, ns_string};

use crate::capture::geometry::{Point, Rect};
use crate::capture::{output, screen_under_mouse};
use crate::config::ImageFormat;
use crate::quick_access::Item;
use crate::ui::draw;

const MIN_ZOOM: f64 = 0.1;
const MAX_ZOOM: f64 = 8.0;

struct Pin {
    id: u64,
    panel: Retained<PinPanel>,
    image: CFRetained<CGImage>,
    ns_image: Retained<NSImage>,
    file: PathBuf,
    saved: bool,
    png: Option<Vec<u8>>,
    /// Size at 100% zoom, in points.
    base: NSSize,
    zoom: Cell<f64>,
    locked: Cell<bool>,
}

#[derive(Default)]
struct Pins {
    pins: Vec<Rc<Pin>>,
    next_id: u64,
}

thread_local! {
    static PINS: RefCell<Pins> = RefCell::new(Pins::default());
    static RETIRED: RefCell<Vec<Retained<PinPanel>>> = const { RefCell::new(Vec::new()) };
}

/// Pins a Quick Access item where it was captured, or in the middle of the
/// screen under the pointer.
pub fn pin(mtm: MainThreadMarker, item: &Item) {
    RETIRED.with(|r| r.borrow_mut().clear());
    let base = item.point_size();
    let frame = initial_frame(mtm, base, item.source);
    let id = PINS.with(|p| {
        let mut pins = p.borrow_mut();
        pins.next_id += 1;
        pins.next_id
    });

    // SAFETY: designated NSWindow initializer on our subclass.
    let panel: Retained<PinPanel> = unsafe {
        msg_send![
            PinPanel::alloc(mtm),
            initWithContentRect: frame,
            styleMask: NSWindowStyleMask::Borderless
                | NSWindowStyleMask::Resizable
                | NSWindowStyleMask::NonactivatingPanel,
            backing: NSBackingStoreType::Buffered,
            defer: false
        ]
    };
    // SAFETY: the pin list keeps a strong reference to the panel.
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setLevel(NSFloatingWindowLevel);
    panel.setHasShadow(true);
    panel.setOpaque(false);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    panel.setHidesOnDeactivate(false);
    panel.setContentAspectRatio(base);
    panel.setContentMinSize(NSSize::new(24.0, 24.0));
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );

    let view = PinView::new(mtm, id, NSRect::new(NSPoint::ZERO, frame.size));
    panel.setContentView(Some(&view));
    // SAFETY: the view owns the tracking area.
    let tracking = unsafe {
        NSTrackingArea::initWithRect_options_owner_userInfo(
            NSTrackingArea::alloc(),
            NSRect::ZERO,
            NSTrackingAreaOptions::MouseEnteredAndExited
                | NSTrackingAreaOptions::ActiveAlways
                | NSTrackingAreaOptions::InVisibleRect,
            Some(&view),
            None,
        )
    };
    view.addTrackingArea(&tracking);

    let pin = Rc::new(Pin {
        id,
        panel: panel.clone(),
        image: item.image(),
        ns_image: item.ns_image(),
        file: item.file(),
        saved: item.is_saved(),
        png: item.png(),
        base,
        zoom: Cell::new(1.0),
        locked: Cell::new(false),
    });
    PINS.with(|p| p.borrow_mut().pins.push(pin));
    panel.makeKeyAndOrderFront(None);
}

pub fn count() -> usize {
    PINS.with(|p| p.borrow().pins.len())
}

pub fn any_locked() -> bool {
    PINS.with(|p| p.borrow().pins.iter().any(|pin| pin.locked.get()))
}

pub fn unlock_all() {
    PINS.with(|p| {
        for pin in &p.borrow().pins {
            set_locked(pin, false);
        }
    });
}

pub fn close_all() {
    let ids: Vec<u64> = PINS.with(|p| p.borrow().pins.iter().map(|pin| pin.id).collect());
    for id in ids {
        close(id);
    }
}

/// Centered on where the capture was (`source`, global CoreGraphics
/// points), or on the screen under the pointer.
fn initial_frame(mtm: MainThreadMarker, size: NSSize, source: Option<Rect>) -> NSRect {
    let screens = NSScreen::screens(mtm);
    let center = match (source, screens.firstObject()) {
        // CoreGraphics y grows down from the top of the primary screen;
        // Cocoa y grows up from its bottom.
        (Some(s), Some(primary)) => NSPoint::new(
            s.x + s.width / 2.0,
            primary.frame().size.height - (s.y + s.height / 2.0),
        ),
        _ => {
            let f = screen_under_mouse(mtm).map_or(NSRect::ZERO, |s| s.visibleFrame());
            NSPoint::new(
                f.origin.x + f.size.width / 2.0,
                f.origin.y + f.size.height / 2.0,
            )
        }
    };
    NSRect::new(
        NSPoint::new(
            (center.x - size.width / 2.0).round(),
            (center.y - size.height / 2.0).round(),
        ),
        size,
    )
}

/// Space around and between arranged pins, in points.
const ARRANGE_MARGIN: f64 = 16.0;
const ARRANGE_GAP: f64 = 12.0;

/// Lays out `sizes` in rows, left to right and top to bottom, inside
/// `area` (top-left origin). If they do not fit at their sizes, all are
/// scaled by the same factor, as large as fits. Returns one rect per size,
/// in order, and the factor.
pub fn arrange_layout(sizes: &[(f64, f64)], area: Rect, gap: f64) -> (Vec<Rect>, f64) {
    let place = |factor: f64| -> Option<Vec<Rect>> {
        let (mut x, mut y, mut row_height) = (area.x, area.y, 0.0f64);
        let mut rects = Vec::with_capacity(sizes.len());
        for &(w, h) in sizes {
            let (w, h) = (w * factor, h * factor);
            if w > area.width || h > area.height {
                return None;
            }
            if x > area.x && x + w > area.max_x() {
                // Next row.
                x = area.x;
                y += row_height + gap;
                row_height = 0.0;
            }
            if y + h > area.max_y() {
                return None;
            }
            rects.push(Rect::new(x, y, w, h));
            x += w + gap;
            row_height = row_height.max(h);
        }
        Some(rects)
    };
    if let Some(rects) = place(1.0) {
        return (rects, 1.0);
    }
    // The largest factor that fits, to within half a percent.
    let (mut low, mut high) = (0.0, 1.0);
    let mut best = None;
    while high - low > 0.005 {
        let mid = (low + high) / 2.0;
        match place(mid) {
            Some(rects) => {
                best = Some((rects, mid));
                low = mid;
            }
            None => high = mid,
        }
    }
    best.unwrap_or_else(|| (place(0.01).unwrap_or_default(), 0.01))
}

/// Tiles all pins on the screen under the pointer, in the order they were
/// pinned, scaling them down together if they do not fit.
pub fn arrange(mtm: MainThreadMarker) {
    let pins: Vec<Rc<Pin>> = PINS.with(|p| p.borrow().pins.clone());
    if pins.is_empty() {
        return;
    }
    let Some(screen) = screen_under_mouse(mtm) else {
        return;
    };
    let v = screen.visibleFrame();
    // Lay out with a top-left origin, then flip into Cocoa coordinates.
    let area = Rect::new(
        0.0,
        0.0,
        v.size.width - 2.0 * ARRANGE_MARGIN,
        v.size.height - 2.0 * ARRANGE_MARGIN,
    );
    let sizes: Vec<(f64, f64)> = pins
        .iter()
        .map(|pin| {
            let f = pin.panel.frame();
            (f.size.width, f.size.height)
        })
        .collect();
    let (rects, _) = arrange_layout(&sizes, area, ARRANGE_GAP);
    for (pin, r) in pins.iter().zip(rects) {
        let frame = NSRect::new(
            NSPoint::new(
                (v.origin.x + ARRANGE_MARGIN + r.x).round(),
                (v.origin.y + v.size.height - ARRANGE_MARGIN - r.y - r.height).round(),
            ),
            NSSize::new(r.width.round(), r.height.round()),
        );
        pin.zoom.set(frame.size.width / pin.base.width);
        pin.panel.setFrame_display_animate(frame, true, true);
    }
}

fn find(id: u64) -> Option<Rc<Pin>> {
    PINS.with(|p| p.borrow().pins.iter().find(|pin| pin.id == id).cloned())
}

fn close(id: u64) {
    let Some(pin) = PINS.with(|p| {
        let mut pins = p.borrow_mut();
        let index = pins.pins.iter().position(|pin| pin.id == id)?;
        Some(pins.pins.remove(index))
    }) else {
        return;
    };
    pin.panel.orderOut(None);
    RETIRED.with(|r| r.borrow_mut().push(pin.panel.clone()));
}

fn set_locked(pin: &Pin, locked: bool) {
    pin.locked.set(locked);
    pin.panel.setIgnoresMouseEvents(locked);
}

/// Resizes around the window's center.
fn set_zoom(pin: &Pin, zoom: f64) {
    let zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
    pin.zoom.set(zoom);
    let frame = pin.panel.frame();
    let size = NSSize::new(
        (pin.base.width * zoom).round(),
        (pin.base.height * zoom).round(),
    );
    let origin = NSPoint::new(
        frame.origin.x + (frame.size.width - size.width) / 2.0,
        frame.origin.y + (frame.size.height - size.height) / 2.0,
    );
    pin.panel.setFrame_display(NSRect::new(origin, size), true);
}

fn copy(pin: &Pin) {
    let png = pin
        .png
        .clone()
        .or_else(|| crate::capture::image::encode(&pin.image, ImageFormat::Png, 1.0));
    match png {
        Some(png) => output::copy_png(&png),
        None => tracing::error!("could not encode pinned screenshot for the clipboard"),
    }
}

fn save_as(mtm: MainThreadMarker, pin: &Pin) {
    let panel = NSSavePanel::savePanel(mtm);
    let name = pin
        .file
        .file_stem()
        .map(|s| format!("{}.png", s.to_string_lossy()))
        .unwrap_or_else(|| "Screenshot.png".into());
    panel.setNameFieldStringValue(&NSString::from_str(&name));
    NSApplication::sharedApplication(mtm).activate();
    if panel.runModal() != NSModalResponseOK {
        return;
    }
    let Some(path) = panel.URL().and_then(|u| u.path()) else {
        return;
    };
    let path = PathBuf::from(path.to_string());
    let format = path
        .extension()
        .and_then(|e| e.to_str())
        .and_then(ImageFormat::from_extension)
        .unwrap_or(ImageFormat::Png);
    // High quality for JPEG; lossless for WebP.
    let quality = if format == ImageFormat::Webp {
        1.0
    } else {
        0.9
    };
    match output::save_to(&path, &pin.image, format, quality) {
        Ok(_) => tracing::info!(path = %path.display(), "pinned screenshot saved"),
        Err(e) => tracing::error!(error = %e, "could not save pinned screenshot"),
    }
}

fn reveal(path: &Path) {
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    NSWorkspace::sharedWorkspace()
        .activateFileViewerSelectingURLs(&NSArray::from_retained_slice(&[url]));
}

/// Context menu tags.
mod tag {
    pub const COPY: isize = 1;
    pub const SAVE_AS: isize = 2;
    pub const SHOW_IN_FINDER: isize = 3;
    pub const ACTUAL_SIZE: isize = 4;
    pub const LOCK: isize = 5;
    pub const CLOSE: isize = 6;
    pub const ARRANGE: isize = 7;
    /// Opacity items use the percentage plus this offset.
    pub const OPACITY: isize = 100;
}

fn defer(f: impl FnOnce(MainThreadMarker) + Send + 'static) {
    DispatchQueue::main().exec_async(move || {
        f(MainThreadMarker::new().expect("the main queue runs on the main thread"));
    });
}

fn close_rect() -> Rect {
    Rect::new(6.0, 6.0, 22.0, 22.0)
}

/// How close to a corner (in points) a press starts resizing.
const CORNER_GRAB: f64 = 14.0;

/// The corner of `bounds` (flipped view coordinates) within reach of `p`,
/// as its opposite corner in screen coordinates: the point that stays put
/// while resizing.
fn grabbed_corner(view: &NSView, bounds: Rect, p: Point) -> Option<NSPoint> {
    let near = |a: f64, b: f64| (a - b).abs() <= CORNER_GRAB;
    let left = near(p.x, bounds.x);
    let right = near(p.x, bounds.max_x());
    let top = near(p.y, bounds.y);
    let bottom = near(p.y, bounds.max_y());
    if !(left || right) || !(top || bottom) {
        return None;
    }
    // The opposite corner, in the view's flipped coordinates.
    let fixed = NSPoint::new(
        if left { bounds.max_x() } else { bounds.x },
        if top { bounds.max_y() } else { bounds.y },
    );
    let window = view.window()?;
    let in_window = view.convertPoint_toView(fixed, None);
    Some(window.convertPointToScreen(in_window))
}

/// The frame for resizing from `fixed` (screen points) toward `mouse`,
/// keeping `aspect` (width / height), at least `min` wide.
fn resized_frame(fixed: NSPoint, mouse: NSPoint, aspect: f64, min: f64) -> NSRect {
    let width = (mouse.x - fixed.x)
        .abs()
        .max((mouse.y - fixed.y).abs() * aspect)
        .max(min)
        .round();
    let height = (width / aspect).round();
    let x = if mouse.x < fixed.x {
        fixed.x - width
    } else {
        fixed.x
    };
    let y = if mouse.y < fixed.y {
        fixed.y - height
    } else {
        fixed.y
    };
    NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
}

define_class!(
    /// Borderless panels cannot become key by default; pins must, for
    /// keyboard shortcuts.
    // SAFETY: NSPanel can be subclassed; no Drop implementation.
    #[unsafe(super(NSPanel, NSWindow, NSResponder, objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    struct PinPanel;

    impl PinPanel {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool {
            true
        }
    }
);

pub struct PinIvars {
    id: u64,
    hover: Cell<bool>,
    /// While resizing: the corner that stays put, in screen points.
    resizing: Cell<Option<NSPoint>>,
}

define_class!(
    // SAFETY: NSView can be subclassed; no Drop implementation.
    #[unsafe(super(NSView, NSResponder, objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = PinIvars]
    struct PinView;

    impl PinView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            self.draw();
        }

        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _event: &NSEvent) {
            self.ivars().hover.set(true);
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            self.ivars().hover.set(false);
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let id = self.ivars().id;
            let p = self.convertPoint_fromView(event.locationInWindow(), None);
            if close_rect().contains(Point::new(p.x, p.y)) {
                defer(move |_| close(id));
                return;
            }
            let b = self.bounds();
            let bounds = Rect::new(0.0, 0.0, b.size.width, b.size.height);
            if let Some(fixed) = grabbed_corner(self, bounds, Point::new(p.x, p.y)) {
                self.ivars().resizing.set(Some(fixed));
                if let Some(window) = self.window() {
                    window.makeKeyWindow();
                }
                return;
            }
            if let Some(window) = self.window() {
                window.makeKeyWindow();
                window.performWindowDragWithEvent(event);
            }
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, _event: &NSEvent) {
            let (Some(fixed), Some(pin)) = (self.ivars().resizing.get(), find(self.ivars().id))
            else {
                return;
            };
            let aspect = pin.base.width / pin.base.height;
            let min = (pin.base.width * MIN_ZOOM).max(24.0);
            let frame = resized_frame(fixed, NSEvent::mouseLocation(), aspect, min);
            pin.zoom.set(frame.size.width / pin.base.width);
            pin.panel.setFrame_display(frame, true);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            self.ivars().resizing.set(None);
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            if let Some(pin) = find(self.ivars().id) {
                let menu = self.context_menu(&pin);
                NSMenu::popUpContextMenu_withEvent_forView(&menu, event, self);
            }
        }

        #[unsafe(method(magnifyWithEvent:))]
        fn magnify_with_event(&self, event: &NSEvent) {
            if let Some(pin) = find(self.ivars().id) {
                set_zoom(&pin, pin.zoom.get() * (1.0 + event.magnification()));
            }
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            const C: u16 = 8;
            const W: u16 = 13;
            const EQUALS: u16 = 24;
            const MINUS: u16 = 27;
            const ZERO: u16 = 29;
            const ESCAPE: u16 = 53;
            let Some(pin) = find(self.ivars().id) else {
                return;
            };
            let command = event.modifierFlags().contains(NSEventModifierFlags::Command);
            let id = pin.id;
            match (command, event.keyCode()) {
                (_, ESCAPE) | (true, W) => defer(move |_| close(id)),
                (true, C) => copy(&pin),
                (true, EQUALS) => set_zoom(&pin, pin.zoom.get() * 1.25),
                (true, MINUS) => set_zoom(&pin, pin.zoom.get() / 1.25),
                (true, ZERO) => set_zoom(&pin, 1.0),
                _ => {}
            }
        }

        #[unsafe(method(pinMenuAction:))]
        fn pin_menu_action(&self, sender: &NSMenuItem) {
            let Some(pin) = find(self.ivars().id) else {
                return;
            };
            let id = pin.id;
            match sender.tag() {
                tag::COPY => copy(&pin),
                tag::SAVE_AS => defer(move |mtm| {
                    if let Some(pin) = find(id) {
                        save_as(mtm, &pin);
                    }
                }),
                tag::SHOW_IN_FINDER => reveal(&pin.file),
                tag::ACTUAL_SIZE => set_zoom(&pin, 1.0),
                tag::LOCK => set_locked(&pin, true),
                tag::CLOSE => defer(move |_| close(id)),
                tag::ARRANGE => defer(arrange),
                t if t > tag::OPACITY => {
                    pin.panel.setAlphaValue((t - tag::OPACITY) as f64 / 100.0);
                }
                _ => {}
            }
        }
    }
);

impl PinView {
    fn new(mtm: MainThreadMarker, id: u64, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(PinIvars {
            id,
            hover: Cell::new(false),
            resizing: Cell::new(None),
        });
        // SAFETY: NSView's designated initializer.
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    fn context_menu(&self, pin: &Pin) -> Retained<NSMenu> {
        let mtm = self.mtm();
        let menu = NSMenu::new(mtm);
        menu.setAutoenablesItems(false);
        let add = |menu: &NSMenu, title: &str, tag: isize| {
            // SAFETY: `pinMenuAction:` exists on PinView with the expected
            // signature, and the view outlives its menu.
            let item = unsafe {
                let item = NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &NSString::from_str(title),
                    Some(sel!(pinMenuAction:)),
                    ns_string!(""),
                );
                item.setTarget(Some(self as &AnyObject));
                item
            };
            item.setTag(tag);
            menu.addItem(&item);
            item
        };
        add(&menu, "Copy", tag::COPY);
        add(&menu, "Save As…", tag::SAVE_AS);
        if pin.saved {
            add(&menu, "Show in Finder", tag::SHOW_IN_FINDER);
        }
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        add(&menu, "Actual Size", tag::ACTUAL_SIZE);
        let opacity = NSMenu::new(mtm);
        let current = (pin.panel.alphaValue() * 100.0).round() as isize;
        for percent in [100, 75, 50, 25] {
            let item = add(&opacity, &format!("{percent}%"), tag::OPACITY + percent);
            item.setState(if percent == current {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            });
        }
        let opacity_item = NSMenuItem::new(mtm);
        opacity_item.setTitle(ns_string!("Opacity"));
        opacity_item.setSubmenu(Some(&opacity));
        menu.addItem(&opacity_item);
        add(&menu, "Lock (Click Through)", tag::LOCK);
        if count() > 1 {
            add(&menu, "Arrange All Pins", tag::ARRANGE);
        }
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        add(&menu, "Close", tag::CLOSE);
        menu
    }

    fn draw(&self) {
        let Some(pin) = find(self.ivars().id) else {
            return;
        };
        let b = self.bounds();
        let bounds = Rect::new(0.0, 0.0, b.size.width, b.size.height);
        if let Some(context) = NSGraphicsContext::currentContext() {
            context.setImageInterpolation(NSImageInterpolation::High);
        }
        // SAFETY: drawing inside drawRect:.
        unsafe {
            pin.ns_image
                .drawInRect_fromRect_operation_fraction_respectFlipped_hints(
                    b,
                    NSRect::ZERO,
                    NSCompositingOperation::SourceOver,
                    1.0,
                    true,
                    None,
                );
        }
        if self.ivars().hover.get() {
            NSColor::colorWithWhite_alpha(1.0, 0.35).setStroke();
            draw::stroke_rect(draw::inset(bounds, 0.5), 1.0);
            let close = close_rect();
            draw::fill_rounded(
                close,
                close.width / 2.0,
                &NSColor::colorWithWhite_alpha(0.0, 0.6),
            );
            draw::draw_symbol("xmark", close, 10.0, &NSColor::whiteColor());
            // Corner marks: drag a corner to resize. White on a dark
            // outline, so they show on light and dark images alike.
            let outline = NSColor::colorWithWhite_alpha(0.0, 0.55);
            let mark = NSColor::colorWithWhite_alpha(1.0, 0.95);
            let (len, inset) = (10.0, 4.0);
            for (x, y, dx, dy) in [
                (bounds.max_x() - inset, bounds.y + inset, -1.0, 1.0),
                (bounds.x + inset, bounds.max_y() - inset, 1.0, -1.0),
                (bounds.max_x() - inset, bounds.max_y() - inset, -1.0, -1.0),
            ] {
                let horizontal = Rect::new(x.min(x + dx * len), y.min(y + dy * 2.0), len, 2.0);
                let vertical = Rect::new(x.min(x + dx * 2.0), y.min(y + dy * len), 2.0, len);
                for (color, grow) in [(&outline, 1.0), (&mark, 0.0)] {
                    draw::fill_rect(draw::inset(horizontal, -grow), color);
                    draw::fill_rect(draw::inset(vertical, -grow), color);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arranges_in_rows_at_full_size_when_they_fit() {
        let area = Rect::new(0.0, 0.0, 1000.0, 800.0);
        let (rects, factor) = arrange_layout(
            &[(400.0, 300.0), (400.0, 200.0), (300.0, 300.0)],
            area,
            10.0,
        );
        assert_eq!(factor, 1.0);
        assert_eq!(rects[0], Rect::new(0.0, 0.0, 400.0, 300.0));
        assert_eq!(rects[1], Rect::new(410.0, 0.0, 400.0, 200.0));
        // Does not fit beside the first two: next row, below the tallest.
        assert_eq!(rects[2], Rect::new(0.0, 310.0, 300.0, 300.0));
    }

    #[test]
    fn scales_everything_down_together_to_fit() {
        let area = Rect::new(0.0, 0.0, 1000.0, 600.0);
        let sizes = [(900.0, 500.0), (900.0, 500.0), (900.0, 500.0)];
        let (rects, factor) = arrange_layout(&sizes, area, 10.0);
        assert!(factor < 1.0 && factor > 0.3, "{factor}");
        for r in &rects {
            assert!(
                r.x >= 0.0 && r.max_x() <= 1000.0 && r.max_y() <= 600.0,
                "{r:?}"
            );
            assert!((r.width / r.height - 1.8).abs() < 1e-9);
        }
        // No two overlap.
        for (i, a) in rects.iter().enumerate() {
            for b in &rects[i + 1..] {
                let o = a.intersection(b);
                assert!(o.width <= 0.0 || o.height <= 0.0, "{a:?} overlaps {b:?}");
            }
        }
        // Nearly as large as possible: a bit larger would not fit.
        let bigger: Vec<_> = sizes
            .iter()
            .map(|(w, h)| (w * (factor + 0.02), h * (factor + 0.02)))
            .collect();
        assert!(arrange_layout(&bigger, area, 10.0).1 < 1.0);
    }

    #[test]
    fn a_single_huge_pin_is_scaled_to_the_area() {
        let (rects, factor) = arrange_layout(
            &[(4000.0, 2000.0)],
            Rect::new(0.0, 0.0, 1000.0, 1000.0),
            10.0,
        );
        assert!((factor - 0.25).abs() < 0.01, "{factor}");
        assert!(rects[0].width <= 1000.0);
    }

    #[test]
    fn resizing_keeps_the_shape_and_the_opposite_corner() {
        // Dragging the top-right corner of a 200 x 100 pin whose
        // bottom-left corner is at (100, 100).
        let fixed = NSPoint::new(100.0, 100.0);
        let f = resized_frame(fixed, NSPoint::new(500.0, 150.0), 2.0, 24.0);
        assert_eq!((f.origin.x, f.origin.y), (100.0, 100.0));
        assert_eq!((f.size.width, f.size.height), (400.0, 200.0));
        // Past the fixed corner: flips over, same shape.
        let f = resized_frame(fixed, NSPoint::new(20.0, 90.0), 2.0, 24.0);
        assert_eq!(
            (f.origin.x, f.origin.y, f.size.width, f.size.height),
            (20.0, 60.0, 80.0, 40.0)
        );
        // Never smaller than the minimum.
        let f = resized_frame(fixed, NSPoint::new(101.0, 101.0), 2.0, 24.0);
        assert_eq!((f.size.width, f.size.height), (24.0, 12.0));
    }
}
