//! System-wide hotkeys via Carbon's `RegisterEventHotKey`.
//!
//! Unlike a `CGEventTap`, this needs no Accessibility permission. Events are
//! delivered on the main thread through the application event target, so the
//! handler runs inside the normal `NSApplication` run loop.

use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr;
use std::rc::Rc;

use crate::shortcut::Shortcut;

type OSStatus = i32;
type EventRef = *mut c_void;
type EventHandlerCallRef = *mut c_void;
type EventTargetRef = *mut c_void;
type EventHandlerRef = *mut c_void;
type EventHotKeyRef = *mut c_void;
type EventHandlerUPP = unsafe extern "C" fn(EventHandlerCallRef, EventRef, *mut c_void) -> OSStatus;

#[repr(C)]
struct EventTypeSpec {
    event_class: u32,
    event_kind: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct EventHotKeyID {
    signature: u32,
    id: u32,
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn GetApplicationEventTarget() -> EventTargetRef;
    fn InstallEventHandler(
        target: EventTargetRef,
        handler: EventHandlerUPP,
        num_types: u32,
        list: *const EventTypeSpec,
        user_data: *mut c_void,
        out_ref: *mut EventHandlerRef,
    ) -> OSStatus;
    fn RegisterEventHotKey(
        key_code: u32,
        modifiers: u32,
        id: EventHotKeyID,
        target: EventTargetRef,
        options: u32,
        out_ref: *mut EventHotKeyRef,
    ) -> OSStatus;
    fn UnregisterEventHotKey(hotkey: EventHotKeyRef) -> OSStatus;
    fn GetEventParameter(
        event: EventRef,
        name: u32,
        desired_type: u32,
        actual_type: *mut u32,
        buffer_size: usize,
        actual_size: *mut usize,
        data: *mut c_void,
    ) -> OSStatus;
}

const fn four_cc(code: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*code)
}

const SIGNATURE: u32 = four_cc(b"CSZh");
const K_EVENT_CLASS_KEYBOARD: u32 = four_cc(b"keyb");
const K_EVENT_HOT_KEY_PRESSED: u32 = 5;
const K_EVENT_PARAM_DIRECT_OBJECT: u32 = four_cc(b"----");
const TYPE_EVENT_HOT_KEY_ID: u32 = four_cc(b"hkid");
const NO_ERR: OSStatus = 0;
const EVENT_NOT_HANDLED_ERR: OSStatus = -9874;
const EVENT_HOT_KEY_EXISTS_ERR: OSStatus = -9878;

type Handler = Rc<dyn Fn(u32)>;

thread_local! {
    static HANDLER: RefCell<Option<Handler>> = const { RefCell::new(None) };
}

unsafe extern "C" fn on_hotkey(
    _call: EventHandlerCallRef,
    event: EventRef,
    _user_data: *mut c_void,
) -> OSStatus {
    let mut id = EventHotKeyID::default();
    // SAFETY: `event` is the live event Carbon passed us and `id` is a
    // correctly sized out buffer for `typeEventHotKeyID`.
    let status = unsafe {
        GetEventParameter(
            event,
            K_EVENT_PARAM_DIRECT_OBJECT,
            TYPE_EVENT_HOT_KEY_ID,
            ptr::null_mut(),
            size_of::<EventHotKeyID>(),
            ptr::null_mut(),
            (&raw mut id).cast(),
        )
    };
    if status != NO_ERR || id.signature != SIGNATURE {
        return EVENT_NOT_HANDLED_ERR;
    }
    // Clone the handler out so it can run without the RefCell borrowed (it
    // may open a modal panel that spins a nested run loop).
    let handler = HANDLER.with(|h| h.borrow().clone());
    match handler {
        Some(handler) => {
            handler(id.id);
            NO_ERR
        }
        None => EVENT_NOT_HANDLED_ERR,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RegisterError {
    #[error("{0} is already used by another app or the system")]
    InUse(String),
    #[error("could not register {0} (OSStatus {1})")]
    Failed(String, OSStatus),
}

/// Owns the registered hotkeys; dropping it unregisters them.
pub struct Hotkeys {
    registered: Vec<EventHotKeyRef>,
}

impl Hotkeys {
    /// Installs the Carbon event handler. `handler` receives the `id` passed
    /// to [`Hotkeys::register`]. Must be called on the main thread, once.
    pub fn new(handler: impl Fn(u32) + 'static) -> Self {
        HANDLER.with(|h| *h.borrow_mut() = Some(Rc::new(handler)));
        let spec = EventTypeSpec {
            event_class: K_EVENT_CLASS_KEYBOARD,
            event_kind: K_EVENT_HOT_KEY_PRESSED,
        };
        // SAFETY: plain Carbon call with a valid spec array of length 1. The
        // handler stays installed for the life of the process.
        let status = unsafe {
            InstallEventHandler(
                GetApplicationEventTarget(),
                on_hotkey,
                1,
                &spec,
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        if status != NO_ERR {
            tracing::error!(status, "InstallEventHandler failed; hotkeys disabled");
        }
        Self {
            registered: Vec::new(),
        }
    }

    pub fn register(&mut self, shortcut: &Shortcut, id: u32) -> Result<(), RegisterError> {
        let mut hotkey_ref: EventHotKeyRef = ptr::null_mut();
        // SAFETY: valid arguments; `hotkey_ref` is a valid out pointer.
        let status = unsafe {
            RegisterEventHotKey(
                shortcut.key.code,
                shortcut.modifiers.carbon_mask(),
                EventHotKeyID {
                    signature: SIGNATURE,
                    id,
                },
                GetApplicationEventTarget(),
                0,
                &mut hotkey_ref,
            )
        };
        match status {
            NO_ERR => {
                self.registered.push(hotkey_ref);
                Ok(())
            }
            EVENT_HOT_KEY_EXISTS_ERR => Err(RegisterError::InUse(shortcut.to_string())),
            other => Err(RegisterError::Failed(shortcut.to_string(), other)),
        }
    }

    pub fn unregister_all(&mut self) {
        for hotkey_ref in self.registered.drain(..) {
            // SAFETY: each ref came from a successful RegisterEventHotKey and
            // is unregistered exactly once.
            unsafe { UnregisterEventHotKey(hotkey_ref) };
        }
    }
}

impl Drop for Hotkeys {
    fn drop(&mut self) {
        self.unregister_all();
    }
}
