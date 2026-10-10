//! The editor window: a toolbar of tools and actions, a contextual options
//! bar, and the canvas.

use std::cell::{OnceCell, RefCell};
use std::path::PathBuf;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSApplication, NSAutoresizingMaskOptions, NSBackingStoreType, NSBezelStyle, NSButton,
    NSColor, NSColorSpace, NSColorWell, NSColorWellStyle, NSControlStateValueOff,
    NSControlStateValueOn, NSControlTextEditingDelegate, NSCursor, NSEvent, NSEventModifierFlags,
    NSFocusRingType, NSGraphicsContext, NSImage, NSModalResponseOK, NSPopUpButton, NSResponder,
    NSSavePanel, NSSegmentedControl, NSSlider, NSTextField, NSTextFieldDelegate, NSView, NSWindow,
    NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    NSAffineTransform, NSNotification, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
    ns_string,
};

use super::model::{Annotation, ArrowStyle, Aspect, Background, Color, Fill, Shape, TextStyle};
use super::render::{self, BaseImage};
use super::session::{Effect, Session};
use super::tools::Tool;
use super::{Source, project, registry};
use crate::capture::geometry::{Point, Rect};
use crate::capture::output;
use crate::config::ImageFormat;
use crate::ui::draw;

const TOOLBAR_HEIGHT: f64 = 48.0;
const OPTIONS_HEIGHT: f64 = 40.0;
const CANVAS_MARGIN: f64 = 24.0;

/// Background presets offered in the options bar.
const BACKGROUNDS: &[(&str, Option<Fill>)] = &[
    ("No Background", None),
    (
        "Sunset",
        Some(Fill::Gradient {
            from: Color::rgb(0.98, 0.45, 0.42),
            to: Color::rgb(0.55, 0.36, 0.96),
        }),
    ),
    (
        "Ocean",
        Some(Fill::Gradient {
            from: Color::rgb(0.15, 0.55, 0.95),
            to: Color::rgb(0.2, 0.85, 0.75),
        }),
    ),
    (
        "Forest",
        Some(Fill::Gradient {
            from: Color::rgb(0.18, 0.55, 0.33),
            to: Color::rgb(0.75, 0.85, 0.3),
        }),
    ),
    (
        "Peach",
        Some(Fill::Gradient {
            from: Color::rgb(1.0, 0.78, 0.55),
            to: Color::rgb(0.98, 0.45, 0.55),
        }),
    ),
    (
        "Midnight",
        Some(Fill::Gradient {
            from: Color::rgb(0.1, 0.1, 0.25),
            to: Color::rgb(0.32, 0.16, 0.46),
        }),
    ),
    (
        "White",
        Some(Fill::Solid {
            color: Color::WHITE,
        }),
    ),
    (
        "Light Gray",
        Some(Fill::Solid {
            color: Color::rgb(0.92, 0.92, 0.93),
        }),
    ),
    (
        "Black",
        Some(Fill::Solid {
            color: Color::BLACK,
        }),
    ),
];

const ASPECTS: &[(&str, Aspect)] = &[
    ("Auto", Aspect::Auto),
    ("1:1", Aspect::Square),
    ("16:9", Aspect::Wide),
    ("4:3", Aspect::Standard),
];

const MORE_SAVE_AS: isize = 1;
const MORE_SAVE_PROJECT: isize = 2;
const MORE_PIN: isize = 3;
const MORE_COPY_TEXT: isize = 4;
const MORE_REDACT: isize = 5;

struct Ui {
    tools: Retained<NSSegmentedControl>,
    color: Retained<NSColorWell>,
    width: Retained<NSSlider>,
    style: Retained<NSPopUpButton>,
    background: Retained<NSPopUpButton>,
    padding: Retained<NSSlider>,
    radius: Retained<NSSlider>,
    shadow: Retained<NSButton>,
    aspect: Retained<NSPopUpButton>,
    undo: Retained<NSButton>,
    redo: Retained<NSButton>,
    drawing_group: Vec<Retained<NSView>>,
    background_group: Vec<Retained<NSView>>,
}

struct TextEdit {
    field: Retained<NSTextField>,
    origin: Point,
    existing: Option<u64>,
}

