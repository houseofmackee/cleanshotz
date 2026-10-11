//! The guide window: the captured steps in order, each with its
//! screenshot and an editable caption, a title, and Export (Markdown,
//! HTML or PDF).

use std::cell::{Cell, OnceCell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, ProtocolObject};
use objc2::{
    AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel,
};
use objc2_app_kit::{
    NSAccessibility, NSAlert, NSAlertFirstButtonReturn, NSAutoresizingMaskOptions,
    NSBackingStoreType, NSBezelStyle, NSButton, NSControlTextEditingDelegate, NSFont,
    NSFontWeightSemibold, NSImage, NSImageScaling, NSImageView, NSPopUpButton, NSScrollView,
    NSTextField, NSTextFieldDelegate, NSView, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{NSNotification, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

use super::Step;
use super::export::Format;

const WIDTH: f64 = 780.0;
const HEIGHT: f64 = 680.0;
const TOP: f64 = 60.0;
const ROW: f64 = 232.0;
const IMAGE: NSSize = NSSize {
    width: 330.0,
    height: 206.0,
};

pub struct Ivars {
    window: OnceCell<Retained<NSWindow>>,
    title: OnceCell<Retained<NSTextField>>,
    list: OnceCell<Retained<GuideList>>,
    steps: RefCell<Vec<Step>>,
    captions: RefCell<Vec<Retained<NSTextField>>>,
    /// Exported at least once since the last change: closing does not ask.
    exported: Cell<bool>,
}

thread_local! {
    static GUIDES: RefCell<Vec<Retained<Guide>>> = const { RefCell::new(Vec::new()) };
}

/// Opens a guide window for `steps`.
pub fn open(mtm: MainThreadMarker, steps: Vec<Step>) {
    let guide = Guide::new(mtm, steps);
    if let Some(window) = guide.ivars().window.get() {
        crate::ui::bring_forward(mtm, window);
    }
    GUIDES.with(|g| g.borrow_mut().push(guide));
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    struct Guide;

    unsafe impl NSObjectProtocol for Guide {}

    unsafe impl NSWindowDelegate for Guide {
        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, _sender: &NSWindow) -> bool {
            self.ivars().exported.get()
                || crate::editor::skip_confirmations()
                || self.confirm_discard()
        }

        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            // Released once the current event is done with it.
            let this = self as *const Guide as usize;
            dispatch2::DispatchQueue::main().exec_async(move || {
                GUIDES.with(|g| {
                    g.borrow_mut()
                        .retain(|c| &**c as *const Guide as usize != this)
                });
            });
        }
    }

    impl Guide {
        #[unsafe(method(export:))]
        fn export_action(&self, sender: &NSPopUpButton) {
            let format = match sender.indexOfSelectedItem() {
                1 => Format::Markdown,
                2 => Format::Html,
                3 => Format::Pdf,
                _ => return,
            };
            self.export(format);
        }

        #[unsafe(method(remove:))]
        fn remove_action(&self, sender: &NSButton) {
            self.sync_captions();
            let index = sender.tag() as usize;
            {
                let mut steps = self.ivars().steps.borrow_mut();
                if index < steps.len() && steps.len() > 1 {
                    steps.remove(index);
                }
            }
            self.ivars().exported.set(false);
            self.build_rows();
        }

    }

    unsafe impl NSControlTextEditingDelegate for Guide {
        #[unsafe(method(controlTextDidChange:))]
        fn text_changed(&self, _notification: &NSNotification) {
            self.ivars().exported.set(false);
        }
    }

    unsafe impl NSTextFieldDelegate for Guide {}
);

