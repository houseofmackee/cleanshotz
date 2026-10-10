//! Overlays that are part of a recording: the camera bubble and the
//! keystroke display.
//!
//! They float just below CleanShotZ's status-level UI (which recordings
//! leave out), and must exist before the recording's content filter is
//! built so the filter can include them.

use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr::{self, NonNull};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSBox, NSBoxType, NSColor, NSFont, NSFontWeightSemibold, NSPanel,
    NSTextAlignment, NSTextField, NSTitlePosition, NSView, NSWindowCollectionBehavior,
    NSWindowStyleMask,
};
use objc2_av_foundation::{
    AVCaptureDevice, AVCaptureDeviceInput, AVCaptureSession, AVCaptureVideoPreviewLayer,
    AVLayerVideoGravityResizeAspectFill, AVMediaTypeVideo,
};
use objc2_core_foundation::{
    CFMachPort, CFRetained, CFRunLoop, CFRunLoopSource, kCFRunLoopCommonModes,
};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString, NSTimer};

use super::keys;
use crate::config::{CameraShape, KeystrokeMode};
use crate::platform::permissions::{Permission, Status};

/// Above normal and floating windows, below status level (25), where
/// recordings leave CleanShotZ's own UI out.
const OVERLAY_LEVEL: isize = 24;

fn overlay_panel(mtm: MainThreadMarker, frame: NSRect) -> Retained<NSPanel> {
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        frame,
        NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
        NSBackingStoreType::Buffered,
        false,
    );
    // SAFETY: the overlay owner keeps a strong reference.
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setLevel(OVERLAY_LEVEL);
    panel.setOpaque(false);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    panel.setHidesOnDeactivate(false);
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::Stationary,
    );
    panel
}

/// Moves a value into a background closure.
struct AssertSend<T>(T);
// SAFETY: AVCaptureSession may be started and stopped from any thread.
unsafe impl<T> Send for AssertSend<T> {}
impl<T> AssertSend<T> {
    fn into_inner(self) -> T {
        self.0
    }
}

pub struct Camera {
    /// `None` in fake-camera test mode.
    session: Option<Retained<AVCaptureSession>>,
    panel: Retained<NSPanel>,
}

impl Camera {
    /// Shows the camera bubble in the bottom-right corner of `area`
    /// (Cocoa screen coordinates). It can be dragged during the recording.
    pub fn start(
        mtm: MainThreadMarker,
        area: NSRect,
        size: f64,
        shape: CameraShape,
    ) -> Result<Camera, String> {
        // Debug builds: CLEANSHOTZ_FAKE_CAMERA=1 shows the bubble with a
        // placeholder instead of the camera (no permission, no camera use).
        let fake = cfg!(debug_assertions)
            && std::env::var_os("CLEANSHOTZ_FAKE_CAMERA").is_some_and(|v| !v.is_empty());
        let session = if fake { None } else { Some(Self::session()?) };
        let panel = Self::bubble(mtm, area, size, shape, session.as_deref());
        if let Some(session) = &session {
            // Starting a capture session blocks; keep it off the main thread.
            let starting = AssertSend(session.clone());
            std::thread::spawn(move || {
                let session = starting.into_inner();
                // SAFETY: AVCaptureSession may be started from any thread.
                unsafe { session.startRunning() };
            });
        }
        Ok(Camera { session, panel })
    }

    fn session() -> Result<Retained<AVCaptureSession>, String> {
        match Permission::Camera.status() {
            Status::Granted => {}
            Status::NotDetermined => {
                Permission::Camera.request();
                return Err("Allow camera access, then record again to include the camera".into());
            }
            Status::Denied => {
                return Err("Camera access is off in System Settings > Privacy & Security".into());
            }
        }
        // SAFETY: reading AVFoundation constants and creating capture objects.
        let session = unsafe {
            let media = AVMediaTypeVideo.ok_or("no video media type")?;
            let device =
                AVCaptureDevice::defaultDeviceWithMediaType(media).ok_or("no camera found")?;
            let input = AVCaptureDeviceInput::deviceInputWithDevice_error(&device)
                .map_err(|e| e.localizedDescription().to_string())?;
            let session = AVCaptureSession::new();
            if !session.canAddInput(&input) {
                return Err("the camera cannot be used right now".into());
            }
            session.addInput(&input);
            session
        };
        Ok(session)
    }