pub struct EditorIvars {
    pub id: u64,
    base: BaseImage,
    session: RefCell<Session>,
    source: RefCell<Source>,
    window: OnceCell<Retained<NSWindow>>,
    canvas: OnceCell<Retained<CanvasView>>,
    ui: OnceCell<Ui>,
    text: RefCell<Option<TextEdit>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = EditorIvars]
    pub struct EditorController;

    unsafe impl NSObjectProtocol for EditorController {}

    unsafe impl NSWindowDelegate for EditorController {
        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, _sender: &NSWindow) -> bool {
            self.commit_text();
            !self.ivars().session.borrow().dirty || self.confirm_discard()
        }

        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            registry::remove(self.ivars().id);
        }
    }

    unsafe impl NSControlTextEditingDelegate for EditorController {
        #[unsafe(method(controlTextDidChange:))]
        fn control_text_did_change(&self, _notification: &NSNotification) {
            self.fit_text_field();
        }

        #[unsafe(method(controlTextDidEndEditing:))]
        fn control_text_did_end_editing(&self, _notification: &NSNotification) {
            self.commit_text();
        }
    }

    unsafe impl NSTextFieldDelegate for EditorController {}

    impl EditorController {
        #[unsafe(method(toolChanged:))]
        fn tool_changed(&self, sender: &NSSegmentedControl) {
            if let Some(tool) = Tool::ALL.get(sender.selectedSegment() as usize) {
                self.set_tool(*tool);
            }
        }

        #[unsafe(method(colorChanged:))]
        fn color_changed(&self, sender: &NSColorWell) {
            let color = to_color(&sender.color());
            self.ivars().session.borrow_mut().set_color(color);
            if let Some(edit) = self.ivars().text.borrow().as_ref() {
                edit.field.setTextColor(Some(&render::ns_color(color)));
            }
            self.refresh();
        }

        #[unsafe(method(widthChanged:))]
        fn width_changed(&self, sender: &NSSlider) {
            let value = sender.doubleValue().round();
            let text_mode = self.text_size_mode();
            {
                let mut session = self.ivars().session.borrow_mut();
                if text_mode {
                    session.text_size = value;
                    if let Some(id) = session.selection {
                        resize_text(&mut session, id, value);
                    }
                } else {
                    session.set_width(value);
                }
            }
            self.refresh();
        }

        #[unsafe(method(styleChanged:))]
        fn style_changed(&self, sender: &NSPopUpButton) {
            let index = sender.indexOfSelectedItem();
            {
                let mut session = self.ivars().session.borrow_mut();
                match session.tool {
                    Tool::Arrow => session.set_arrow_style(match index {
                        1 => ArrowStyle::Open,
                        2 => ArrowStyle::Double,
                        _ => ArrowStyle::Filled,
                    }),
                    Tool::Rectangle | Tool::Ellipse => session.set_filled(index == 1),
                    Tool::Text => {
                        let style = if index == 1 {
                            TextStyle::Background
                        } else {
                            TextStyle::Plain
                        };
                        session.text_style = style;
                        if let Some(id) = session.selection {
                            restyle_text(&mut session, id, style);
                        }
                    }
                    _ => {}
                }
            }
            self.refresh();
        }

        #[unsafe(method(backgroundChanged:))]
        fn background_changed(&self, _sender: &AnyObject) {
            let Some(ui) = self.ivars().ui.get() else {
                return;
            };
            let preset = ui.background.indexOfSelectedItem().max(0) as usize;
            let background = BACKGROUNDS.get(preset).and_then(|(_, fill)| *fill).map(|fill| {
                Background {
                    fill,
                    padding: ui.padding.doubleValue().round(),
                    corner_radius: ui.radius.doubleValue().round(),
                    shadow: ui.shadow.state() == NSControlStateValueOn,
                    aspect: ASPECTS
                        .get(ui.aspect.indexOfSelectedItem().max(0) as usize)
                        .map_or(Aspect::Auto, |(_, a)| *a),
                }
            });
            self.ivars().session.borrow_mut().set_background(background);
            self.refresh();
        }

        #[unsafe(method(undoEdit:))]
        fn undo_edit(&self, _sender: Option<&AnyObject>) {
            self.undo();
        }

        #[unsafe(method(redoEdit:))]
        fn redo_edit(&self, _sender: Option<&AnyObject>) {
            self.redo();
        }

        #[unsafe(method(copyImage:))]
        fn copy_image(&self, _sender: Option<&AnyObject>) {
            self.copy_to_clipboard();
        }

        #[unsafe(method(saveImage:))]
        fn save_image(&self, _sender: Option<&AnyObject>) {
            self.save();
        }

        #[unsafe(method(moreAction:))]
        fn more_action(&self, sender: &NSPopUpButton) {
            match sender.indexOfSelectedItem() {
                MORE_SAVE_AS => self.save_as(),
                MORE_SAVE_PROJECT => self.save_project(),
                MORE_PIN => self.pin(),
                MORE_COPY_TEXT => self.copy_text(),
                MORE_REDACT => self.redact(),
                _ => {}
            }
        }

        #[unsafe(method(textCommitted:))]
        fn text_committed(&self, _sender: &NSTextField) {
            self.commit_text();
            if let (Some(window), Some(canvas)) = (self.window(), self.ivars().canvas.get()) {
                window.makeFirstResponder(Some(canvas));
            }
        }
    }
);

impl EditorController {
    pub fn open(
        mtm: MainThreadMarker,
        id: u64,
        base: BaseImage,
        session: Session,
        source: Source,
        title: &str,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(EditorIvars {
            id,
            base,
            session: RefCell::new(session),
            source: RefCell::new(source),
            window: OnceCell::new(),
            canvas: OnceCell::new(),
            ui: OnceCell::new(),
            text: RefCell::new(None),
        });
        // SAFETY: NSObject's designated initializer.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        this.build_window(mtm, title);
        this
    }

    pub fn is_dirty(&self) -> bool {
        self.commit_text();
        self.ivars().session.borrow().dirty
    }

    fn confirm_discard(&self) -> bool {
        if super::skip_confirmations() {
            return true;
        }
        let alert = NSAlert::new(self.mtm());
        alert.setMessageText(ns_string!("Discard your changes?"));
        alert.setInformativeText(ns_string!("The annotations have not been saved or copied."));
        alert.addButtonWithTitle(ns_string!("Discard"));
        alert.addButtonWithTitle(ns_string!("Cancel"));
        alert.runModal() == objc2_app_kit::NSAlertFirstButtonReturn
    }

    pub fn undo(&self) {
        self.commit_text();
        self.ivars().session.borrow_mut().undo();
        self.refresh();
    }

    pub fn redo(&self) {
        self.commit_text();
        self.ivars().session.borrow_mut().redo();
        self.refresh();
    }

    fn window(&self) -> Option<Retained<NSWindow>> {
        self.ivars().window.get().cloned()
    }

    fn build_window(&self, mtm: MainThreadMarker, title: &str) {
        let layout = self.ivars().base.layout(&self.ivars().session.borrow().doc);
        // Fit the image, within most of the screen.
        let visible = objc2_app_kit::NSScreen::mainScreen(mtm).map_or(
            NSRect::new(NSPoint::ZERO, NSSize::new(1440.0, 900.0)),
            |s| s.visibleFrame(),
        );
        let chrome = TOOLBAR_HEIGHT + OPTIONS_HEIGHT;
        let width = (layout.width + 2.0 * CANVAS_MARGIN).clamp(860.0, visible.size.width * 0.9);
        let height =
            (layout.height + 2.0 * CANVAS_MARGIN + chrome).clamp(520.0, visible.size.height * 0.9);
        let frame = NSRect::new(NSPoint::ZERO, NSSize::new(width, height));

        // SAFETY: designated NSWindow initializer.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Closable
                    | NSWindowStyleMask::Miniaturizable
                    | NSWindowStyleMask::Resizable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: the registry keeps the controller (and so the window) alive.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&NSString::from_str(title));
        window.setContentMinSize(NSSize::new(860.0, 420.0));
        window.setDelegate(Some(ProtocolObject::from_ref(self)));
        let content = window.contentView().expect("windows have a content view");

