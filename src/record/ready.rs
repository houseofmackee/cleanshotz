//! The panel between choosing what to record and recording, like QuickTime
//! Player's: pick the camera (or none) and the microphone (or none), then
//! Record (Return) or Cancel (Escape). Choosing a camera shows its bubble
//! right away, so it can be checked before recording.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAccessibility, NSAppearance, NSAppearanceCustomization, NSAppearanceNameDarkAqua,
    NSBackingStoreType, NSBezelStyle, NSBox, NSBoxType, NSButton, NSColor, NSImage, NSImageView,
    NSPanel, NSPopUpButton, NSScreen, NSStatusWindowLevel, NSTitlePosition,
    NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use super::devices::{self, Device};
use super::overlays::Camera;
use crate::config::CameraShape;
use crate::ui::panels;

/// What to record with; `None` is off.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Choice {
    pub camera: Option<String>,
    pub microphone: Option<String>,
}

/// Called with the choice and the camera bubble already showing (if any),
/// or `None` when cancelled.
pub type Done = Box<dyn FnOnce(Option<(Choice, Option<Camera>)>)>;

/// How the camera bubble looks, for the preview.
#[derive(Clone, Copy)]
pub struct Bubble {
    pub size: f64,
    pub shape: CameraShape,
}

struct State {
    panel: Retained<ReadyPanel>,
    camera_menu: Retained<NSPopUpButton>,
    microphone_menu: Retained<NSPopUpButton>,
    cameras: Vec<Device>,
    microphones: Vec<Device>,
    area: NSRect,
    bubble: Bubble,
    preview: Option<Camera>,
    done: Option<Done>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    static TARGET: RefCell<Option<Retained<ReadyTarget>>> = const { RefCell::new(None) };
}

define_class!(
    /// Borderless panels cannot take the keyboard by default; this one must,
    /// for Return and Escape.
    #[unsafe(super(NSPanel))]
    #[thread_kind = MainThreadOnly]
    struct ReadyPanel;

    impl ReadyPanel {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool {
            true
        }
    }
);

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    struct ReadyTarget;

    impl ReadyTarget {
        #[unsafe(method(cameraChanged:))]
        fn camera_changed(&self, _sender: &AnyObject) {
            preview_camera(self.mtm());
        }

        #[unsafe(method(record:))]
        fn record(&self, _sender: &AnyObject) {
            finish(true);
        }

        #[unsafe(method(cancel:))]
        fn cancel(&self, _sender: &AnyObject) {
            finish(false);
        }
    }
);

fn target(mtm: MainThreadMarker) -> Retained<ReadyTarget> {
    TARGET.with(|t| {
        t.borrow_mut()
            .get_or_insert_with(|| {
                let this = ReadyTarget::alloc(mtm).set_ivars(());
                // SAFETY: NSObject's designated initializer.
                unsafe { msg_send![super(this), init] }
            })
            .clone()
    })
}

pub fn is_showing() -> bool {
    STATE.with(|s| s.borrow().is_some())
}

