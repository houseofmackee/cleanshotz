//! UI elements under the pointer (buttons, fields, panels), through the
//! accessibility API, for snapping a selection to one. Needs Accessibility
//! permission; without it nothing is found.

use std::ffi::c_void;
use std::ptr;

use objc2_core_foundation::{CFString, CGPoint, CGSize};

use super::geometry::{Point, Rect};

type Element = *mut c_void;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXUIElementCreateApplication(pid: i32) -> Element;
    fn AXUIElementCopyElementAtPosition(
        application: Element,
        x: f32,
        y: f32,
        element: *mut Element,
    ) -> i32;
    fn AXUIElementCopyAttributeValue(
        element: Element,
        attribute: *const c_void,
        value: *mut *mut c_void,
    ) -> i32;
    fn AXUIElementSetMessagingTimeout(element: Element, seconds: f32) -> i32;
    fn AXValueGetValue(value: *mut c_void, kind: u32, out: *mut c_void) -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const c_void);
}

const POINT: u32 = 1; // kAXValueTypeCGPoint
const SIZE: u32 = 2; // kAXValueTypeCGSize

/// Releases a CoreFoundation object when dropped.
struct Owned(*mut c_void);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: an object this module created or copied.
            unsafe { CFRelease(self.0) };
        }
    }
}

fn attribute(element: Element, name: &'static str) -> Option<Owned> {
    let name = CFString::from_static_str(name);
    let mut value: *mut c_void = ptr::null_mut();
    // SAFETY: a valid element and attribute name; the copied value is
    // released by `Owned`.
    let error = unsafe {
        AXUIElementCopyAttributeValue(element, (&*name as *const CFString).cast(), &mut value)
    };
    (error == 0 && !value.is_null()).then_some(Owned(value))
}

/// An element's frame in global points (top-left origin).
fn frame(element: Element) -> Option<Rect> {
    let position = attribute(element, "AXPosition")?;
    let size = attribute(element, "AXSize")?;
    let mut p = CGPoint::new(0.0, 0.0);
    let mut s = CGSize::new(0.0, 0.0);
    // SAFETY: AXValues of the types asked for, read into matching structs.
    let ok = unsafe {
        AXValueGetValue(position.0, POINT, (&mut p as *mut CGPoint).cast())
            && AXValueGetValue(size.0, SIZE, (&mut s as *mut CGSize).cast())
    };
    ok.then(|| Rect::new(p.x, p.y, s.width, s.height))
}

/// The UI element of app `pid` at `at` (global points): the innermost
/// one, or its parent when that is too small to be useful (the text of a
/// button rather than the button).
pub fn at(pid: i32, window: u32, at: Point) -> Option<Rect> {
    // An app cannot answer accessibility requests about itself while its
    // main thread waits for the answer, so this app's windows are hit-tested
    // directly.
    if u32::try_from(pid).ok() == Some(std::process::id()) {
        return own(window, at);
    }
    // SAFETY: creating an element for a process; released by `Owned`.
    let app = Owned(unsafe { AXUIElementCreateApplication(pid) });
    if app.0.is_null() {
        return None;
    }
    // SAFETY: a valid element; a short timeout keeps the pointer smooth
    // when an app is busy.
    unsafe { AXUIElementSetMessagingTimeout(app.0, 0.1) };
    let mut hit: Element = ptr::null_mut();
    // SAFETY: a valid application element; the found element is released
    // by `Owned`.
    let error =
        unsafe { AXUIElementCopyElementAtPosition(app.0, at.x as f32, at.y as f32, &mut hit) };
    if error != 0 || hit.is_null() {
        return None;
    }
    let mut element = Owned(hit);
    let mut rect = frame(element.0)?;
    for _ in 0..4 {
        if rect.width >= 28.0 && rect.height >= 18.0 {
            break;
        }
        let Some(parent) = attribute(element.0, "AXParent") else {
            break;
        };
        let Some(parent_rect) = frame(parent.0) else {
            break;
        };
        element = parent;
        rect = parent_rect;
    }
    (rect.width >= 4.0 && rect.height >= 4.0).then_some(rect)
}

/// What was clicked, for a step's caption.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    Button,
    MenuItem,
    Field,
    Tab,
    Link,
}