        let canvas = CanvasView::new(
            mtm,
            self.ivars().id,
            NSRect::new(NSPoint::ZERO, NSSize::new(width, height - chrome)),
        );
        // Views do not clip their drawing by default since macOS 14.
        canvas.setClipsToBounds(true);
        canvas.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        content.addSubview(&canvas);

        let ui = self.build_bars(mtm, &content, width, height);
        let _ = self.ivars().ui.set(ui);
        let _ = self.ivars().canvas.set(canvas.clone());
        let _ = self.ivars().window.set(window.clone());
        self.sync_controls();

        window.center();
        NSApplication::sharedApplication(mtm).activate();
        window.makeKeyAndOrderFront(None);
        // Front even when macOS declines to activate the app (opened from
        // the command line while the terminal is active).
        window.orderFrontRegardless();
        window.makeFirstResponder(Some(&canvas));
    }

    fn build_bars(&self, mtm: MainThreadMarker, content: &NSView, width: f64, height: f64) -> Ui {
        let target: &AnyObject = self.as_ref();
        let top = NSAutoresizingMaskOptions::ViewMinYMargin;
        let right = NSAutoresizingMaskOptions::ViewMinXMargin | top;
        let add = |view: &NSView, frame: NSRect, mask: NSAutoresizingMaskOptions| {
            view.setFrame(frame);
            view.setAutoresizingMask(mask);
            content.addSubview(view);
        };
        let row1 = height - TOOLBAR_HEIGHT + 10.0;
        let row2 = height - TOOLBAR_HEIGHT - OPTIONS_HEIGHT + 8.0;

        // Tools.
        let tools = NSSegmentedControl::new(mtm);
        tools.setSegmentCount(Tool::ALL.len() as isize);
        for (i, tool) in Tool::ALL.iter().enumerate() {
            let i = i as isize;
            let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
                &NSString::from_str(tool.symbol()),
                Some(&NSString::from_str(tool.title())),
            );
            match image {
                Some(image) => tools.setImage_forSegment(Some(&image), i),
                None => tools.setLabel_forSegment(&NSString::from_str(&tool.title()[..1]), i),
            }
            tools.setWidth_forSegment(32.0, i);
            tools.setToolTip_forSegment(
                Some(&NSString::from_str(&format!(
                    "{} ({})",
                    tool.title(),
                    tool.shortcut().to_ascii_uppercase()
                ))),
                i,
            );
        }
        // SAFETY: `toolChanged:` exists on the controller, which outlives its controls.
        unsafe {
            tools.setTarget(Some(target));
            tools.setAction(Some(sel!(toolChanged:)));
        }
        add(
            &tools,
            NSRect::new(
                NSPoint::new(12.0, row1),
                NSSize::new(Tool::ALL.len() as f64 * 34.0, 28.0),
            ),
            top,
        );

        // Actions on the right.
        let button = |title: &str, action| {
            // SAFETY: the action exists on the controller.
            unsafe {
                NSButton::buttonWithTitle_target_action(
                    &NSString::from_str(title),
                    Some(target),
                    Some(action),
                    mtm,
                )
            }
        };
        let symbol_button = |symbol: &str, label: &str, action| {
            let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
                &NSString::from_str(symbol),
                Some(&NSString::from_str(label)),
            )
            .unwrap_or_default();
            // SAFETY: the action exists on the controller.
            let b = unsafe {
                NSButton::buttonWithImage_target_action(&image, Some(target), Some(action), mtm)
            };
            b.setBezelStyle(NSBezelStyle::Push);
            b.setToolTip(Some(&NSString::from_str(label)));
            b
        };
        let save = button("Save", sel!(saveImage:));
        add(
            &save,
            NSRect::new(
                NSPoint::new(width - 84.0, row1 - 2.0),
                NSSize::new(72.0, 30.0),
            ),
            right,
        );
        let copy = button("Copy", sel!(copyImage:));
        add(
            &copy,
            NSRect::new(
                NSPoint::new(width - 160.0, row1 - 2.0),
                NSSize::new(72.0, 30.0),
            ),
            right,
        );
        let more = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(mtm),
            NSRect::new(NSPoint::ZERO, NSSize::new(54.0, 28.0)),
            true,
        );
        for title in [
            "•••",
            "Save As…",
            "Save Project…",
            "Pin to Screen",
            "Copy Text",
            "Redact Sensitive Info",
        ] {
            more.addItemWithTitle(&NSString::from_str(title));
        }
        // SAFETY: the action exists on the controller.
        unsafe {
            more.setTarget(Some(target));
            more.setAction(Some(sel!(moreAction:)));
        }
        add(
            &more,
            NSRect::new(
                NSPoint::new(width - 222.0, row1 - 1.0),
                NSSize::new(56.0, 28.0),
            ),
            right,
        );
        let redo = symbol_button(
            "arrow.uturn.forward",
            "Redo (Shift-Command-Z)",
            sel!(redoEdit:),
        );
        add(
            &redo,
            NSRect::new(
                NSPoint::new(width - 266.0, row1 - 2.0),
                NSSize::new(40.0, 30.0),
            ),
            right,
        );
        let undo = symbol_button("arrow.uturn.backward", "Undo (Command-Z)", sel!(undoEdit:));
        add(
            &undo,
            NSRect::new(
                NSPoint::new(width - 308.0, row1 - 2.0),
                NSSize::new(40.0, 30.0),
            ),
            right,
        );

        // Drawing options: color, width (or text size), style.
        let color = NSColorWell::colorWellWithStyle(NSColorWellStyle::Expanded, mtm);
        // SAFETY: the action exists on the controller.
        unsafe {
            color.setTarget(Some(target));
            color.setAction(Some(sel!(colorChanged:)));
        }
        add(
            &color,
            NSRect::new(NSPoint::new(12.0, row2), NSSize::new(64.0, 24.0)),
            top,
        );
        // SAFETY: the action exists on the controller.
        let width_slider = unsafe {
            NSSlider::sliderWithValue_minValue_maxValue_target_action(
                4.0,
                1.0,
                20.0,
                Some(target),
                Some(sel!(widthChanged:)),
                mtm,
            )
        };
        width_slider.setContinuous(true);
        add(
            &width_slider,
            NSRect::new(NSPoint::new(88.0, row2), NSSize::new(140.0, 24.0)),
            top,
        );
        let style = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(mtm),
            NSRect::new(NSPoint::ZERO, NSSize::new(140.0, 26.0)),
            false,
        );
        // SAFETY: the action exists on the controller.
        unsafe {
            style.setTarget(Some(target));
            style.setAction(Some(sel!(styleChanged:)));
        }
        add(
            &style,
            NSRect::new(NSPoint::new(240.0, row2 - 1.0), NSSize::new(150.0, 26.0)),
            top,
        );

        // Background options.
        let popup = |items: &[&str], x: f64, w: f64| {
            let p = NSPopUpButton::initWithFrame_pullsDown(
                NSPopUpButton::alloc(mtm),
                NSRect::new(NSPoint::ZERO, NSSize::new(w, 26.0)),
                false,
            );
            for item in items {
                p.addItemWithTitle(&NSString::from_str(item));
            }
            // SAFETY: the action exists on the controller.
            unsafe {
                p.setTarget(Some(target));
                p.setAction(Some(sel!(backgroundChanged:)));
            }
            add(
                &p,
                NSRect::new(NSPoint::new(x, row2 - 1.0), NSSize::new(w, 26.0)),
                top,
            );
            p
        };
        let background = popup(
            &BACKGROUNDS.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            12.0,
            150.0,
        );
        let label = |text: &str, x: f64| {
            let l = NSTextField::labelWithString(&NSString::from_str(text), mtm);
            add(
                &l,
                NSRect::new(NSPoint::new(x, row2 + 3.0), NSSize::new(60.0, 18.0)),
                top,
            );
            l
        };
        let slider = |value: f64, max: f64, x: f64| {
            // SAFETY: the action exists on the controller.
            let s = unsafe {
                NSSlider::sliderWithValue_minValue_maxValue_target_action(
                    value,
                    0.0,
                    max,
                    Some(target),
                    Some(sel!(backgroundChanged:)),
                    mtm,
                )
            };
            s.setContinuous(true);
            add(
                &s,
                NSRect::new(NSPoint::new(x, row2), NSSize::new(110.0, 24.0)),
                top,
            );
            s
        };
        let padding_label = label("Padding", 172.0);
        let padding = slider(64.0, 200.0, 228.0);
        let radius_label = label("Corners", 350.0);
        let radius = slider(12.0, 40.0, 410.0);
        // SAFETY: the action exists on the controller.
        let shadow = unsafe {
            NSButton::checkboxWithTitle_target_action(
                ns_string!("Shadow"),
                Some(target),
                Some(sel!(backgroundChanged:)),
                mtm,
            )
        };
        add(
            &shadow,
            NSRect::new(NSPoint::new(530.0, row2 + 2.0), NSSize::new(80.0, 20.0)),
            top,
        );
        let aspect = popup(
            &ASPECTS.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            614.0,
            90.0,
        );

        let as_view = |v: &NSView| v.retain();
        Ui {
            drawing_group: vec![as_view(&color), as_view(&width_slider), as_view(&style)],
            background_group: vec![
                as_view(&background),
                as_view(&padding_label),
                as_view(&padding),
                as_view(&radius_label),
                as_view(&radius),
                as_view(&shadow),
                as_view(&aspect),
            ],
            tools,
            color,
            width: width_slider,
            style,
            background,
            padding,
            radius,
            shadow,
            aspect,
            undo,
            redo,
        }
    }

    /// Whether the width slider currently controls text size.
    fn text_size_mode(&self) -> bool {
        let session = self.ivars().session.borrow();
        session.tool == Tool::Text
            || session
                .selection
                .and_then(|id| session.doc.get(id))
                .is_some_and(|a| matches!(a.shape, Shape::Text { .. }))
    }

    /// Updates the controls from the session.
    fn sync_controls(&self) {
        let Some(ui) = self.ivars().ui.get() else {
            return;
        };
        let text_mode = self.text_size_mode();
        let session = self.ivars().session.borrow();
        ui.tools.setSelectedSegment(session.tool.index() as isize);
        let selected: Option<&Annotation> = session.selection.and_then(|id| session.doc.get(id));

        let background_tool = session.tool == Tool::Background;
        for v in &ui.drawing_group {
            v.setHidden(background_tool);
        }
        for v in &ui.background_group {
            v.setHidden(!background_tool);
        }
        let shows_color = session.tool.uses_color()
            || selected.is_some_and(|a| {
                !matches!(
                    a.shape,
                    Shape::Pixelate { .. } | Shape::Blur { .. } | Shape::Spotlight { .. }
                )
            });
        ui.color.setEnabled(shows_color);
        ui.width.setEnabled(shows_color);
        let color = selected.map_or(session.color, |a| a.color);
        ui.color.setColor(&render::ns_color(color));
        if text_mode {
            ui.width.setMinValue(12.0);
            ui.width.setMaxValue(96.0);
            let size = match selected.map(|a| &a.shape) {
                Some(Shape::Text { size, .. }) => *size,
                _ => session.text_size,
            };
            ui.width.setDoubleValue(size);
        } else {
            ui.width.setMinValue(1.0);
            ui.width.setMaxValue(20.0);
            ui.width
                .setDoubleValue(selected.map_or(session.width, |a| a.width));
        }

        let tool = session.tool;
        let (items, index): (&[&str], isize) = match tool {
            Tool::Arrow => (
                &["Filled Arrow", "Open Arrow", "Double Arrow"],
                match session.arrow_style {
                    ArrowStyle::Filled => 0,
                    ArrowStyle::Open => 1,
                    ArrowStyle::Double => 2,
                },
            ),
            Tool::Rectangle | Tool::Ellipse => (&["Outline", "Filled"], session.filled as isize),
            Tool::Text => (
                &["Plain Text", "Text on Color"],
                (session.text_style == TextStyle::Background) as isize,
            ),
            _ => (&[], 0),
        };
        ui.style.removeAllItems();
        for item in items {
            ui.style.addItemWithTitle(&NSString::from_str(item));
        }
        ui.style.selectItemAtIndex(index);
        ui.style.setHidden(items.is_empty() || background_tool);

        let bg = session.doc.background;
        let preset = bg.map_or(0, |b| {
            BACKGROUNDS
                .iter()
                .position(|(_, f)| *f == Some(b.fill))
                .unwrap_or(1)
        });
        ui.background.selectItemAtIndex(preset as isize);
        let defaults = bg.unwrap_or_default();
        ui.padding.setDoubleValue(defaults.padding);
        ui.radius.setDoubleValue(defaults.corner_radius);
        ui.shadow.setState(if defaults.shadow {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });
        ui.aspect.selectItemAtIndex(
            ASPECTS
                .iter()
                .position(|(_, a)| *a == defaults.aspect)
                .unwrap_or(0) as isize,
        );

        ui.undo.setEnabled(session.history.can_undo());
        ui.redo.setEnabled(session.history.can_redo());
    }

    /// Redraws the canvas and updates the controls.
    pub fn refresh(&self) {
        if let Some(canvas) = self.ivars().canvas.get() {
            canvas.setNeedsDisplay(true);
        }
        self.sync_controls();
    }

    pub fn set_tool(&self, tool: Tool) {
        self.commit_text();
        self.ivars().session.borrow_mut().set_tool(tool);
        if let Some(canvas) = self.ivars().canvas.get()
            && let Some(window) = canvas.window()
        {
            window.invalidateCursorRectsForView(canvas);
        }
        self.refresh();
    }

    fn begin_text(&self, at: Point, existing: Option<u64>) {
        self.commit_text();
        let Some(canvas) = self.ivars().canvas.get() else {
            return;
        };
        let session = self.ivars().session.borrow();
        let (text, size, style, color) = match existing.and_then(|id| session.doc.get(id)) {
            Some(Annotation {
                shape: Shape::Text {
                    text, size, style, ..
                },
                color,
                ..
            }) => (text.clone(), *size, *style, *color),
            _ => (
                String::new(),
                session.text_size,
                session.text_style,
                session.color,
            ),
        };
        drop(session);
        let mtm = self.mtm();
        let zoom = canvas.zoom();
        let field = NSTextField::new(mtm);
        field.setStringValue(&NSString::from_str(&text));
        field.setFont(Some(&render::text_font(size * zoom)));
        field.setTextColor(Some(&render::ns_color(color)));
        field.setBezeled(false);
        field.setDrawsBackground(style == TextStyle::Background);
        field.setBackgroundColor(Some(&render::ns_color(color)));
        if style == TextStyle::Background {
            field.setTextColor(Some(&NSColor::whiteColor()));
        }
        field.setFocusRingType(NSFocusRingType::None);
        field.setEditable(true);
        // SAFETY: the controller outlives the field and implements the delegate methods.
        unsafe {
            field.setDelegate(Some(ProtocolObject::from_ref(self)));
            field.setTarget(Some(self.as_ref()));
            field.setAction(Some(sel!(textCommitted:)));
        }
        let origin = canvas.to_view(at);
        field.setFrame(NSRect::new(
            NSPoint::new(origin.x, origin.y),
            NSSize::new(60.0, size * zoom * 1.3),
        ));
        canvas.addSubview(&field);
        *self.ivars().text.borrow_mut() = Some(TextEdit {
            field: field.clone(),
            origin: at,
            existing,
        });
        self.fit_text_field();
        if let Some(window) = canvas.window() {
            window.makeFirstResponder(Some(&field));
        }
        canvas.setNeedsDisplay(true);
    }

    fn fit_text_field(&self) {
        if let Some(edit) = self.ivars().text.borrow().as_ref() {
            let frame = edit.field.frame();
            edit.field.sizeToFit();
            let fitted = edit.field.frame();
            edit.field.setFrame(NSRect::new(
                frame.origin,
                NSSize::new(fitted.size.width.max(60.0) + 12.0, fitted.size.height),
            ));
        }
    }

    /// Ends text editing, if any, writing the text into the document.
    pub fn commit_text(&self) {
        let Some(edit) = self.ivars().text.borrow_mut().take() else {
            return;
        };
        let text = edit.field.stringValue().to_string();
        edit.field.removeFromSuperview();
        {
            let mut session = self.ivars().session.borrow_mut();
            let (size, style) = match edit.existing.and_then(|id| session.doc.get(id)) {
                Some(Annotation {
                    shape: Shape::Text { size, style, .. },
                    ..
                }) => (*size, *style),
                _ => (session.text_size, session.text_style),
            };
            let measured = render::text_size(&text, size, style);
            session.commit_text(
                edit.existing,
                edit.origin,
                &text,
                (measured.width, measured.height),
            );
        }
        self.refresh();
    }

    fn editing_text_id(&self) -> Option<u64> {
        self.ivars().text.borrow().as_ref().and_then(|e| e.existing)
    }

    fn rendered(&self) -> Option<objc2_core_foundation::CFRetained<objc2_core_graphics::CGImage>> {
        self.commit_text();
        let session = self.ivars().session.borrow();
        let mut doc = session.doc.clone();
        // A crop being edited counts.
        if let Some(draft) = session.crop_draft {
            doc.crop = Some(draft);
        }
        render::export(&doc, &self.ivars().base)
    }

    fn copy_to_clipboard(&self) {
        let Some(image) = self.rendered() else {
            return;
        };
        match crate::capture::image::encode(&image, ImageFormat::Png, 1.0) {
            Some(png) => {
                output::copy_png(&png);
                self.ivars().session.borrow_mut().dirty = false;
                tracing::info!("annotated image copied");
            }
            None => tracing::error!("could not encode the annotated image"),
        }
    }

    /// Saves over the capture file, or (for opened files and clipboard
    /// images) into the save folder like a new capture. Then closes.
    fn save(&self) {
        let Some(image) = self.rendered() else {
            return;
        };
        let source = self.ivars().source.borrow().clone();
        let scale = self.ivars().base.scale;
        let result = match &source {
            Source::Capture { file } => {
                let format = format_for(file);
                let quality = crate::quick_access::capture_settings().quality(format);
                output::save_to(file, &image, format, quality).map(|_| {
                    crate::quick_access::refresh(file, image.clone());
                    crate::history::refresh(file.clone(), image.clone(), None);
                    file.clone()
                })
            }
            Source::Project { path } => {
                let doc = self.ivars().session.borrow().doc.clone();
                project::save(path, &self.ivars().base, &doc).map(|()| path.clone())
            }
            Source::File { .. } | Source::Clipboard => {
                let settings = crate::quick_access::capture_settings();
                output::save_to_directory(&image, scale, &settings, None).map(|(path, png)| {
                    crate::history::record_image(image.clone(), path.clone(), None);
                    crate::quick_access::add(
                        self.mtm(),
                        output::Delivered {
                            image: image.clone(),
                            scale,
                            source: None,
                            file: path.clone(),
                            saved: true,
                            png: (settings.format == ImageFormat::Png).then_some(png),
                        },
                    );
                    path
                })
            }
        };
        match result {
            Ok(path) => {
                tracing::info!(path = %path.display(), "annotated image saved");
                self.ivars().session.borrow_mut().dirty = false;
                if let Some(window) = self.window() {
                    window.close();
                }
            }
            Err(e) => self.show_error("Could not save", &e),
        }
    }

    fn save_panel(&self, name: &str) -> Option<PathBuf> {
        let panel = NSSavePanel::savePanel(self.mtm());
        panel.setNameFieldStringValue(&NSString::from_str(name));
        if panel.runModal() != NSModalResponseOK {
            return None;
        }
        panel
            .URL()
            .and_then(|u| u.path())
            .map(|p| PathBuf::from(p.to_string()))
    }

    fn suggested_stem(&self) -> String {
        let source = self.ivars().source.borrow();
        let path = match &*source {
            Source::Capture { file } => Some(file),
            Source::File { path } | Source::Project { path } => Some(path),
            Source::Clipboard => None,
        };
        path.and_then(|p| p.file_stem()).map_or_else(
            || "Annotated".to_string(),
            |s| s.to_string_lossy().into_owned(),
        )
    }

    fn save_as(&self) {
        let Some(image) = self.rendered() else {
            return;
        };
        let Some(path) = self.save_panel(&format!("{}.png", self.suggested_stem())) else {
            return;
        };
        match output::save_to(
            &path,
            &image,
            format_for(&path),
            quality_for(format_for(&path)),
        ) {
            Ok(_) => {
                self.ivars().session.borrow_mut().dirty = false;
                tracing::info!(path = %path.display(), "annotated image saved");
            }
            Err(e) => self.show_error("Could not save", &e),
        }
    }

    fn save_project(&self) {
        self.commit_text();
        let name = format!("{}.{}", self.suggested_stem(), project::EXTENSION);
        let Some(path) = self.save_panel(&name) else {
            return;
        };
        let doc = self.ivars().session.borrow().doc.clone();
        match project::save(&path, &self.ivars().base, &doc) {
            Ok(()) => {
                self.ivars().session.borrow_mut().dirty = false;
                *self.ivars().source.borrow_mut() = Source::Project { path: path.clone() };
                if let Some(window) = self.window() {
                    window.setTitle(&NSString::from_str(
                        &path.file_name().unwrap_or_default().to_string_lossy(),
                    ));
                }
                tracing::info!(path = %path.display(), "project saved");
            }
            Err(e) => self.show_error("Could not save the project", &e),
        }
    }

    fn pin(&self) {
        let Some(image) = self.rendered() else {
            return;
        };
        let file = match &*self.ivars().source.borrow() {
            Source::Capture { file } => file.clone(),
            Source::File { path } | Source::Project { path } => path.clone(),
            Source::Clipboard => PathBuf::from("Annotated.png"),
        };
        let item =
            crate::quick_access::Item::new(image, self.ivars().base.scale, None, file, false);
        crate::pin::pin(self.mtm(), &item);
    }

    fn copy_text(&self) {
        if let Some(image) = self.rendered() {
            crate::ocr::copy_text(image, None);
        }
    }

    /// Finds sensitive information in the original image (on a background
    /// thread) and covers it.
    fn redact(&self) {
        let id = self.ivars().id;
        let image = self.ivars().base.cg.clone();
        let (width, height) = (self.ivars().base.width, self.ivars().base.height);
        let settings = crate::ocr::settings();
        crate::ui::toast::show(self.mtm(), "Looking for sensitive info…");
        std::thread::spawn(move || {
            let result = crate::ocr::recognize(&image, &settings, |text| {
                crate::ocr::sensitive::find(text)
                    .into_iter()
                    .map(|(_, r)| r)
                    .collect()
            });
            let rects: Result<Vec<Rect>, String> = result.map(|lines| {
                lines
                    .iter()
                    .flat_map(|l| l.matches.iter())
                    .map(|m| {
                        Rect::new(
                            m.x * width,
                            m.y * height,
                            m.width * width,
                            m.height * height,
                        )
                    })
                    .collect()
            });
            dispatch2::DispatchQueue::main().exec_async(move || {
                let mtm = MainThreadMarker::new().expect("the main queue runs on the main thread");
                let Some(editor) = registry::get(id) else {
                    return;
                };
                match rects {
                    Ok(rects) => {
                        let n = editor.ivars().session.borrow_mut().add_redactions(&rects);
                        editor.refresh();
                        let message = match n {
                            0 => "No sensitive info found".to_string(),
                            1 => "Redacted 1 item".to_string(),
                            n => format!("Redacted {n} items"),
                        };
                        crate::ui::toast::show(mtm, &message);
                    }
                    Err(e) => editor.show_error("Text recognition failed", &e),
                }
            });
        });
    }

    fn show_error(&self, title: &str, message: &str) {
        tracing::error!(error = message, "{title}");
        let alert = NSAlert::new(self.mtm());
        alert.setMessageText(&NSString::from_str(title));
        alert.setInformativeText(&NSString::from_str(message));
        alert.runModal();
    }
}