    fn bubble(
        mtm: MainThreadMarker,
        area: NSRect,
        size: f64,
        shape: CameraShape,
        session: Option<&AVCaptureSession>,
    ) -> Retained<NSPanel> {
        let side = size
            .min(area.size.width * 0.4)
            .min(area.size.height * 0.4)
            .max(64.0)
            .round();
        let margin = 24.0;
        let frame = NSRect::new(
            NSPoint::new(
                area.origin.x + area.size.width - side - margin,
                area.origin.y + margin,
            ),
            NSSize::new(side, side),
        );
        let panel = overlay_panel(mtm, frame);
        panel.setHasShadow(true);
        panel.setMovableByWindowBackground(true);

        let view = NSView::new(mtm);
        view.setFrame(NSRect::new(NSPoint::ZERO, frame.size));
        view.setWantsLayer(true);
        let radius = match shape {
            CameraShape::Circle => side / 2.0,
            CameraShape::Rounded => side * 0.18,
        };
        if let Some(layer) = view.layer() {
            layer.setCornerRadius(radius);
            layer.setMasksToBounds(true);
            layer.setBackgroundColor(Some(&NSColor::blackColor().CGColor()));
            layer.setBorderWidth(3.0);
            layer.setBorderColor(Some(&NSColor::colorWithWhite_alpha(1.0, 0.9).CGColor()));
            if session.is_none() {
                layer.setBackgroundColor(Some(
                    &NSColor::colorWithSRGBRed_green_blue_alpha(0.25, 0.45, 0.85, 1.0).CGColor(),
                ));
            }
            // SAFETY: configuring a preview layer for a configured session.
            if let Some(session) = session {
                unsafe {
                    let preview = AVCaptureVideoPreviewLayer::layerWithSession(session);
                    if let Some(gravity) = AVLayerVideoGravityResizeAspectFill {
                        preview.setVideoGravity(gravity);
                    }
                    preview.setFrame(view.bounds());
                    // Mirrored, like a mirror and like FaceTime.
                    if let Some(connection) = preview.connection()
                        && connection.isVideoMirroringSupported()
                    {
                        connection.setAutomaticallyAdjustsVideoMirroring(false);
                        connection.setVideoMirrored(true);
                    }
                    layer.addSublayer(&preview);
                }
            }
        }
        panel.setContentView(Some(&view));
        panel.orderFrontRegardless();
        panel
    }

    pub fn stop(self) {
        self.panel.orderOut(None);
        let Some(session) = self.session else {
            return;
        };
        let stopping = AssertSend(session);
        std::thread::spawn(move || {
            let session = stopping.into_inner();
            // SAFETY: AVCaptureSession may be stopped from any thread.
            unsafe { session.stopRunning() };
        });
    }
}

struct KeyState {
    panel: Retained<NSPanel>,
    label: Retained<NSTextField>,
    area: NSRect,
    mode: KeystrokeMode,
    /// The event tap and its run loop source; `None` in fake-key test mode.
    tap: Option<(CFRetained<CFMachPort>, CFRetained<CFRunLoopSource>)>,
    hide: Option<Retained<NSTimer>>,
}

thread_local! {
    static KEYS: RefCell<Option<KeyState>> = const { RefCell::new(None) };
}

/// The keystroke display: shows each key press in a pill at the bottom of
/// the recorded area for a moment.
pub struct KeyDisplay;

impl KeyDisplay {
    /// Needs Input Monitoring permission; asks for it the first time.
    pub fn start(
        mtm: MainThreadMarker,
        area: NSRect,
        mode: KeystrokeMode,
    ) -> Result<KeyDisplay, String> {
        // Debug builds: CLEANSHOTZ_FAKE_KEYS=1 skips the event tap (and its
        // permission); keys then come from the dev-show-key URL command.
        let fake = cfg!(debug_assertions)
            && std::env::var_os("CLEANSHOTZ_FAKE_KEYS").is_some_and(|v| !v.is_empty());
        let tap = if fake {
            None
        } else {
            if Permission::InputMonitoring.status() != Status::Granted {
                Permission::InputMonitoring.request();
                return Err(
                    "Allow Input Monitoring for CleanShotZ, then record again to show keystrokes"
                        .into(),
                );
            }
            let tap = event_tap()?;
            let source = CFMachPort::new_run_loop_source(None, Some(&tap), 0)
                .ok_or("could not listen for key presses")?;
            if let Some(main) = CFRunLoop::main() {
                // SAFETY: reading a CoreFoundation constant.
                main.add_source(Some(&source), unsafe { kCFRunLoopCommonModes });
            }
            Some((tap, source))
        };

        // Created now, invisible, so the recording includes it; shown on
        // key presses.
        let panel = overlay_panel(mtm, NSRect::new(area.origin, NSSize::new(100.0, 44.0)));
        panel.setIgnoresMouseEvents(true);
        panel.setHasShadow(false);
        panel.setAlphaValue(0.0);
        let background = NSBox::new(mtm);
        background.setBoxType(NSBoxType::Custom);
        background.setTitlePosition(NSTitlePosition::NoTitle);
        background.setBorderWidth(0.0);
        background.setCornerRadius(12.0);
        background.setFillColor(&NSColor::colorWithWhite_alpha(0.08, 0.85));
        let label = NSTextField::labelWithString(&NSString::from_str(""), mtm);
        // SAFETY: reading an AppKit constant.
        label.setFont(Some(&NSFont::systemFontOfSize_weight(24.0, unsafe {
            NSFontWeightSemibold
        })));
        label.setTextColor(Some(&NSColor::whiteColor()));
        label.setAlignment(NSTextAlignment::Center);
        background.addSubview(&label);
        panel.setContentView(Some(&background));
        panel.orderFrontRegardless();

        KEYS.with(|k| {
            *k.borrow_mut() = Some(KeyState {
                panel,
                label,
                area,
                mode,
                tap,
                hide: None,
            })
        });
        Ok(KeyDisplay)
    }

