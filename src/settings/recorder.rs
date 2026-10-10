//! A button that records a keyboard shortcut: click it, then press the
//! keys. Escape cancels; Delete removes the shortcut. Global shortcuts are
//! suspended while recording so the keys reach the button.

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSBezelStyle, NSButton, NSEvent, NSEventModifierFlags};
use objc2_foundation::NSString;

use crate::action::Action;
use crate::shortcut::{Shortcut, key_name};

const ESCAPE: u16 = 53;
const DELETE: u16 = 51;
const FORWARD_DELETE: u16 = 117;

pub enum Outcome {
    Set(String),
    Clear,
    Cancel,
}

pub struct Ivars {
    action: Action,
    shortcut: RefCell<String>,
    recording: Cell<bool>,
    /// A hint is showing; releasing the modifiers keeps it.
    hinting: Cell<bool>,
}

define_class!(
    // SAFETY: NSButton has no subclassing requirements; no Drop.
    #[unsafe(super(NSButton))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    pub struct ShortcutRecorder;

    impl ShortcutRecorder {
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(resignFirstResponder))]
        fn resign_first_responder(&self) -> bool {
            if self.ivars().recording.get() {
                self.finish(Outcome::Cancel);
            }
            // SAFETY: forwarding to NSResponder.
            let resigned: bool = unsafe { msg_send![super(self), resignFirstResponder] };
            resigned
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            if self.ivars().recording.get() {
                self.record(event);
            } else {
                // SAFETY: forwarding to NSButton (Space presses it).
                unsafe { msg_send![super(self), keyDown: event] }
            }
        }

        /// Shortcuts with Command arrive here before `keyDown:` and before
        /// the main menu sees them.
        #[unsafe(method(performKeyEquivalent:))]
        fn perform_key_equivalent(&self, event: &NSEvent) -> bool {
            if self.ivars().recording.get() {
                self.record(event);
                true
            } else {
                // SAFETY: forwarding to NSButton.
                unsafe { msg_send![super(self), performKeyEquivalent: event] }
            }
        }

        #[unsafe(method(flagsChanged:))]
        fn flags_changed(&self, event: &NSEvent) {
            if self.ivars().recording.get() {
                let held = symbols(modifiers(event));
                if !held.is_empty() {
                    self.ivars().hinting.set(false);
                    self.set_title(&format!("{held}…"));
                } else if !self.ivars().hinting.get() {
                    self.set_title("Type shortcut…");
                }
            }
        }

        #[unsafe(method(toggleRecording:))]
        fn toggle_recording(&self, _sender: &AnyObject) {
            if self.ivars().recording.get() {
                self.finish(Outcome::Cancel);
            } else {
                self.start();
            }
        }
    }
);

impl ShortcutRecorder {
    pub fn new(mtm: MainThreadMarker, action: Action, shortcut: &str) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            action,
            shortcut: RefCell::new(shortcut.to_string()),
            recording: Cell::new(false),
            hinting: Cell::new(false),
        });
        // SAFETY: NSObject's initializer; NSButton sets up its cell.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        this.setBezelStyle(NSBezelStyle::Push);
        // SAFETY: the action is defined on this class.
        unsafe {
            this.setTarget(Some(&this));
            this.setAction(Some(sel!(toggleRecording:)));
        }
        this.widthAnchor()
            .constraintEqualToConstant(160.0)
            .setActive(true);
        this.show_shortcut();
        this
    }

    pub fn action(&self) -> Action {
        self.ivars().action
    }

    pub fn set_shortcut(&self, shortcut: &str) {
        *self.ivars().shortcut.borrow_mut() = shortcut.to_string();
        if !self.ivars().recording.get() {
            self.show_shortcut();
        }
    }

    fn start(&self) {
        self.ivars().recording.set(true);
        self.ivars().hinting.set(false);
        super::recording_changed(self.mtm(), true);
        self.set_title("Type shortcut…");
        if let Some(window) = self.window() {
            window.makeFirstResponder(Some(self));
        }
    }

    fn finish(&self, outcome: Outcome) {
        self.ivars().recording.set(false);
        self.show_shortcut();
        super::recorded(self.mtm(), self.ivars().action, outcome);
    }

    fn record(&self, event: &NSEvent) {
        let code = event.keyCode();
        let m = modifiers(event);
        if m.is_empty() && code == ESCAPE {
            self.finish(Outcome::Cancel);
            return;
        }
        if m.is_empty() && (code == DELETE || code == FORWARD_DELETE) {
            self.finish(Outcome::Clear);
            return;
        }
        let Some(key) = key_name(u32::from(code)) else {
            self.hint("That key can't be used");
            return;
        };
        let mut parts = Vec::new();
        for (on, name) in [
            (m.contains(NSEventModifierFlags::Control), "ctrl"),
            (m.contains(NSEventModifierFlags::Option), "alt"),
            (m.contains(NSEventModifierFlags::Shift), "shift"),
            (m.contains(NSEventModifierFlags::Command), "cmd"),
        ] {
            if on {
                parts.push(name);
            }
        }
        parts.push(key);
        let text = parts.join("+");
        let shortcut = match Shortcut::parse(&text) {
            Ok(s) => s,
            Err(_) => {
                self.hint("Add ⌘, ⌥ or ⌃");
                return;
            }
        };
        // Shift alone would take over typing capital letters everywhere.
        let only_shift = shortcut.modifiers.shift
            && !shortcut.modifiers.command
            && !shortcut.modifiers.option
            && !shortcut.modifiers.control;
        if only_shift && !key.starts_with('f') {
            self.hint("Add ⌘, ⌥ or ⌃");
            return;
        }
        self.finish(Outcome::Set(text));
    }

    fn hint(&self, text: &str) {
        self.ivars().hinting.set(true);
        objc2_app_kit::NSBeep();
        self.set_title(text);
    }

    fn show_shortcut(&self) {
        let shortcut = self.ivars().shortcut.borrow().clone();
        let title = match Shortcut::parse(&shortcut) {
            Ok(s) => s.to_string(),
            Err(_) if shortcut.trim().is_empty() => t!("Record Shortcut"),
            Err(_) => t!("Invalid: {shortcut}", shortcut),
        };
        self.set_title(&title);
    }

    fn set_title(&self, title: &str) {
        self.setTitle(&NSString::from_str(title));
    }
}

fn modifiers(event: &NSEvent) -> NSEventModifierFlags {
    event.modifierFlags()
        & (NSEventModifierFlags::Command
            | NSEventModifierFlags::Shift
            | NSEventModifierFlags::Option
            | NSEventModifierFlags::Control)
}

/// Modifier symbols in macOS menu order.
fn symbols(m: NSEventModifierFlags) -> String {
    [
        (NSEventModifierFlags::Control, "⌃"),
        (NSEventModifierFlags::Option, "⌥"),
        (NSEventModifierFlags::Shift, "⇧"),
        (NSEventModifierFlags::Command, "⌘"),
    ]
    .iter()
    .filter(|(flag, _)| m.contains(*flag))
    .map(|(_, s)| *s)
    .collect()
}