fn format_for(path: &std::path::Path) -> ImageFormat {
    path.extension()
        .and_then(|e| e.to_str())
        .and_then(ImageFormat::from_extension)
        .unwrap_or(ImageFormat::Png)
}

/// Save As quality: high for JPEG, lossless for WebP.
fn quality_for(format: ImageFormat) -> f64 {
    if format == ImageFormat::Webp {
        1.0
    } else {
        0.92
    }
}

fn to_color(color: &NSColor) -> Color {
    let srgb = color.colorUsingColorSpace(&NSColorSpace::sRGBColorSpace());
    match srgb {
        Some(c) => Color {
            r: c.redComponent(),
            g: c.greenComponent(),
            b: c.blueComponent(),
            a: c.alphaComponent(),
        },
        None => Color::RED,
    }
}

fn resize_text(session: &mut Session, id: u64, new_size: f64) {
    restyle(session, id, |size, _| *size = new_size);
}

fn restyle_text(session: &mut Session, id: u64, new_style: TextStyle) {
    restyle(session, id, |_, style| *style = new_style);
}

/// Changes a text annotation's size or style and re-measures it.
fn restyle(session: &mut Session, id: u64, f: impl FnOnce(&mut f64, &mut TextStyle)) {
    let Some(Annotation {
        shape:
            Shape::Text {
                origin,
                text,
                size,
                style,
                ..
            },
        ..
    }) = session.doc.get(id).cloned()
    else {
        return;
    };
    let (mut size, mut style) = (size, style);
    f(&mut size, &mut style);
    let measured = render::text_size(&text, size, style);
    session.history.record(&session.doc);
    session.dirty = true;
    if let Some(a) = session.doc.get_mut(id) {
        a.shape = Shape::Text {
            origin,
            bounds: Rect::new(origin.x, origin.y, measured.width, measured.height),
            text,
            size,
            style,
        };
    }
}