impl Guide {
    fn new(mtm: MainThreadMarker, steps: Vec<Step>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            window: OnceCell::new(),
            title: OnceCell::new(),
            list: OnceCell::new(),
            steps: RefCell::new(steps),
            captions: RefCell::new(Vec::new()),
            exported: Cell::new(false),
        });
        // SAFETY: NSObject's designated initializer.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        this.build(mtm);
        this
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
        // SAFETY: GUIDES keeps the controller, which keeps the window.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&NSString::from_str(&t!("Step Guide")));
        window.setContentMinSize(NSSize::new(WIDTH, 360.0));
        window.setDelegate(Some(ProtocolObject::from_ref(self)));
        let content = window.contentView().expect("windows have a content view");
        let target: &AnyObject = self.as_ref();
        let top = NSAutoresizingMaskOptions::ViewMinYMargin;

        let title = NSTextField::new(mtm);
        title.setStringValue(&NSString::from_str(&t!("How to …")));
        title.setPlaceholderString(Some(&NSString::from_str(&t!("Title"))));
        // SAFETY: reading an AppKit constant.
        title.setFont(Some(&NSFont::systemFontOfSize_weight(18.0, unsafe {
            NSFontWeightSemibold
        })));
        title.setFrame(NSRect::new(
            NSPoint::new(16.0, HEIGHT - 46.0),
            NSSize::new(WIDTH - 180.0, 30.0),
        ));
        title.setAutoresizingMask(top | NSAutoresizingMaskOptions::ViewWidthSizable);
        title.setAccessibilityLabel(Some(&NSString::from_str(&t!("Title"))));
        // SAFETY: the delegate methods exist on Guide, which outlives the field.
        unsafe { title.setDelegate(Some(ProtocolObject::from_ref(self))) };
        content.addSubview(&title);

        let export = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(mtm),
            NSRect::new(
                NSPoint::new(WIDTH - 148.0, HEIGHT - 46.0),
                NSSize::new(132.0, 30.0),
            ),
            true,
        );
        for item in [t!("Export"), t!("Markdown…"), t!("HTML…"), t!("PDF…")] {
            export.addItemWithTitle(&NSString::from_str(&item));
        }
        // SAFETY: the action exists on Guide.
        unsafe {
            export.setTarget(Some(target));
            export.setAction(Some(sel!(export:)));
        }
        export.setAutoresizingMask(top | NSAutoresizingMaskOptions::ViewMinXMargin);
        content.addSubview(&export);

        let scroll = NSScrollView::new(mtm);
        scroll.setFrame(NSRect::new(NSPoint::ZERO, NSSize::new(WIDTH, HEIGHT - TOP)));
        scroll.setHasVerticalScroller(true);
        scroll.setAutohidesScrollers(true);
        scroll.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        let list = GuideList::new(
            mtm,
            NSRect::new(NSPoint::ZERO, NSSize::new(WIDTH, HEIGHT - TOP)),
        );
        list.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
        scroll.setDocumentView(Some(&list));
        content.addSubview(&scroll);

        let _ = self.ivars().title.set(title.clone());
        let _ = self.ivars().list.set(list);
        let _ = self.ivars().window.set(window.clone());
        self.build_rows();
        window.center();
        window.makeFirstResponder(Some(&title));
        // Select the placeholder title so typing replaces it.
        // SAFETY: the field is in the window.
        unsafe { title.selectText(None) };
    }

    /// Lays out one row per step: screenshot, number, caption, Remove.
    fn build_rows(&self) {
        let mtm = self.mtm();
        let Some(list) = self.ivars().list.get() else {
            return;
        };
        for view in list.subviews().iter() {
            view.removeFromSuperview();
        }
        let steps = self.ivars().steps.borrow();
        let width = list.frame().size.width.max(WIDTH);
        list.setFrameSize(NSSize::new(width, ROW * steps.len() as f64 + 16.0));
        let target: &AnyObject = self.as_ref();
        let mut captions = Vec::new();
        let text_x = 16.0 + IMAGE.width + 20.0;
        let text_width = width - text_x - 20.0;
        let flexible = NSAutoresizingMaskOptions::ViewWidthSizable;
        for (i, step) in steps.iter().enumerate() {
            let y = 16.0 + i as f64 * ROW;
            let label = t!("Step {n}", n = i + 1);
            let (w, h) = crate::capture::image::pixel_size(&step.image);
            let image = NSImage::initWithCGImage_size(
                NSImage::alloc(),
                &step.image,
                NSSize::new(w as f64 / step.scale, h as f64 / step.scale),
            );
            let view = NSImageView::imageViewWithImage(&image, mtm);
            view.setImageScaling(NSImageScaling::ScaleProportionallyDown);
            view.setFrame(NSRect::new(NSPoint::new(16.0, y), IMAGE));
            view.setAccessibilityLabel(Some(&NSString::from_str(&t!(
                "Screenshot of {label}",
                label
            ))));
            list.addSubview(&view);

            let heading = NSTextField::labelWithString(&NSString::from_str(&label), mtm);
            // SAFETY: reading an AppKit constant.
            heading.setFont(Some(&NSFont::systemFontOfSize_weight(14.0, unsafe {
                NSFontWeightSemibold
            })));
            heading.setFrame(NSRect::new(
                NSPoint::new(text_x, y),
                NSSize::new(text_width, 20.0),
            ));
            heading.setAutoresizingMask(flexible);
            list.addSubview(&heading);

            let caption =
                NSTextField::wrappingLabelWithString(&NSString::from_str(&step.caption), mtm);
            caption.setEditable(true);
            caption.setSelectable(true);
            caption.setBezeled(true);
            caption.setDrawsBackground(true);
            caption.setFont(Some(&NSFont::systemFontOfSize(13.0)));
            caption.setFrame(NSRect::new(
                NSPoint::new(text_x, y + 26.0),
                NSSize::new(text_width, 120.0),
            ));
            caption.setAutoresizingMask(flexible);
            caption.setAccessibilityLabel(Some(&NSString::from_str(&t!(
                "Caption for {label}",
                label
            ))));
            // SAFETY: the delegate methods exist on Guide, which outlives
            // the field.
            unsafe { caption.setDelegate(Some(ProtocolObject::from_ref(self))) };
            list.addSubview(&caption);
            captions.push(caption);

            // SAFETY: the action exists on Guide.
            let remove = unsafe {
                NSButton::buttonWithTitle_target_action(
                    &NSString::from_str(&t!("Remove Step")),
                    Some(target),
                    Some(sel!(remove:)),
                    mtm,
                )
            };
            remove.setBezelStyle(NSBezelStyle::Push);
            remove.setTag(i as isize);
            remove.setEnabled(steps.len() > 1);
            remove.setFrame(NSRect::new(
                NSPoint::new(text_x - 6.0, y + 154.0),
                NSSize::new(130.0, 30.0),
            ));
            list.addSubview(&remove);
        }
        *self.ivars().captions.borrow_mut() = captions;
    }

    /// Copies the caption fields into the steps.
    fn sync_captions(&self) {
        let captions = self.ivars().captions.borrow();
        for (step, field) in self
            .ivars()
            .steps
            .borrow_mut()
            .iter_mut()
            .zip(captions.iter())
        {
            step.caption = field.stringValue().to_string();
        }
    }

    fn export(&self, format: Format) {
        self.sync_captions();
        let title = self
            .ivars()
            .title
            .get()
            .map(|t| t.stringValue().to_string())
            .unwrap_or_default();
        let steps = self.ivars().steps.borrow();
        if super::export::save(self.mtm(), format, &title, &steps) {
            self.ivars().exported.set(true);
        }
    }

    fn confirm_discard(&self) -> bool {
        let alert = NSAlert::new(self.mtm());
        alert.setMessageText(&NSString::from_str(&t!("Discard this guide?")));
        alert.setInformativeText(&NSString::from_str(&t!(
            "It has not been exported. Its steps are not saved anywhere else."
        )));
        alert.addButtonWithTitle(&NSString::from_str(&t!("Discard")));
        alert.addButtonWithTitle(&NSString::from_str(&t!("Cancel")));
        alert.runModal() == NSAlertFirstButtonReturn
    }
}

define_class!(
    /// The rows, top to bottom.
    // SAFETY: NSView can be subclassed; no Drop implementation.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    struct GuideList;

    impl GuideList {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

impl GuideList {
    fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSView's designated initializer.
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }
}