impl Control {
    fn from_role(role: &str) -> Option<Control> {
        Some(match role {
            "AXButton"
            | "AXCheckBox"
            | "AXPopUpButton"
            | "AXMenuButton"
            | "AXDisclosureTriangle" => Control::Button,
            "AXMenuItem" | "AXMenuBarItem" => Control::MenuItem,
            "AXTextField" | "AXTextArea" | "AXComboBox" | "AXSearchField" => Control::Field,
            "AXRadioButton" | "AXTab" => Control::Tab,
            "AXLink" => Control::Link,
            _ => return None,
        })
    }
}

/// The control of app `pid` at `at` (global points) and its name: its
/// title or description, from the element hit or, when that is a button's
/// text or icon, the element holding it. Values are never read, so text
/// typed into fields stays out of captions. `None` for this app, without
/// Accessibility permission, or when no named control was clicked.
pub fn control_at(pid: i32, at: Point) -> Option<(Control, String)> {
    if u32::try_from(pid).ok() == Some(std::process::id()) {
        return None;
    }
    // SAFETY: as in `at`.
    let app = Owned(unsafe { AXUIElementCreateApplication(pid) });
    if app.0.is_null() {
        return None;
    }
    // SAFETY: a valid element.
    unsafe { AXUIElementSetMessagingTimeout(app.0, 0.1) };
    let mut hit: Element = ptr::null_mut();
    // SAFETY: as in `at`.
    let error =
        unsafe { AXUIElementCopyElementAtPosition(app.0, at.x as f32, at.y as f32, &mut hit) };
    if error != 0 || hit.is_null() {
        return None;
    }
    let mut element = Owned(hit);
    for _ in 0..3 {
        let role = attribute(element.0, "AXRole").and_then(|v| string(&v))?;
        if let Some(control) = Control::from_role(&role) {
            let names: &[&str] = if control == Control::Field {
                &["AXTitle", "AXDescription", "AXPlaceholderValue"]
            } else {
                &["AXTitle", "AXDescription"]
            };
            return names
                .iter()
                .find_map(|n| attribute(element.0, n).and_then(|v| string(&v)))
                .and_then(|text| usable_name(&text))
                .map(|name| (control, name));
        }
        // Only a control's parts lead up to it.
        if !matches!(role.as_str(), "AXStaticText" | "AXImage" | "AXUnknown") {
            return None;
        }
        element = attribute(element.0, "AXParent")?;
    }
    None
}

/// A CFString's contents; `None` for other types.
fn string(value: &Owned) -> Option<String> {
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFGetTypeID(cf: *const c_void) -> usize;
        fn CFStringGetTypeID() -> usize;
    }
    // SAFETY: a valid CoreFoundation object.
    if unsafe { CFGetTypeID(value.0) != CFStringGetTypeID() } {
        return None;
    }
    // SAFETY: checked to be a CFString, which `Owned` keeps alive.
    let s: &CFString = unsafe { &*(value.0 as *const CFString) };
    Some(s.to_string())
}

/// A name worth putting in a caption: one line, not too long.
fn usable_name(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty() && text.chars().count() <= 60 && !text.contains('\n'))
        .then(|| text.to_string())
}

/// The view of this app's window `window` at `at` (global points,
/// top-left origin), widened to its parent when too small like [`at`].
fn own(window: u32, at: Point) -> Option<Rect> {
    let mtm = objc2::MainThreadMarker::new()?;
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    let window = app.windowWithWindowNumber(window as isize)?;
    let screens = objc2_app_kit::NSScreen::screens(mtm);
    let primary = screens.firstObject()?.frame().size.height;
    let screen_point = objc2_foundation::NSPoint::new(at.x, primary - at.y);
    let local = window.convertPointFromScreen(screen_point);
    // From the window's root view, which also holds title bar accessories.
    let mut root = window.contentView()?;
    // SAFETY: reading the view hierarchy on the main thread.
    while let Some(parent) = unsafe { root.superview() } {
        root = parent;
    }
    let mut view = root.hitTest(local)?;
    let global = |v: &objc2_app_kit::NSView| {
        let r = window.convertRectToScreen(v.convertRect_toView(v.bounds(), None));
        Rect::new(
            r.origin.x,
            primary - r.origin.y - r.size.height,
            r.size.width,
            r.size.height,
        )
    };
    let mut rect = global(&view);
    for _ in 0..4 {
        if rect.width >= 28.0 && rect.height >= 18.0 {
            break;
        }
        // SAFETY: as above.
        let Some(parent) = (unsafe { view.superview() }) else {
            break;
        };
        view = parent;
        rect = global(&view);
    }
    (rect.width >= 4.0 && rect.height >= 4.0).then_some(rect)
}