define_class!(
    // SAFETY: NSView can be subclassed; no Drop implementation.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = u64]
    pub struct CanvasView;

    impl CanvasView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
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

        #[unsafe(method(resetCursorRects))]
        fn reset_cursor_rects(&self) {
            let Some(editor) = self.editor() else {
                return;
            };
            let tool = editor.ivars().session.borrow().tool;
            let cursor = match tool {
                Tool::Select | Tool::Background => NSCursor::arrowCursor(),
                Tool::Text => NSCursor::IBeamCursor(),
                _ => NSCursor::crosshairCursor(),
            };
            self.addCursorRect_cursor(self.bounds(), &cursor);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let Some(editor) = self.editor() else {
                return;
            };
            editor.commit_text();
            if let Some(window) = self.window() {
                window.makeFirstResponder(Some(self));
            }
            let p = self.event_point(event);
            let tolerance = 6.0 / self.zoom();
            let effect = editor.ivars().session.borrow_mut().mouse_down(p, tolerance);
            if let Effect::EditText { at, existing } = effect {
                editor.begin_text(at, existing);
            }
            editor.refresh();
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let Some(editor) = self.editor() else {
                return;
            };
            let p = self.event_point(event);
            let shift = event.modifierFlags().contains(NSEventModifierFlags::Shift);
            editor.ivars().session.borrow_mut().mouse_dragged(p, shift);
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            if let Some(editor) = self.editor() {
                editor.ivars().session.borrow_mut().mouse_up();
                editor.refresh();
            }
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            if !self.handle_key(event) {
                // SAFETY: forwarding the event to the superclass.
                unsafe {
                    let _: () = msg_send![super(self), keyDown: event];
                }
            }
        }

        #[unsafe(method(undo:))]
        fn undo(&self, _sender: Option<&AnyObject>) {
            if let Some(editor) = self.editor() {
                editor.undo();
            }
        }

        #[unsafe(method(redo:))]
        fn redo(&self, _sender: Option<&AnyObject>) {
            if let Some(editor) = self.editor() {
                editor.redo();
            }
        }

        #[unsafe(method(copy:))]
        fn copy(&self, _sender: Option<&AnyObject>) {
            if let Some(editor) = self.editor() {
                editor.copy_to_clipboard();
            }
        }

        #[unsafe(method(delete:))]
        fn delete(&self, _sender: Option<&AnyObject>) {
            if let Some(editor) = self.editor() {
                editor.ivars().session.borrow_mut().delete_selection();
                editor.refresh();
            }
        }
    }
);

