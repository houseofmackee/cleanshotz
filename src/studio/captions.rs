//! The captions window of a Studio recording: every caption with its time,
//! to correct the text, play from it or remove it. Edits apply when a
//! field is left (Return, Tab or a click elsewhere).

use std::cell::{OnceCell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAccessibility, NSAutoresizingMaskOptions, NSBackingStoreType, NSButton, NSColor, NSFont,
    NSImage, NSScrollView, NSTextField, NSView, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{NSNotification, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

use crate::captions::Cue;

const WIDTH: f64 = 620.0;
const HEIGHT: f64 = 520.0;
const ROW: f64 = 34.0;

pub struct Ivars {
    /// The Studio editor the captions belong to.
    editor: u64,
    window: OnceCell<Retained<NSWindow>>,
    list: OnceCell<Retained<CaptionList>>,
    fields: RefCell<Vec<Retained<NSTextField>>>,
}

thread_local! {
    static WINDOWS: RefCell<Vec<Retained<CaptionsWindow>>> = const { RefCell::new(Vec::new()) };
}

/// Opens (or brings forward) the captions window of Studio editor `editor`.
pub fn open(mtm: MainThreadMarker, editor: u64) {
    let existing = WINDOWS.with(|w| {
        w.borrow()
            .iter()
            .find(|c| c.ivars().editor == editor)
            .cloned()
    });
    let controller = existing.unwrap_or_else(|| {
        let c = CaptionsWindow::new(mtm, editor);
        WINDOWS.with(|w| w.borrow_mut().push(c.clone()));
        c
    });
    controller.build_rows();
    if let Some(window) = controller.ivars().window.get() {
        crate::ui::bring_forward(mtm, window);
    }
}

/// Shows the captions again after they changed (made again).
pub fn refresh(editor: u64) {
    WINDOWS.with(|w| {
        for c in w.borrow().iter().filter(|c| c.ivars().editor == editor) {
            c.build_rows();
        }
    });
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    struct CaptionsWindow;

    unsafe impl NSObjectProtocol for CaptionsWindow {}

    unsafe impl NSWindowDelegate for CaptionsWindow {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            self.apply_fields();
            let editor = self.ivars().editor;
            dispatch2::DispatchQueue::main().exec_async(move || {
                WINDOWS.with(|w| w.borrow_mut().retain(|c| c.ivars().editor != editor));
            });
        }
    }

    impl CaptionsWindow {
        #[unsafe(method(textEdited:))]
        fn text_edited(&self, _sender: &NSTextField) {
            self.apply_fields();
        }

        #[unsafe(method(play:))]
        fn play(&self, sender: &NSButton) {
            let cues = self.cues();
            if let (Some(cue), Some(editor)) = (
                cues.get(sender.tag() as usize),
                super::registry::get(self.ivars().editor),
            ) {
                editor.play_from(cue.start);
            }
        }

        #[unsafe(method(remove:))]
        fn remove(&self, sender: &NSButton) {
            self.apply_fields();
            let mut cues = self.cues();
            let index = sender.tag() as usize;
            if index < cues.len() {
                cues.remove(index);
                if let Some(editor) = super::registry::get(self.ivars().editor) {
                    editor.set_caption_cues(cues);
                }
                self.build_rows();
            }
        }
    }
);

impl CaptionsWindow {
    fn new(mtm: MainThreadMarker, editor: u64) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            editor,
            window: OnceCell::new(),
            list: OnceCell::new(),
            fields: RefCell::new(Vec::new()),
        });
        // SAFETY: NSObject's designated initializer.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        this.build(mtm);
        this
    }

    fn cues(&self) -> Vec<Cue> {
        super::registry::get(self.ivars().editor)
            .map(|e| e.caption_cues())
            .unwrap_or_default()
    }

    fn build(&self, mtm: MainThreadMarker) {
        // SAFETY: designated NSWindow initializer.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::ZERO, NSSize::new(WIDTH, HEIGHT)),
                NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Closable
                    | NSWindowStyleMask::Miniaturizable
                    | NSWindowStyleMask::Resizable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: WINDOWS keeps the controller, which keeps the window.
        unsafe { window.setReleasedWhenClosed(false) };
        let name = super::registry::get(self.ivars().editor)
            .and_then(|e| e.window_title())
            .unwrap_or_default();
        window.setTitle(&NSString::from_str(&t!("Captions: {name}", name)));
        window.setContentMinSize(NSSize::new(420.0, 240.0));
        window.setDelegate(Some(ProtocolObject::from_ref(self)));
        let content = window.contentView().expect("windows have a content view");
        let scroll = NSScrollView::new(mtm);
        scroll.setFrame(NSRect::new(NSPoint::ZERO, NSSize::new(WIDTH, HEIGHT)));
        scroll.setHasVerticalScroller(true);
        scroll.setAutohidesScrollers(true);
        scroll.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        let list = CaptionList::new(mtm, NSRect::new(NSPoint::ZERO, NSSize::new(WIDTH, HEIGHT)));
        list.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
        scroll.setDocumentView(Some(&list));
        content.addSubview(&scroll);
        let _ = self.ivars().list.set(list);
        let _ = self.ivars().window.set(window.clone());
        window.center();
    }

    /// One row per caption: time, text, play, remove.
    fn build_rows(&self) {
        let mtm = self.mtm();
        let Some(list) = self.ivars().list.get() else {
            return;
        };
        for view in list.subviews().iter() {
            view.removeFromSuperview();
        }
        let cues = self.cues();
        let width = list.frame().size.width.max(420.0);
        list.setFrameSize(NSSize::new(width, ROW * cues.len() as f64 + 16.0));
        let target: &AnyObject = self.as_ref();
        let flexible = NSAutoresizingMaskOptions::ViewWidthSizable;
        let right = NSAutoresizingMaskOptions::ViewMinXMargin;
        let mut fields = Vec::new();
        for (i, cue) in cues.iter().enumerate() {
            let y = 8.0 + i as f64 * ROW;
            let time =
                NSTextField::labelWithString(&NSString::from_str(&format_time(cue.start)), mtm);
            time.setFont(Some(&NSFont::monospacedDigitSystemFontOfSize_weight(
                12.0, 0.0,
            )));
            time.setTextColor(Some(&NSColor::secondaryLabelColor()));
            time.setFrame(NSRect::new(
                NSPoint::new(12.0, y + 6.0),
                NSSize::new(64.0, 18.0),
            ));
            list.addSubview(&time);

            let field = NSTextField::new(mtm);
            field.setStringValue(&NSString::from_str(&cue.text));
            field.setFrame(NSRect::new(
                NSPoint::new(80.0, y + 3.0),
                NSSize::new(width - 80.0 - 76.0, 24.0),
            ));
            field.setAutoresizingMask(flexible);
            field.setTag(i as isize);
            field.setAccessibilityLabel(Some(&NSString::from_str(&t!(
                "Caption at {time}",
                time = format_time(cue.start)
            ))));
            if let Some(cell) = field.cell() {
                cell.setSendsActionOnEndEditing(true);
            }
            // SAFETY: the action exists on the controller, which outlives
            // the field.
            unsafe {
                field.setTarget(Some(target));
                field.setAction(Some(sel!(textEdited:)));
            }
            list.addSubview(&field);
            fields.push(field);

            let symbol = |name: &str, description: &str, action, x: f64| {
                // SAFETY: the action exists on the controller.
                let button = unsafe {
                    NSButton::buttonWithTitle_target_action(
                        &NSString::from_str(""),
                        Some(target),
                        Some(action),
                        mtm,
                    )
                };
                button.setImage(
                    NSImage::imageWithSystemSymbolName_accessibilityDescription(
                        &NSString::from_str(name),
                        Some(&NSString::from_str(&crate::i18n::tr(description))),
                    )
                    .as_deref(),
                );
                button.setBordered(false);
                button.setTag(i as isize);
                button.setToolTip(Some(&NSString::from_str(&crate::i18n::tr(description))));
                button.setFrame(NSRect::new(
                    NSPoint::new(x, y + 3.0),
                    NSSize::new(28.0, 24.0),
                ));
                button.setAutoresizingMask(right);
                list.addSubview(&button);
            };
            symbol("play.fill", "Play from here", sel!(play:), width - 68.0);
            symbol(
                "minus.circle",
                "Remove this caption",
                sel!(remove:),
                width - 36.0,
            );
        }
        *self.ivars().fields.borrow_mut() = fields;
    }

    /// Reads the text fields into the captions.
    fn apply_fields(&self) {
        let Some(editor) = super::registry::get(self.ivars().editor) else {
            return;
        };
        let mut cues = editor.caption_cues();
        let mut changed = false;
        for field in self.ivars().fields.borrow().iter() {
            let index = field.tag() as usize;
            let text = field.stringValue().to_string().trim().to_string();
            if let Some(cue) = cues.get_mut(index)
                && cue.text != text
                && !text.is_empty()
            {
                cue.text = text;
                changed = true;
            }
        }
        if changed {
            editor.set_caption_cues(cues);
        }
    }
}

/// `m:ss.s`.
fn format_time(t: f64) -> String {
    let tenths = (t.max(0.0) * 10.0).round() as u64;
    format!("{}:{:02}.{}", tenths / 600, tenths / 10 % 60, tenths % 10)
}

define_class!(
    /// The rows, top to bottom.
    // SAFETY: NSView can be subclassed; no Drop implementation.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    struct CaptionList;

    impl CaptionList {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

impl CaptionList {
    fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSView's designated initializer.
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn formats_times() {
        assert_eq!(super::format_time(0.0), "0:00.0");
        assert_eq!(super::format_time(83.46), "1:23.5");
    }
}