/// Shows the panel below (or above) `area` (Cocoa screen coordinates).
/// `camera_allowed`: false for window recordings, which leave the camera
/// out.
pub fn show(
    mtm: MainThreadMarker,
    area: NSRect,
    camera_allowed: bool,
    initial: &Choice,
    bubble: Bubble,
    done: Done,
) {
    cancel_quietly();
    let cameras = if camera_allowed {
        devices::cameras()
    } else {
        Vec::new()
    };
    let microphones = devices::microphones();
    let size = NSSize::new(560.0, 52.0);
    let visible = NSScreen::screens(mtm)
        .iter()
        .find(|s| {
            let f = s.frame();
            let (cx, cy) = (
                area.origin.x + area.size.width / 2.0,
                area.origin.y + area.size.height / 2.0,
            );
            cx >= f.origin.x
                && cx <= f.origin.x + f.size.width
                && cy >= f.origin.y
                && cy <= f.origin.y + f.size.height
        })
        .or_else(|| NSScreen::mainScreen(mtm))
        .map_or(area, |s| s.visibleFrame());
    let frame = NSRect::new(panels::control_origin(area, visible, size), size);
    // SAFETY: designated NSPanel initializer on our subclass.
    let panel: Retained<ReadyPanel> = unsafe {
        msg_send![
            ReadyPanel::alloc(mtm),
            initWithContentRect: frame,
            styleMask: NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
            backing: NSBackingStoreType::Buffered,
            defer: false
        ]
    };
    // SAFETY: STATE keeps a strong reference while the panel is shown.
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setLevel(NSStatusWindowLevel);
    panel.setOpaque(false);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    panel.setHasShadow(true);
    panel.setHidesOnDeactivate(false);
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::Stationary,
    );
    // SAFETY: reading an AppKit constant.
    if let Some(dark) = NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }) {
        panel.setAppearance(Some(&dark));
    }

    let background = NSBox::new(mtm);
    background.setBoxType(NSBoxType::Custom);
    background.setTitlePosition(NSTitlePosition::NoTitle);
    background.setBorderWidth(0.0);
    background.setCornerRadius(14.0);
    background.setFillColor(&NSColor::colorWithWhite_alpha(0.12, 0.95));
    background.setFrame(NSRect::new(NSPoint::ZERO, size));
    panel.setContentView(Some(&background));

    let target = target(mtm);
    let target: &AnyObject = target.as_ref();
    let icon = |symbol: &str, x: f64| {
        let view = NSImageView::new(mtm);
        if let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str(symbol),
            None,
        ) {
            view.setImage(Some(&image));
        }
        view.setContentTintColor(Some(&NSColor::whiteColor()));
        view.setFrame(NSRect::new(NSPoint::new(x, 15.0), NSSize::new(22.0, 22.0)));
        background.addSubview(&view);
    };
    let menu = |items: Vec<String>, x: f64, action, label: &str| {
        let popup = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(mtm),
            NSRect::new(NSPoint::new(x, 12.0), NSSize::new(170.0, 28.0)),
            false,
        );
        for item in items {
            popup.addItemWithTitle(&NSString::from_str(&item));
        }
        // SAFETY: the action exists on the target, which lives for the
        // whole process.
        unsafe {
            popup.setTarget(Some(target));
            popup.setAction(action);
        }
        popup.setAccessibilityLabel(Some(&NSString::from_str(&crate::i18n::tr(label))));
        background.addSubview(&popup);
        popup
    };
    let names = |none: String, list: &[Device]| {
        std::iter::once(none)
            .chain(list.iter().map(|d| d.name.clone()))
            .collect::<Vec<_>>()
    };
    icon("video.fill", 14.0);
    let camera_menu = menu(
        names(t!("No Camera"), &cameras),
        40.0,
        Some(sel!(cameraChanged:)),
        "Camera",
    );
    camera_menu.setEnabled(camera_allowed && !cameras.is_empty());
    if !camera_allowed {
        camera_menu.setToolTip(Some(&NSString::from_str(&t!(
            "Window recordings leave the camera out"
        ))));
    }
    icon("mic.fill", 220.0);
    let microphone_menu = menu(
        names(t!("No Microphone"), &microphones),
        246.0,
        None,
        "Microphone",
    );
    let button = |title: &str, x: f64, width: f64, action, key: &str| {
        // SAFETY: the action exists on the target.
        let b = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str(&crate::i18n::tr(title)),
                Some(target),
                Some(action),
                mtm,
            )
        };
        b.setBezelStyle(NSBezelStyle::Push);
        b.setKeyEquivalent(&NSString::from_str(key));
        b.setFrame(NSRect::new(NSPoint::new(x, 11.0), NSSize::new(width, 30.0)));
        background.addSubview(&b);
        b
    };
    button("Cancel", 424.0, 60.0, sel!(cancel:), "\u{1b}");
    let record = button("Record", 488.0, 64.0, sel!(record:), "\r");
    record.setBezelColor(Some(&NSColor::systemRedColor()));

    // The previous choice, or the system defaults.
    let select = |popup: &NSPopUpButton, list: &[Device], chosen: Option<&String>| {
        let index = chosen
            .and_then(|id| list.iter().position(|d| d.id == *id))
            .map_or(0, |i| i + 1);
        popup.selectItemAtIndex(index as isize);
    };
    select(&camera_menu, &cameras, initial.camera.as_ref());
    select(&microphone_menu, &microphones, initial.microphone.as_ref());

    crate::ui::bring_forward(mtm, &panel);
    STATE.with(|s| {
        *s.borrow_mut() = Some(State {
            panel,
            camera_menu,
            microphone_menu,
            cameras,
            microphones,
            area,
            bubble,
            preview: None,
            done: Some(done),
        });
    });
    preview_camera(mtm);
    crate::ui::access::announce(
        mtm,
        &t!("Choose the camera and microphone, then press Return to record or Escape to cancel."),
    );
}

/// Shows the chosen camera's bubble (or none).
fn preview_camera(mtm: MainThreadMarker) {
    STATE.with(|s| {
        let mut state = s.borrow_mut();
        let Some(state) = state.as_mut() else {
            return;
        };
        if let Some(old) = state.preview.take() {
            old.stop();
        }
        let index = state.camera_menu.indexOfSelectedItem();
        let Some(device) = usize::try_from(index - 1)
            .ok()
            .and_then(|i| state.cameras.get(i))
        else {
            return;
        };
        match Camera::start(
            mtm,
            state.area,
            state.bubble.size,
            state.bubble.shape,
            &device.id,
        ) {
            Ok(camera) => state.preview = Some(camera),
            Err(e) => {
                tracing::warn!(error = %e, "camera not shown");
                crate::ui::toast::show(mtm, &e);
                state.camera_menu.selectItemAtIndex(0);
            }
        }
    });
}

fn chosen(popup: &NSPopUpButton, list: &[Device]) -> Option<String> {
    usize::try_from(popup.indexOfSelectedItem() - 1)
        .ok()
        .and_then(|i| list.get(i))
        .map(|d| d.id.clone())
}

fn finish(record: bool) {
    let Some(mut state) = STATE.with(|s| s.borrow_mut().take()) else {
        return;
    };
    state.panel.orderOut(None);
    let choice = Choice {
        camera: chosen(&state.camera_menu, &state.cameras),
        microphone: chosen(&state.microphone_menu, &state.microphones),
    };
    let preview = state.preview.take();
    let Some(done) = state.done.take() else {
        return;
    };
    if record {
        done(Some((choice, preview)));
    } else {
        if let Some(camera) = preview {
            camera.stop();
        }
        done(None);
    }
}

/// Records with the current choice (Record Screen pressed again).
pub fn record_now() {
    finish(true);
}

/// Closes a panel left open (a new recording replaces it).
fn cancel_quietly() {
    finish(false);
}