impl CanvasView {
    fn new(mtm: MainThreadMarker, editor_id: u64, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(editor_id);
        // SAFETY: NSView's designated initializer.
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    fn editor(&self) -> Option<Retained<EditorController>> {
        registry::get(*self.ivars())
    }

    /// The document as displayed: while cropping, the whole image without
    /// crop or background.
    fn display_doc(editor: &EditorController) -> super::model::Document {
        let session = editor.ivars().session.borrow();
        let mut doc = session.doc.clone();
        if session.tool == Tool::Crop {
            doc.crop = None;
            doc.background = None;
        }
        doc
    }

    /// Canvas-to-view scale and offset that fit the canvas in the view.
    fn transform(&self, editor: &EditorController) -> (f64, Point, super::model::CanvasLayout) {
        let layout = editor.ivars().base.layout(&Self::display_doc(editor));
        let b = self.bounds();
        let zoom = ((b.size.width - 2.0 * CANVAS_MARGIN) / layout.width)
            .min((b.size.height - 2.0 * CANVAS_MARGIN) / layout.height)
            .clamp(0.05, 2.0);
        let offset = Point::new(
            ((b.size.width - layout.width * zoom) / 2.0).round(),
            ((b.size.height - layout.height * zoom) / 2.0).round(),
        );
        (zoom, offset, layout)
    }

    pub fn zoom(&self) -> f64 {
        self.editor().map_or(1.0, |e| self.transform(&e).0)
    }

    fn to_image(&self, p: Point) -> Point {
        let Some(editor) = self.editor() else {
            return p;
        };
        let (zoom, offset, layout) = self.transform(&editor);
        layout.to_image(Point::new((p.x - offset.x) / zoom, (p.y - offset.y) / zoom))
    }

    pub fn to_view(&self, p: Point) -> Point {
        let Some(editor) = self.editor() else {
            return p;
        };
        let (zoom, offset, layout) = self.transform(&editor);
        let c = layout.to_canvas(p);
        Point::new(offset.x + c.x * zoom, offset.y + c.y * zoom)
    }

    fn view_rect(&self, r: Rect) -> Rect {
        let a = self.to_view(Point::new(r.x, r.y));
        let b = self.to_view(Point::new(r.max_x(), r.max_y()));
        Rect::from_corners(a, b)
    }

    fn event_point(&self, event: &NSEvent) -> Point {
        let p = self.convertPoint_fromView(event.locationInWindow(), None);
        self.to_image(Point::new(p.x, p.y))
    }

    /// Returns false if the key was not handled.
    fn handle_key(&self, event: &NSEvent) -> bool {
        const RETURN: u16 = 36;
        const DELETE: u16 = 51;
        const ESCAPE: u16 = 53;
        const FORWARD_DELETE: u16 = 117;
        let Some(editor) = self.editor() else {
            return false;
        };
        let flags = event.modifierFlags();
        let command = flags.contains(NSEventModifierFlags::Command);
        let shift = flags.contains(NSEventModifierFlags::Shift);
        let chars = event
            .charactersIgnoringModifiers()
            .map(|s| s.to_string().to_lowercase())
            .unwrap_or_default();
        match (command, event.keyCode(), chars.as_str()) {
            (true, _, "z") if shift => editor.redo(),
            (true, _, "z") => editor.undo(),
            (true, _, "c") => editor.copy_to_clipboard(),
            (true, _, "s") if shift => editor.save_as(),
            (true, _, "s") => editor.save(),
            (true, _, "w") => {
                if let Some(window) = self.window() {
                    window.performClose(None);
                }
            }
            (false, DELETE | FORWARD_DELETE, _) => {
                editor.ivars().session.borrow_mut().delete_selection();
                editor.refresh();
            }
            (false, ESCAPE, _) => {
                let mut session = editor.ivars().session.borrow_mut();
                if session.tool == Tool::Crop {
                    session.reset_crop();
                } else {
                    session.selection = None;
                }
                drop(session);
                editor.refresh();
            }
            (false, RETURN, _) if editor.ivars().session.borrow().tool == Tool::Crop => {
                editor.set_tool(Tool::Select);
            }
            (false, _, c) if c.chars().count() == 1 => {
                match Tool::from_shortcut(c.chars().next().unwrap()) {
                    Some(tool) => editor.set_tool(tool),
                    None => return false,
                }
            }
            _ => return false,
        }
        true
    }

    fn draw(&self) {
        let Some(editor) = self.editor() else {
            return;
        };
        let b = self.bounds();
        let bounds = Rect::new(0.0, 0.0, b.size.width, b.size.height);
        draw::fill_rect(bounds, &NSColor::colorWithWhite_alpha(0.16, 1.0));
        let Some(context) = NSGraphicsContext::currentContext() else {
            return;
        };
        let (zoom, offset, layout) = self.transform(&editor);
        let doc = Self::display_doc(&editor);

        context.saveGraphicsState();
        let transform = NSAffineTransform::transform();
        transform.translateXBy_yBy(offset.x, offset.y);
        transform.scaleBy(zoom);
        objc2_app_kit::NSAffineTransformNSAppKitAdditions::concat(&*transform);
        render::render(&doc, &editor.ivars().base, editor.editing_text_id());
        context.restoreGraphicsState();

        let session = editor.ivars().session.borrow();
        let accent = NSColor::controlAccentColor();
        if let Some(draft) = session.crop_draft.filter(|_| session.tool == Tool::Crop) {
            let image = self.view_rect(layout.source);
            let crop = self.view_rect(draft);
            // Dim outside the crop, inside the image.
            let dim = NSColor::colorWithWhite_alpha(0.0, 0.55);
            for r in [
                Rect::new(image.x, image.y, image.width, crop.y - image.y),
                Rect::new(
                    image.x,
                    crop.max_y(),
                    image.width,
                    image.max_y() - crop.max_y(),
                ),
                Rect::new(image.x, crop.y, crop.x - image.x, crop.height),
                Rect::new(
                    crop.max_x(),
                    crop.y,
                    image.max_x() - crop.max_x(),
                    crop.height,
                ),
            ] {
                if r.width > 0.0 && r.height > 0.0 {
                    draw::fill_rect(r, &dim);
                }
            }
            NSColor::whiteColor().setStroke();
            draw::stroke_rect(draw::inset(crop, -0.5), 1.0);
            for corner in [
                Point::new(crop.x, crop.y),
                Point::new(crop.max_x(), crop.y),
                Point::new(crop.x, crop.max_y()),
                Point::new(crop.max_x(), crop.max_y()),
            ] {
                draw_handle(corner, &accent);
            }
            let label = format!("{} × {}", draft.width.round(), draft.height.round());
            draw::draw_label(
                &label,
                Point::new(crop.x, (crop.max_y() + 6.0).min(bounds.max_y() - 24.0)),
                12.0,
            );
        } else if let Some(a) = session.selection.and_then(|id| session.doc.get(id)) {
            let r = draw::inset(self.view_rect(a.bounds()), -4.0);
            accent.setStroke();
            draw::stroke_rect(r, 1.0);
            for (_, p) in a.handles() {
                draw_handle(self.to_view(p), &accent);
            }
        }
        let _ = zoom;
    }
}

fn draw_handle(p: Point, accent: &NSColor) {
    let r = Rect::new(p.x - 4.5, p.y - 4.5, 9.0, 9.0);
    draw::fill_rounded(r, 2.0, &NSColor::whiteColor());
    draw::stroke_rounded(r, 2.0, 1.5, accent);
}
