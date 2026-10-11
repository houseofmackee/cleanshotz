//! The editor window: a toolbar of tools and actions, a contextual options
//! bar, and the canvas.

use std::cell::{Cell, OnceCell, RefCell};
use std::path::PathBuf;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSAutoresizingMaskOptions, NSBackingStoreType, NSBezelStyle, NSButton, NSColor,
    NSColorSpace, NSColorWell, NSColorWellStyle, NSControlStateValueOff, NSControlStateValueOn,
    NSControlTextEditingDelegate, NSCursor, NSDraggingInfo, NSEvent, NSEventModifierFlags,
    NSFocusRingType, NSGraphicsContext, NSImage, NSModalResponseOK, NSPopUpButton, NSResponder,
    NSSavePanel, NSSegmentedControl, NSSlider, NSTextField, NSTextFieldDelegate, NSView, NSWindow,
    NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    NSAffineTransform, NSArray, NSNotification, NSObjectProtocol, NSPoint, NSRect, NSSize,
    NSString, ns_string,
};

use super::model::{
    Annotation, ArrowStyle, Aspect, Background, Color, Fill, Orientation, Shape, TextStyle, Turn,
};
use super::picture::{Origin, Picture};
use super::presets::{self, Preset};
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

const MORE_SAVE_AS: isize = 1;
const MORE_SAVE_PROJECT: isize = 2;
const MORE_PIN: isize = 3;
const MORE_COPY_TEXT: isize = 4;
const MORE_REDACT: isize = 5;
const MORE_ADD_RIGHT: isize = 6;
const MORE_ADD_BELOW: isize = 7;
const MORE_ROTATE_RIGHT: isize = 8;
const MORE_ROTATE_LEFT: isize = 9;
const MORE_FLIP_HORIZONTAL: isize = 10;
const MORE_FLIP_VERTICAL: isize = 11;
const MORE_RESIZE: isize = 12;
const MORE_BEFORE_AFTER: isize = 13;
const MORE_PRINT: isize = 14;
/// Image layer opacities offered in the style menu.
const OPACITIES: [f64; 4] = [1.0, 0.75, 0.5, 0.25];

// Tags of the background menu's items.
const BG_NONE: isize = 1;
/// Plus the index of a built-in fill.
const BG_BUILTIN: isize = 100;
/// Plus the index of a saved preset.
const BG_SAVED: isize = 200;
const BG_WALLPAPER: isize = 300;
const BG_CHOOSE: isize = 301;
/// The current image background, when it is not a preset.
const BG_IMAGE: isize = 302;
const BG_SAVE: isize = 400;
const BG_DELETE: isize = 401;

/// Background images are kept at most this many pixels on a side.
const MAX_BACKGROUND: usize = 3840;