    pub fn stop(self) {
        KEYS.with(|k| {
            if let Some(state) = k.borrow_mut().take() {
                if let Some((tap, source)) = &state.tap {
                    tap.invalidate();
                    if let Some(main) = CFRunLoop::main() {
                        // SAFETY: reading a CoreFoundation constant.
                        main.remove_source(Some(source), unsafe { kCFRunLoopCommonModes });
                    }
                }
                if let Some(timer) = &state.hide {
                    timer.invalidate();
                }
                state.panel.orderOut(None);
            }
        });
    }
}

fn event_tap() -> Result<CFRetained<CFMachPort>, String> {
    let mask = 1u64 << CGEventType::KeyDown.0;
    // SAFETY: a listen-only session tap with a valid callback; no user data.
    unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::SessionEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::ListenOnly,
            mask,
            Some(on_key),
            ptr::null_mut(),
        )
    }
    .ok_or_else(|| "could not listen for key presses (Input Monitoring permission)".into())
}

unsafe extern "C-unwind" fn on_key(
    _proxy: CGEventTapProxy,
    kind: CGEventType,
    event: NonNull<CGEvent>,
    _user: *mut c_void,
) -> *mut CGEvent {
    // SAFETY: the event is valid for the duration of the callback.
    let event_ref = unsafe { event.as_ref() };
    if kind == CGEventType::TapDisabledByTimeout || kind == CGEventType::TapDisabledByUserInput {
        KEYS.with(|k| {
            if let Some((tap, _)) = k.borrow().as_ref().and_then(|s| s.tap.as_ref()) {
                CGEvent::tap_enable(tap, true);
            }
        });
    } else if kind == CGEventType::KeyDown {
        let code = CGEvent::integer_value_field(Some(event_ref), CGEventField::KeyboardEventKeycode)
            as u32;
        let flags = CGEvent::flags(Some(event_ref)).0;
        show_key(code, keys::modifiers_from_flags(flags));
    }
    event.as_ptr()
}

fn show_key(code: u32, modifiers: crate::shortcut::Modifiers) {
    KEYS.with(|k| {
        let mut keys = k.borrow_mut();
        let Some(state) = keys.as_mut() else {
            return;
        };
        let Some(text) = keys::label(code, modifiers, state.mode) else {
            return;
        };
        state.label.setStringValue(&NSString::from_str(&text));
        state.label.sizeToFit();
        let text_size = state.label.frame().size;
        let size = NSSize::new((text_size.width + 36.0).max(56.0), text_size.height + 20.0);
        let area = state.area;
        let origin = NSPoint::new(
            (area.origin.x + (area.size.width - size.width) / 2.0).round(),
            area.origin.y + 40.0,
        );
        state
            .panel
            .setFrame_display(NSRect::new(origin, size), true);
        if let Some(content) = state.panel.contentView() {
            content.setFrame(NSRect::new(NSPoint::ZERO, size));
        }
        state.label.setFrame(NSRect::new(
            NSPoint::new(0.0, (size.height - text_size.height) / 2.0),
            NSSize::new(size.width, text_size.height),
        ));
        state.panel.setAlphaValue(1.0);

        if let Some(timer) = state.hide.take() {
            timer.invalidate();
        }
        let block = RcBlock::new(|_timer: NonNull<NSTimer>| {
            KEYS.with(|k| {
                if let Some(state) = k.borrow_mut().as_mut() {
                    state.panel.setAlphaValue(0.0);
                    state.hide = None;
                }
            });
        });
        // SAFETY: the block has the expected signature; the run loop owns
        // the one-shot timer.
        state.hide = Some(unsafe {
            NSTimer::scheduledTimerWithTimeInterval_repeats_block(1.5, false, &block)
        });
    });
}

/// Development aid (debug builds): shows a key press as if it had been
/// typed, without an event tap, to check the display.
#[cfg(debug_assertions)]
pub fn simulate_key(code: u32, flags: u64) {
    show_key(code, keys::modifiers_from_flags(flags));
}