struct Ui {
    tools: Retained<NSSegmentedControl>,
    color: Retained<NSColorWell>,
    width: Retained<NSSlider>,
    style: Retained<NSPopUpButton>,
    sketchy: Retained<NSButton>,
    background: Retained<NSPopUpButton>,
    padding: Retained<NSSlider>,
    radius: Retained<NSSlider>,
    shadow: Retained<NSButton>,
    aspect: Retained<NSPopUpButton>,
    balance: Retained<NSButton>,
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
    base: RefCell<BaseImage>,
    session: RefCell<Session>,
    source: RefCell<Source>,
    window: OnceCell<Retained<NSWindow>>,
    canvas: OnceCell<Retained<CanvasView>>,
    ui: OnceCell<Ui>,
    text: RefCell<Option<TextEdit>>,
    /// Saved background presets, read when the window opens.
    presets: RefCell<Vec<Preset>>,
    /// Text recognition for the text highlighter has started for this
    /// orientation of the image.
    text_lines_for: Cell<Option<Orientation>>,
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
            let mut wants_text = false;
            {
                let mut session = self.ivars().session.borrow_mut();
                match session.tool {
                    _ if session.selected_opacity().is_some() => {
                        let opacity = OPACITIES.get(index.max(0) as usize).copied().unwrap_or(1.0);
                        session.set_opacity(opacity);
                    }
                    Tool::Highlighter => {
                        session.highlight_text = index == 1;
                        wants_text = session.highlight_text;
                    }
                    Tool::Arrow => session.set_arrow_style(match index {
                        1 => ArrowStyle::Open,
                        2 => ArrowStyle::Double,
                        3 => ArrowStyle::Tapered,
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
            if wants_text {
                self.recognize_text_lines();
            }
            self.refresh();
        }

        #[unsafe(method(sketchyChanged:))]
        fn sketchy_changed(&self, sender: &NSButton) {
            self.ivars()
                .session
                .borrow_mut()
                .set_sketchy(sender.state() == NSControlStateValueOn);
            self.refresh();
        }

        #[unsafe(method(backgroundChanged:))]
        fn background_changed(&self, sender: &AnyObject) {
            self.change_background(sender);
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
                MORE_ADD_RIGHT => self.add_images(false),
                MORE_ADD_BELOW => self.add_images(true),
                MORE_ROTATE_RIGHT => self.turn(Turn::Right),
                MORE_ROTATE_LEFT => self.turn(Turn::Left),
                MORE_FLIP_HORIZONTAL => self.turn(Turn::FlipHorizontal),
                MORE_FLIP_VERTICAL => self.turn(Turn::FlipVertical),
                MORE_RESIZE => self.resize(),
                MORE_BEFORE_AFTER => self.export_before_after(),
                MORE_PRINT => self.print(),
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
            base: RefCell::new(base),
            session: RefCell::new(session),
            source: RefCell::new(source),
            window: OnceCell::new(),
            canvas: OnceCell::new(),
            ui: OnceCell::new(),
            text: RefCell::new(None),
            presets: RefCell::new(presets::saved()),
            text_lines_for: Cell::new(None),
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
        let layout = self
            .ivars()
            .base
            .borrow()
            .layout(&self.ivars().session.borrow().doc);
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
        let id = self.ivars().id;
        let editor_action =
            |name: &'static str, f: fn(&EditorController)| -> (&'static str, Box<dyn Fn()>) {
                (
                    name,
                    Box::new(move || {
                        if let Some(editor) = registry::get(id) {
                            f(&editor);
                        }
                    }),
                )
            };
        crate::ui::access::actions(
            &canvas,
            vec![
                editor_action("Undo", |e| e.undo()),
                editor_action("Redo", |e| e.redo()),
                editor_action("Rotate Right", |e| e.turn(Turn::Right)),
                editor_action("Rotate Left", |e| e.turn(Turn::Left)),
                editor_action("Copy Image", |e| e.copy_to_clipboard()),
                editor_action("Save", |e| e.save()),
            ],
        );

        let ui = self.build_bars(mtm, &content, width, height);
        let _ = self.ivars().ui.set(ui);
        let _ = self.ivars().canvas.set(canvas.clone());
        self.describe_canvas(&canvas);
        let _ = self.ivars().window.set(window.clone());
        self.sync_controls();

        window.center();
        crate::ui::bring_forward(mtm, &window);
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
                Some(&NSString::from_str(&crate::i18n::tr(tool.title()))),
            );
            match image {
                Some(image) => tools.setImage_forSegment(Some(&image), i),
                None => tools.setLabel_forSegment(&NSString::from_str(&tool.title()[..1]), i),
            }
            tools.setWidth_forSegment(32.0, i);
            tools.setToolTip_forSegment(
                Some(&NSString::from_str(&format!(
                    "{} ({})",
                    crate::i18n::tr(tool.title()),
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
        for title in crate::i18n::labels(&[
            "•••",
            "Save As…",
            "Save Project…",
            "Pin to Screen",
            "Copy Text",
            "Redact Sensitive Info",
            "Add Image to the Right…",
            "Add Image Below…",
            "Rotate Right",
            "Rotate Left",
            "Flip Horizontally",
            "Flip Vertically",
            "Resize…",
            "Export Before/After GIF…",
            "Print…",
        ]) {
            more.addItemWithTitle(&NSString::from_str(&title));
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
        // SAFETY: the action exists on the controller.
        let sketchy = unsafe {
            NSButton::checkboxWithTitle_target_action(
                &NSString::from_str(&t!("Hand-drawn")),
                Some(target),
                Some(sel!(sketchyChanged:)),
                mtm,
            )
        };
        add(
            &sketchy,
            NSRect::new(NSPoint::new(398.0, row2 + 2.0), NSSize::new(110.0, 20.0)),
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
                p.addItemWithTitle(&NSString::from_str(&crate::i18n::tr(item)));
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
        // Filled in by `sync_background_menu`.
        let background = popup(&[], 12.0, 150.0);
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
        let padding_label = label(&t!("Padding"), 172.0);
        let padding = slider(64.0, 200.0, 228.0);
        let radius_label = label(&t!("Corners"), 350.0);
        let radius = slider(12.0, 40.0, 410.0);
        // SAFETY: the action exists on the controller.
        let shadow = unsafe {
            NSButton::checkboxWithTitle_target_action(
                &NSString::from_str(&t!("Shadow")),
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
            &Aspect::ALL.iter().map(|a| a.title()).collect::<Vec<_>>(),
            614.0,
            150.0,
        );
        // SAFETY: the action exists on the controller.
        let balance = unsafe {
            NSButton::checkboxWithTitle_target_action(
                &NSString::from_str(&t!("Balance")),
                Some(target),
                Some(sel!(backgroundChanged:)),
                mtm,
            )
        };
        balance.setToolTip(Some(&NSString::from_str(&t!(
            "Trim plain margins so the capture sits in the middle"
        ))));
        add(
            &balance,
            NSRect::new(NSPoint::new(772.0, row2 + 2.0), NSSize::new(84.0, 20.0)),
            top,
        );

        let as_view = |v: &NSView| v.retain();
        Ui {
            drawing_group: vec![
                as_view(&color),
                as_view(&width_slider),
                as_view(&style),
                as_view(&sketchy),
            ],
            background_group: vec![
                as_view(&background),
                as_view(&padding_label),
                as_view(&padding),
                as_view(&radius_label),
                as_view(&radius),
                as_view(&shadow),
                as_view(&aspect),
                as_view(&balance),
            ],
            tools,
            color,
            width: width_slider,
            style,
            sketchy,
            background,
            padding,
            radius,
            shadow,
            aspect,
            balance,
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
        let selected_opacity = session.selected_opacity();
        let (items, index): (Vec<String>, isize) = match tool {
            _ if selected_opacity.is_some() => {
                let opacity = selected_opacity.unwrap_or(1.0);
                (
                    OPACITIES
                        .iter()
                        .map(|o| t!("Opacity {percent}%", percent = (o * 100.0).round()))
                        .collect(),
                    OPACITIES
                        .iter()
                        .enumerate()
                        .min_by(|a, b| (a.1 - opacity).abs().total_cmp(&(b.1 - opacity).abs()))
                        .map_or(0, |(i, _)| i as isize),
                )
            }
            Tool::Highlighter => (
                crate::i18n::labels(&["Freehand", "Snap to Text"]),
                session.highlight_text as isize,
            ),
            Tool::Arrow => (
                crate::i18n::labels(&[
                    "Filled Arrow",
                    "Open Arrow",
                    "Double Arrow",
                    "Tapered Arrow",
                ]),
                match session.arrow_style {
                    ArrowStyle::Filled => 0,
                    ArrowStyle::Open => 1,
                    ArrowStyle::Double => 2,
                    ArrowStyle::Tapered => 3,
                },
            ),
            Tool::Rectangle | Tool::Ellipse => (
                crate::i18n::labels(&["Outline", "Filled"]),
                session.filled as isize,
            ),
            Tool::Text => (
                crate::i18n::labels(&["Plain Text", "Text on Color"]),
                (session.text_style == TextStyle::Background) as isize,
            ),
            _ => (Vec::new(), 0),
        };
        ui.style.removeAllItems();
        for item in &items {
            ui.style.addItemWithTitle(&NSString::from_str(item));
        }
        ui.style.selectItemAtIndex(index);
        ui.style.setHidden(items.is_empty() || background_tool);

        // Hand-drawn: for the shapes that support it, unless tapered.
        let sketchable = |shape: &Shape| match shape {
            Shape::Arrow { style, .. } => *style != ArrowStyle::Tapered,
            Shape::Line { .. } => true,
            Shape::Rectangle { filled, .. } | Shape::Ellipse { filled, .. } => !*filled,
            _ => false,
        };
        let (shows_sketchy, sketchy) = match selected {
            Some(a) => (sketchable(&a.shape), a.sketchy),
            None => (
                match tool {
                    Tool::Arrow => session.arrow_style != ArrowStyle::Tapered,
                    Tool::Line => true,
                    Tool::Rectangle | Tool::Ellipse => !session.filled,
                    _ => false,
                },
                session.sketchy,
            ),
        };
        ui.sketchy.setHidden(!shows_sketchy || background_tool);
        ui.sketchy.setState(if sketchy {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });

        let defaults = session.doc.background.clone().unwrap_or_default();
        ui.padding.setDoubleValue(defaults.padding);
        ui.radius.setDoubleValue(defaults.corner_radius);
        ui.shadow.setState(if defaults.shadow {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });
        ui.aspect.selectItemAtIndex(
            Aspect::ALL
                .iter()
                .position(|a| *a == defaults.aspect)
                .unwrap_or(0) as isize,
        );
        ui.balance.setState(if defaults.balance {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });
        if background_tool {
            self.sync_background_menu(ui, session.doc.background.as_ref());
        }

        ui.undo.setEnabled(session.history.can_undo());
        ui.redo.setEnabled(session.history.can_redo());
    }

    /// Rebuilds the background menu: no background, the built-in fills,
    /// saved presets, image choices and preset commands, with the current
    /// background selected.
    fn sync_background_menu(&self, ui: &Ui, background: Option<&Background>) {
        let presets = self.ivars().presets.borrow();
        let builtin = presets::builtin();
        let saved_index = background.and_then(|b| presets.iter().position(|p| p.matches(b)));
        let selected = match background {
            None => BG_NONE,
            Some(b) => saved_index
                .map(|i| BG_SAVED + i as isize)
                .or_else(|| {
                    builtin
                        .iter()
                        .position(|(_, f)| *f == b.fill)
                        .map(|i| BG_BUILTIN + i as isize)
                })
                .unwrap_or(match &b.fill {
                    Fill::Image { picture } if *picture.origin() == Origin::Wallpaper => {
                        BG_WALLPAPER
                    }
                    Fill::Image { .. } => BG_IMAGE,
                    _ => BG_BUILTIN,
                }),
        };
        let popup = &ui.background;
        popup.removeAllItems();
        let Some(menu) = popup.menu() else {
            return;
        };
        let mtm = self.mtm();
        let item = |title: &str, tag: isize| {
            popup.addItemWithTitle(&NSString::from_str(title));
            if let Some(item) = popup.lastItem() {
                item.setTag(tag);
            }
        };
        let separator = || menu.addItem(&objc2_app_kit::NSMenuItem::separatorItem(mtm));
        item("No Background", BG_NONE);
        for (i, (name, _)) in builtin.iter().enumerate() {
            item(name, BG_BUILTIN + i as isize);
        }
        if !presets.is_empty() {
            separator();
            for (i, preset) in presets.iter().enumerate() {
                item(&preset.name, BG_SAVED + i as isize);
            }
        }
        separator();
        item("Desktop Wallpaper", BG_WALLPAPER);
        if selected == BG_IMAGE {
            item("Image", BG_IMAGE);
        }
        item("Choose Image…", BG_CHOOSE);
        if background.is_some() {
            separator();
            item("Save as Preset…", BG_SAVE);
            if let Some(i) = saved_index {
                item(&t!("Delete “{name}”", name = presets[i].name), BG_DELETE);
            }
        }
        popup.selectItemWithTag(selected);
    }

    /// Applies the background menu or a background option.
    fn change_background(&self, sender: &AnyObject) {
        let Some(ui) = self.ivars().ui.get() else {
            return;
        };
        let mtm = self.mtm();
        let current = self.ivars().session.borrow().doc.background.clone();
        let with_options = |fill: Fill| Background {
            fill,
            padding: ui.padding.doubleValue().round(),
            corner_radius: ui.radius.doubleValue().round(),
            shadow: ui.shadow.state() == NSControlStateValueOn,
            aspect: Aspect::ALL
                .get(ui.aspect.indexOfSelectedItem().max(0) as usize)
                .copied()
                .unwrap_or(Aspect::Auto),
            balance: ui.balance.state() == NSControlStateValueOn,
        };
        let menu: &AnyObject = ui.background.as_ref();
        let background = if !std::ptr::eq(sender, menu) {
            // A slider, Shadow or the aspect ratio: keep the fill.
            current.map(|b| with_options(b.fill))
        } else {
            let tag = ui.background.selectedItem().map_or(BG_NONE, |i| i.tag());
            match tag {
                BG_NONE => None,
                t if (BG_BUILTIN..BG_SAVED).contains(&t) => presets::builtin()
                    .into_iter()
                    .nth((t - BG_BUILTIN) as usize)
                    .map(|(_, fill)| with_options(fill)),
                t if (BG_SAVED..BG_WALLPAPER).contains(&t) => {
                    let preset = self
                        .ivars()
                        .presets
                        .borrow()
                        .get((t - BG_SAVED) as usize)
                        .cloned();
                    match preset.map(|p| p.background(mtm)) {
                        Some(Ok(background)) => Some(background),
                        Some(Err(e)) => {
                            self.show_error("Could not use the preset", &e);
                            current
                        }
                        None => current,
                    }
                }
                BG_WALLPAPER => match presets::wallpaper(mtm) {
                    Some(picture) => Some(with_options(Fill::Image { picture })),
                    None => {
                        self.show_error("Could not read the desktop picture", "");
                        current
                    }
                },
                BG_CHOOSE => match choose_images(mtm, false).into_iter().next() {
                    Some(picture) => Some(with_options(Fill::Image {
                        picture: picture.limited(MAX_BACKGROUND),
                    })),
                    None => current,
                },
                BG_SAVE => {
                    if let Some(background) = &current
                        && let Some(name) = ask_name(mtm, "Save Background as Preset")
                    {
                        match presets::add(&name, background) {
                            Ok(()) => *self.ivars().presets.borrow_mut() = presets::saved(),
                            Err(e) => self.show_error("Could not save the preset", &e),
                        }
                    }
                    current
                }
                BG_DELETE => {
                    let name = current.as_ref().and_then(|b| {
                        self.ivars()
                            .presets
                            .borrow()
                            .iter()
                            .find(|p| p.matches(b))
                            .map(|p| p.name.clone())
                    });
                    if let Some(name) = name {
                        match presets::remove(&name) {
                            Ok(()) => *self.ivars().presets.borrow_mut() = presets::saved(),
                            Err(e) => self.show_error("Could not delete the preset", &e),
                        }
                    }
                    current
                }
                _ => current,
            }
        };
        if let Some(background) = &background {
            super::remember_background(background);
        }
        self.ivars().session.borrow_mut().set_background(background);
        self.refresh();
    }

    /// Adds image layers centered on `at` (image points), or on the image.
    pub fn insert_pictures(&self, pictures: Vec<Picture>, at: Option<Point>) {
        if pictures.is_empty() {
            objc2_app_kit::NSBeep();
            return;
        }
        self.commit_text();
        {
            let mut session = self.ivars().session.borrow_mut();
            for (i, picture) in pictures.into_iter().enumerate() {
                // Several at once fan out a little so all stay visible.
                let offset = 24.0 * i as f64;
                let at = at.map(|p| Point::new(p.x + offset, p.y + offset));
                session.insert_picture(picture, at);
            }
        }
        self.refresh();
    }

    /// Pastes images from the clipboard as layers.
    pub fn paste_images(&self) {
        let pictures = super::pictures_from(&objc2_app_kit::NSPasteboard::generalPasteboard());
        self.insert_pictures(pictures, None);
    }

    /// Combines captures: adds chosen images next to everything so far.
    fn add_images(&self, below: bool) {
        let pictures = choose_images(self.mtm(), true);
        if pictures.is_empty() {
            return;
        }
        self.commit_text();
        {
            let mut session = self.ivars().session.borrow_mut();
            for picture in pictures {
                session.append_picture(picture, below);
            }
        }
        self.refresh();
    }

    /// Redraws the canvas and updates the controls.
    pub fn refresh(&self) {
        self.sync_orientation();
        if let Some(canvas) = self.ivars().canvas.get() {
            canvas.setNeedsDisplay(true);
            self.describe_canvas(canvas);
        }
        self.sync_controls();
    }

    /// Rebuilds the turned image when the document's orientation changed
    /// (a turn, or undoing one).
    fn sync_orientation(&self) {
        let orientation = self.ivars().session.borrow().doc.orientation;
        if self.ivars().base.borrow().orientation == orientation {
            return;
        }
        let turned = self.ivars().base.borrow().with_orientation(orientation);
        *self.ivars().base.borrow_mut() = turned;
        // Recognized text belongs to the old orientation.
        self.ivars().session.borrow_mut().text_lines.clear();
        self.ivars().text_lines_for.set(None);
        if self.ivars().session.borrow().highlight_text {
            self.recognize_text_lines();
        }
    }

    /// Recognizes the lines of text in the image in the background, for the
    /// text highlighter. Once per orientation.
    fn recognize_text_lines(&self) {
        let orientation = self.ivars().session.borrow().doc.orientation;
        if self.ivars().text_lines_for.get() == Some(orientation) {
            return;
        }
        self.ivars().text_lines_for.set(Some(orientation));
        let (image, size) = {
            let base = self.ivars().base.borrow();
            (base.cg.clone(), (base.width, base.height))
        };
        let id = self.ivars().id;
        std::thread::spawn(move || {
            let settings = crate::ocr::settings();
            let lines = match crate::ocr::recognize(&image, &settings, |_| Vec::new()) {
                Ok(lines) => lines,
                Err(e) => {
                    tracing::warn!(error = %e, "text recognition for the highlighter failed");
                    return;
                }
            };
            let rects: Vec<Rect> = lines
                .iter()
                .map(|l| {
                    Rect::new(
                        l.bounds.x * size.0,
                        l.bounds.y * size.1,
                        l.bounds.width * size.0,
                        l.bounds.height * size.1,
                    )
                })
                .collect();
            dispatch2::DispatchQueue::main().exec_async(move || {
                if let Some(editor) = registry::get(id)
                    && editor.ivars().text_lines_for.get() == Some(orientation)
                {
                    editor.ivars().session.borrow_mut().text_lines = rects;
                }
            });
        });
    }

    /// Measures the run of one color through `p` (image points), across
    /// or up and down, and adds it as a measurement.
    fn measure_automatically(&self, p: Point, vertical: bool) {
        let found = {
            let base = self.ivars().base.borrow();
            let scale = base.scale;
            let (x, y) = ((p.x * scale).floor(), (p.y * scale).floor());
            if x < 0.0 || y < 0.0 {
                None
            } else {
                crate::capture::image::to_pixels(&base.cg).and_then(|(w, h, pixels)| {
                    super::trim::span(&pixels, w, h, (x as usize, y as usize), vertical, 12).map(
                        |(lo, hi)| {
                            let (lo, hi) = (lo as f64 / scale, (hi + 1) as f64 / scale);
                            if vertical {
                                (Point::new(p.x, lo), Point::new(p.x, hi))
                            } else {
                                (Point::new(lo, p.y), Point::new(hi, p.y))
                            }
                        },
                    )
                })
            }
        };
        if let Some((start, end)) = found {
            self.ivars().session.borrow_mut().add_measure(start, end);
        }
    }

    /// Prints the image as it is now (long captures over several pages).
    pub fn print(&self) {
        let Some(image) = self.rendered() else {
            return;
        };
        let title = self
            .window()
            .map(|w| w.title().to_string())
            .unwrap_or_default();
        crate::ui::print::print_image(self.mtm(), &image, &title);
    }

    /// Saves a two-frame GIF: the image without, then with, its top image
    /// layer (overlay compare).
    fn export_before_after(&self) {
        let Some(after) = self.rendered() else {
            return;
        };
        let before = {
            let session = self.ivars().session.borrow();
            let mut doc = session.doc.clone();
            if let Some(draft) = session.crop_draft {
                doc.crop = Some(draft);
            }
            let Some(top) = doc
                .annotations
                .iter()
                .rposition(|a| matches!(a.shape, Shape::Image { .. }))
            else {
                drop(session);
                crate::ui::toast::show(
                    self.mtm(),
                    "Paste or add a second image first, to compare with",
                );
                return;
            };
            doc.annotations.remove(top);
            // Keep the size of the canvas the same in both frames.
            let base = self.ivars().base.borrow();
            let after_layout = base.layout(&session.doc);
            doc.crop = Some(after_layout.source);
            render::export(&doc, &base)
        };
        let Some(before) = before else {
            return;
        };
        let panel = NSSavePanel::savePanel(self.mtm());
        panel.setNameFieldStringValue(&NSString::from_str(&t!("Before and After.gif")));
        if panel.runModal() != NSModalResponseOK {
            return;
        }
        let Some(path) = panel.URL().and_then(|u| u.path()) else {
            return;
        };
        let path = PathBuf::from(path.to_string());
        match crate::record::media::write_gif(&path, 2, 1.0, [before, after].into_iter()) {
            Ok(_) => {
                tracing::info!(path = %path.display(), "before and after GIF saved");
                crate::ui::toast::show(self.mtm(), "Saved");
            }
            Err(e) => {
                tracing::error!(error = %e, "could not save the before and after GIF");
                crate::ui::toast::show(self.mtm(), "Could not save the GIF");
            }
        }
    }

    /// Asks for the exported size and sets it.
    pub fn resize(&self) {
        self.commit_text();
        let (native, shown, retina) = {
            let mut doc = self.ivars().session.borrow().doc.clone();
            let base = self.ivars().base.borrow();
            let shown = render::export_size(&doc, &base);
            doc.output_scale = None;
            (render::export_size(&doc, &base), shown, base.scale > 1.0)
        };
        let Some(answer) = ask_size(self.mtm(), shown, retina) else {
            return;
        };
        let scale = match answer {
            SizeAnswer::Original => None,
            SizeAnswer::OneX => {
                let scale = self.ivars().base.borrow().scale;
                Some(1.0 / scale)
            }
            SizeAnswer::Typed(width, height) => {
                match super::model::resize_factor(&width, &height, native, shown) {
                    Ok(scale) => scale,
                    Err(()) => {
                        crate::ui::toast::show(
                            self.mtm(),
                            "Type a size in pixels or a percentage, such as 1280 or 50%",
                        );
                        return;
                    }
                }
            }
        };
        self.ivars().session.borrow_mut().set_output_scale(scale);
        let size = render::export_size(
            &self.ivars().session.borrow().doc,
            &self.ivars().base.borrow(),
        );
        crate::ui::toast::show(
            self.mtm(),
            &t!(
                "Exports at {width} × {height} pixels",
                width = size.0,
                height = size.1
            ),
        );
        self.refresh();
    }

    /// Rotates or mirrors the image with its annotations.
    pub fn turn(&self, turn: Turn) {
        self.commit_text();
        self.ivars().session.borrow_mut().turn(turn);
        self.refresh();
    }

    /// What VoiceOver says for the canvas: the image, how many annotations
    /// it has and the current tool.
    fn describe_canvas(&self, canvas: &CanvasView) {
        let session = self.ivars().session.borrow();
        let base = self.ivars().base.borrow();
        let count = session.doc.annotations.len();
        let annotations = match count {
            0 => t!("no annotations"),
            1 => t!("1 annotation"),
            n => t!("{n} annotations", n),
        };
        crate::ui::access::describe(
            canvas,
            // SAFETY: reading an AppKit constant.
            unsafe { objc2_app_kit::NSAccessibilityImageRole },
            &t!(
                "Image, {width} by {height} points, {annotations}. Tool: {tool}",
                width = base.width.round(),
                height = base.height.round(),
                annotations,
                tool = crate::i18n::tr(session.tool.title()),
            ),
            Some(&t!(
                "Pick tools in the toolbar above. Undo, Redo, Copy and Save are in the actions."
            )),
        );
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
        render::export(&doc, &self.ivars().base.borrow())
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
        let scale = render::export_scale(
            &self.ivars().session.borrow().doc,
            &self.ivars().base.borrow(),
        );
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
                project::save(path, &self.ivars().base.borrow(), &doc).map(|()| path.clone())
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
        match project::save(&path, &self.ivars().base.borrow(), &doc) {
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
        let scale = render::export_scale(
            &self.ivars().session.borrow().doc,
            &self.ivars().base.borrow(),
        );
        let item = crate::quick_access::Item::new(image, scale, None, file, false);
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
        let base = self.ivars().base.borrow();
        let image = base.cg.clone();
        let (width, height) = (base.width, base.height);
        drop(base);
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
                            0 => t!("No sensitive info found"),
                            1 => t!("Redacted 1 item"),
                            n => t!("Redacted {n} items", n),
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

/// Asks for image files; ones that are not images are skipped.
fn choose_images(mtm: MainThreadMarker, several: bool) -> Vec<Picture> {
    let panel = objc2_app_kit::NSOpenPanel::openPanel(mtm);
    panel.setCanChooseDirectories(false);
    panel.setAllowsMultipleSelection(several);
    if panel.runModal() != NSModalResponseOK {
        return Vec::new();
    }
    panel
        .URLs()
        .iter()
        .filter_map(|url| url.path())
        .filter_map(|path| Picture::load(std::path::Path::new(&path.to_string())))
        .collect()
}

enum SizeAnswer {
    Original,
    OneX,
    Typed(String, String),
}

/// Asks for the exported width and height, showing `shown` (pixels).
/// `retina` offers 1x. `None` when cancelled.
fn ask_size(mtm: MainThreadMarker, shown: (usize, usize), retina: bool) -> Option<SizeAnswer> {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(&t!("Resize")));
    alert.setInformativeText(&NSString::from_str(&t!(
        "The size of the exported image in pixels. Type pixels or a percentage of the original; proportions are kept."
    )));
    alert.addButtonWithTitle(&NSString::from_str(&t!("Resize")));
    alert.addButtonWithTitle(&NSString::from_str(&t!("Cancel")));
    alert.addButtonWithTitle(&NSString::from_str(&t!("Original Size")));
    if retina {
        alert.addButtonWithTitle(&NSString::from_str(&t!("1x")));
    }
    let view = NSView::initWithFrame(
        NSView::alloc(mtm),
        NSRect::new(NSPoint::ZERO, NSSize::new(240.0, 58.0)),
    );
    let row = |label: &str, value: usize, y: f64| {
        let text = NSTextField::labelWithString(&NSString::from_str(&crate::i18n::tr(label)), mtm);
        text.setFrame(NSRect::new(
            NSPoint::new(0.0, y + 3.0),
            NSSize::new(64.0, 18.0),
        ));
        view.addSubview(&text);
        let field = NSTextField::initWithFrame(
            NSTextField::alloc(mtm),
            NSRect::new(NSPoint::new(68.0, y), NSSize::new(110.0, 24.0)),
        );
        field.setStringValue(&NSString::from_str(&value.to_string()));
        view.addSubview(&field);
        let unit = NSTextField::labelWithString(ns_string!("px"), mtm);
        unit.setFrame(NSRect::new(
            NSPoint::new(184.0, y + 3.0),
            NSSize::new(40.0, 18.0),
        ));
        view.addSubview(&unit);
        field
    };
    let width = row("Width:", shown.0, 32.0);
    let height = row("Height:", shown.1, 0.0);
    // SAFETY: both fields live in the alert for as long as it runs.
    unsafe { width.setNextKeyView(Some(&height)) };
    alert.setAccessoryView(Some(&view));
    alert.window().setInitialFirstResponder(Some(&width));
    let response = alert.runModal();
    if response == objc2_app_kit::NSAlertFirstButtonReturn {
        Some(SizeAnswer::Typed(
            width.stringValue().to_string(),
            height.stringValue().to_string(),
        ))
    } else if response == objc2_app_kit::NSAlertThirdButtonReturn {
        Some(SizeAnswer::Original)
    } else if response == objc2_app_kit::NSAlertThirdButtonReturn + 1 {
        Some(SizeAnswer::OneX)
    } else {
        None
    }
}

/// Asks for a name; `None` when cancelled or left empty.
fn ask_name(mtm: MainThreadMarker, title: &str) -> Option<String> {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(title));
    alert.addButtonWithTitle(ns_string!("Save"));
    alert.addButtonWithTitle(ns_string!("Cancel"));
    let field = NSTextField::initWithFrame(
        NSTextField::alloc(mtm),
        NSRect::new(NSPoint::ZERO, NSSize::new(240.0, 24.0)),
    );
    field.setPlaceholderString(Some(ns_string!("Name")));
    alert.setAccessoryView(Some(&field));
    alert.window().setInitialFirstResponder(Some(&field));
    if alert.runModal() != objc2_app_kit::NSAlertFirstButtonReturn {
        return None;
    }
    let name = field.stringValue().to_string().trim().to_string();
    (!name.is_empty()).then_some(name)
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

type ViewTransform = (f64, Point, super::model::CanvasLayout);

pub struct CanvasIvars {
    editor: u64,
    /// The view transform while the mouse is down. Moving an image layer
    /// past the edge grows the canvas, which would otherwise rescale the
    /// view under the pointer and make the layer run away from it.
    frozen: std::cell::Cell<Option<ViewTransform>>,
}

define_class!(
    // SAFETY: NSView can be subclassed; no Drop implementation.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = CanvasIvars]
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
            self.ivars().frozen.set(Some(self.transform(&editor)));
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
            self.ivars().frozen.set(None);
            if let Some(editor) = self.editor() {
                editor.ivars().session.borrow_mut().mouse_up();
                let pending = editor.ivars().session.borrow_mut().take_pending_measure();
                if let Some(p) = pending {
                    // Shift measures up and down instead of across.
                    let vertical = NSEvent::modifierFlags_class().contains(NSEventModifierFlags::Shift);
                    editor.measure_automatically(p, vertical);
                }
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

        #[unsafe(method(paste:))]
        fn paste(&self, _sender: Option<&AnyObject>) {
            if let Some(editor) = self.editor() {
                editor.paste_images();
            }
        }

        #[unsafe(method(draggingEntered:))]
        fn dragging_entered(
            &self,
            sender: &ProtocolObject<dyn NSDraggingInfo>,
        ) -> objc2_app_kit::NSDragOperation {
            let pasteboard = sender.draggingPasteboard();
            let types = NSArray::from_retained_slice(&[
                NSString::from_str("public.file-url"),
                NSString::from_str("public.image"),
            ]);
            if pasteboard.availableTypeFromArray(&types).is_some() {
                objc2_app_kit::NSDragOperation::Copy
            } else {
                objc2_app_kit::NSDragOperation::None
            }
        }

        #[unsafe(method(performDragOperation:))]
        fn perform_drag_operation(
            &self,
            sender: &ProtocolObject<dyn NSDraggingInfo>,
        ) -> bool {
            self.drop_pictures(sender)
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
        let this = Self::alloc(mtm).set_ivars(CanvasIvars {
            editor: editor_id,
            frozen: std::cell::Cell::new(None),
        });
        // SAFETY: NSView's designated initializer.
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        // Images and image files can be dropped onto the canvas, Quick
        // Access cards included: that combines captures.
        let types = NSArray::from_retained_slice(&[
            NSString::from_str("public.file-url"),
            NSString::from_str("public.png"),
            NSString::from_str("public.tiff"),
        ]);
        this.registerForDraggedTypes(&types);
        this
    }

    fn editor(&self) -> Option<Retained<EditorController>> {
        registry::get(self.ivars().editor)
    }

    /// Adds dropped images where they were dropped.
    fn drop_pictures(&self, info: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
        let Some(editor) = self.editor() else {
            return false;
        };
        let pictures = super::pictures_from(&info.draggingPasteboard());
        if pictures.is_empty() {
            return false;
        }
        let p = self.convertPoint_fromView(info.draggingLocation(), None);
        let at = self.to_image(Point::new(p.x, p.y));
        editor.insert_pictures(pictures, Some(at));
        true
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
    fn transform(&self, editor: &EditorController) -> ViewTransform {
        if let Some(frozen) = self.ivars().frozen.get() {
            return frozen;
        }
        let layout = editor
            .ivars()
            .base
            .borrow()
            .layout(&Self::display_doc(editor));
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
            (true, _, "p") => editor.print(),
            (true, _, "r") if shift => editor.turn(Turn::Left),
            (true, _, "r") => editor.turn(Turn::Right),
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
        // While frozen the document may have grown: keep its image points
        // where the frozen layout put them.
        let now = editor.ivars().base.borrow().layout(&doc);
        transform.translateXBy_yBy(
            (layout.content.x - layout.source.x) - (now.content.x - now.source.x),
            (layout.content.y - layout.source.y) - (now.content.y - now.source.y),
        );
        objc2_app_kit::NSAffineTransformNSAppKitAdditions::concat(&*transform);
        render::render(
            &doc,
            &editor.ivars().base.borrow(),
            editor.editing_text_id(),
        );
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
