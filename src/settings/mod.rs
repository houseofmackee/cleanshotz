//! The Settings window: native controls for the settings file. Changes
//! apply at once and are written to the file; options without a control
//! here (upload headers and form fields) stay editable in the file.

mod recorder;

use std::cell::{Cell, OnceCell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSColor, NSControlStateValueOff, NSControlStateValueOn, NSFont,
    NSGridCell, NSGridCellPlacement, NSGridRowAlignment, NSGridView, NSImage,
    NSLayoutConstraintOrientation, NSOpenPanel, NSPopUpButton, NSSecureTextField, NSStackView,
    NSTextField, NSToolbar, NSToolbarDelegate, NSToolbarItem, NSView, NSWindow, NSWindowStyleMask,
    NSWindowToolbarStyle,
};
use objc2_foundation::{NSArray, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, ns_string};

use self::recorder::{Outcome, ShortcutRecorder};
use crate::action::Action;
use crate::config::{
    CameraShape, Codec, Config, Destination, Hotkeys, ImageFormat, KeystrokeMode, Quality, Side,
    VideoFormat,
};
use crate::i18n::tr;
use crate::platform::login_item;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    General,
    Screenshots,
    QuickAccess,
    Recording,
    History,
    Uploads,
    Watermark,
    Shortcuts,
}

impl Tab {
    const ALL: [Tab; 8] = [
        Tab::General,
        Tab::Screenshots,
        Tab::QuickAccess,
        Tab::Recording,
        Tab::History,
        Tab::Uploads,
        Tab::Watermark,
        Tab::Shortcuts,
    ];

    fn id(self) -> &'static str {
        match self {
            Tab::General => "general",
            Tab::Screenshots => "screenshots",
            Tab::QuickAccess => "quickaccess",
            Tab::Recording => "recording",
            Tab::History => "history",
            Tab::Uploads => "uploads",
            Tab::Watermark => "watermark",
            Tab::Shortcuts => "shortcuts",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Tab::General => "General",
            Tab::Screenshots => "Screenshots",
            Tab::QuickAccess => "Quick Access",
            Tab::Recording => "Recording",
            Tab::History => "History",
            Tab::Uploads => "Uploads",
            Tab::Watermark => "Watermark",
            Tab::Shortcuts => "Shortcuts",
        }
    }

    fn symbol(self) -> &'static str {
        match self {
            Tab::General => "gearshape",
            Tab::Screenshots => "camera.viewfinder",
            Tab::QuickAccess => "square.on.square",
            Tab::Recording => "record.circle",
            Tab::History => "clock.arrow.circlepath",
            Tab::Uploads => "icloud.and.arrow.up",
            Tab::Watermark => "c.circle",
            Tab::Shortcuts => "keyboard",
        }
    }

    fn from_id(id: &str) -> Option<Tab> {
        Self::ALL.into_iter().find(|t| t.id() == id)
    }

    /// The tab for a CleanShot `open-settings` tab name. Tabs CleanShotZ
    /// does not have open General; `about` is handled by the caller.
    pub fn for_url(name: &str) -> Tab {
        match name {
            "screenshots" | "annotate" => Tab::Screenshots,
            "quickaccess" => Tab::QuickAccess,
            "recording" => Tab::Recording,
            "cloud" | "uploads" => Tab::Uploads,
            "watermark" => Tab::Watermark,
            "shortcuts" => Tab::Shortcuts,
            "history" => Tab::History,
            _ => Tab::General,
        }
    }
}

/// Changes, saves and applies the settings; returns shortcut errors.
pub type Update = Box<dyn Fn(&dyn Fn(&mut Config)) -> Vec<String>>;

/// What the window needs from the app.
pub struct Host {
    pub config: Box<dyn Fn() -> Config>,
    pub update: Update,
    /// Turns global shortcuts off while one is being recorded, and back
    /// on (returning registration errors).
    pub suspend_hotkeys: Box<dyn Fn(bool) -> Vec<String>>,
}

type Get<T> = fn(&Config) -> T;
type Set<T> = fn(&mut Config, T);

enum Bind {
    Check(Get<bool>, Set<bool>),
    /// A pop-up menu; `rebuild` redraws the tab after a change (for
    /// choices that show or hide other rows).
    Choice {
        get: Get<usize>,
        set: Set<usize>,
        rebuild: bool,
    },
    Text(Get<String>, Set<String>),
    Number(Get<f64>, Set<f64>),
    /// Stored in the Keychain as `name`; `set` receives `keychain:name`.
    Secret {
        name: &'static str,
        get: Get<String>,
        set: Set<String>,
    },
    Folder(Get<String>, Set<String>),
    /// An image file, chosen with an open panel.
    ImageFile(Get<String>, Set<String>),
    /// The watermark on a sample picture; the control is the label for
    /// what is wrong with it.
    WatermarkPreview,
    LoginItem,
    /// Sparkle's automatic checks (kept by Sparkle, not in the file).
    Updates,
    Shortcut(Action),
    ClearShortcut(Action),
}

struct Bound {
    /// The control that sends the action.
    control: Retained<NSView>,
    /// The view that shows the value (the control itself, except for
    /// folders, where a label shows it and a button changes it).
    view: Retained<NSView>,
    bind: Bind,
}

pub struct Ivars {
    host: Host,
    window: OnceCell<Retained<NSWindow>>,
    tab: Cell<Tab>,
    bound: RefCell<Vec<Bound>>,
    status: RefCell<Option<Retained<NSTextField>>>,
}

thread_local! {
    static WINDOW: RefCell<Option<Retained<SettingsWindow>>> = const { RefCell::new(None) };
}

/// Shows the Settings window at `tab`.
pub fn open(mtm: MainThreadMarker, tab: Tab, host: impl FnOnce() -> Host) {
    let controller = WINDOW.with(|w| w.borrow().clone()).unwrap_or_else(|| {
        let c = SettingsWindow::new(mtm, host());
        WINDOW.with(|w| *w.borrow_mut() = Some(c.clone()));
        c
    });
    controller.show_tab(tab);
    if let Some(window) = controller.ivars().window.get() {
        crate::ui::bring_forward(mtm, window);
    }
}

/// Redraws the window after the settings file was reloaded.
pub fn refresh(_mtm: MainThreadMarker) {
    if let Some(c) = WINDOW.with(|w| w.borrow().clone())
        && c.ivars().window.get().is_some_and(|w| w.isVisible())
    {
        c.show_tab(c.ivars().tab.get());
    }
}

fn controller() -> Option<Retained<SettingsWindow>> {
    WINDOW.with(|w| w.borrow().clone())
}

fn recording_changed(_mtm: MainThreadMarker, recording: bool) {
    if let Some(c) = controller()
        && recording
    {
        (c.ivars().host.suspend_hotkeys)(true);
        c.set_status("");
    }
}

fn recorded(_mtm: MainThreadMarker, action: Action, outcome: Outcome) {
    let Some(c) = controller() else {
        return;
    };
    let host = &c.ivars().host;
    let (errors, note) = match outcome {
        Outcome::Cancel => ((host.suspend_hotkeys)(false), String::new()),
        Outcome::Clear => (
            (host.update)(&|config| config.hotkeys.set(action, String::new())),
            String::new(),
        ),
        Outcome::Set(text) => {
            // A shortcut can only trigger one action: take it from any other.
            let config = (host.config)();
            let same = |other: &str| {
                let (a, b) = (
                    crate::shortcut::Shortcut::parse(other),
                    crate::shortcut::Shortcut::parse(&text),
                );
                matches!((a, b), (Ok(a), Ok(b)) if a == b)
            };
            let taken_from: Vec<Action> = Hotkeys::ACTIONS
                .into_iter()
                .filter(|a| *a != action && same(&config.hotkeys.get(*a)))
                .collect();
            let errors = (host.update)(&|config| {
                for other in &taken_from {
                    config.hotkeys.set(*other, String::new());
                }
                config.hotkeys.set(action, text.clone());
            });
            let note = taken_from
                .iter()
                .map(|a| {
                    t!(
                        "Removed it from {action}.",
                        action = crate::i18n::tr(a.title())
                    )
                })
                .collect::<Vec<_>>()
                .join(" ");
            (errors, note)
        }
    };
    c.load_values();
    let mut status = errors.join("\n");
    if !note.is_empty() {
        status = if status.is_empty() {
            note
        } else {
            format!("{note}\n{status}")
        };
    }
    c.set_status(&status);
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    struct SettingsWindow;

    unsafe impl NSObjectProtocol for SettingsWindow {}

    unsafe impl NSToolbarDelegate for SettingsWindow {
        #[unsafe(method_id(toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:))]
        fn toolbar_item(
            &self,
            _toolbar: &NSToolbar,
            identifier: &NSString,
            _will_insert: bool,
        ) -> Option<Retained<NSToolbarItem>> {
            self.toolbar_item_for(identifier)
        }

        #[unsafe(method_id(toolbarDefaultItemIdentifiers:))]
        fn default_items(&self, _toolbar: &NSToolbar) -> Retained<NSArray<NSString>> {
            tab_ids()
        }

        #[unsafe(method_id(toolbarAllowedItemIdentifiers:))]
        fn allowed_items(&self, _toolbar: &NSToolbar) -> Retained<NSArray<NSString>> {
            tab_ids()
        }

        #[unsafe(method_id(toolbarSelectableItemIdentifiers:))]
        fn selectable_items(&self, _toolbar: &NSToolbar) -> Retained<NSArray<NSString>> {
            tab_ids()
        }
    }

    impl SettingsWindow {
        #[unsafe(method(selectTab:))]
        fn select_tab(&self, sender: &NSToolbarItem) {
            if let Some(tab) = Tab::from_id(&sender.itemIdentifier().to_string()) {
                self.show_tab(tab);
            }
        }

        #[unsafe(method(changed:))]
        fn changed(&self, sender: &NSView) {
            self.apply(sender);
        }

        #[unsafe(method(openSettingsFile:))]
        fn open_settings_file(&self, _sender: &AnyObject) {
            let _ = std::process::Command::new("/usr/bin/open")
                .arg("-t")
                .arg(crate::config::config_path())
                .spawn();
        }

        #[unsafe(method(openHistory:))]
        fn open_history(&self, _sender: &AnyObject) {
            crate::history::open_window(self.mtm());
        }
    }
);

fn tab_ids() -> Retained<NSArray<NSString>> {
    let ids: Vec<Retained<NSString>> = Tab::ALL
        .iter()
        .map(|t| NSString::from_str(t.id()))
        .collect();
    NSArray::from_retained_slice(&ids)
}

impl SettingsWindow {
    fn toolbar_item_for(&self, identifier: &NSString) -> Option<Retained<NSToolbarItem>> {
        let tab = Tab::from_id(&identifier.to_string())?;
        let item =
            NSToolbarItem::initWithItemIdentifier(NSToolbarItem::alloc(self.mtm()), identifier);
        item.setLabel(&NSString::from_str(&tr(tab.title())));
        item.setImage(
            NSImage::imageWithSystemSymbolName_accessibilityDescription(
                &NSString::from_str(tab.symbol()),
                Some(&NSString::from_str(&tr(tab.title()))),
            )
            .as_deref(),
        );
        // SAFETY: the action is defined on SettingsWindow, which lives
        // as long as the window.
        unsafe {
            item.setTarget(Some(self));
            item.setAction(Some(sel!(selectTab:)));
        }
        Some(item)
    }

    fn new(mtm: MainThreadMarker, host: Host) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            host,
            window: OnceCell::new(),
            tab: Cell::new(Tab::General),
            bound: RefCell::new(Vec::new()),
            status: RefCell::new(None),
        });
        // SAFETY: NSObject's designated initializer.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        // SAFETY: designated NSWindow initializer.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::ZERO, NSSize::new(560.0, 300.0)),
                NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: the WINDOW thread-local keeps the controller and window.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setToolbarStyle(NSWindowToolbarStyle::Preference);
        let toolbar =
            NSToolbar::initWithIdentifier(NSToolbar::alloc(mtm), ns_string!("CleanShotZSettings"));
        toolbar.setDelegate(Some(ProtocolObject::from_ref(&*this)));
        window.setToolbar(Some(&toolbar));
        window.center();
        let _ = this.ivars().window.set(window);
        this
    }

    fn show_tab(&self, tab: Tab) {
        let mtm = self.mtm();
        let Some(window) = self.ivars().window.get() else {
            return;
        };
        if let Some(toolbar) = window.toolbar() {
            toolbar.setSelectedItemIdentifier(Some(&NSString::from_str(tab.id())));
        }
        // Finish any edit while its binding still exists: a field that
        // loses focus later would report to the next tab's bindings.
        window.makeFirstResponder(None);
        window.setTitle(&NSString::from_str(&tr(tab.title())));
        self.ivars().tab.set(tab);
        self.ivars().bound.borrow_mut().clear();
        *self.ivars().status.borrow_mut() = None;

        let config = (self.ivars().host.config)();
        let mut form = Form::new(self, mtm);
        match tab {
            Tab::General => general(&mut form, &config),
            Tab::Screenshots => screenshots(&mut form),
            Tab::QuickAccess => quick_access(&mut form),
            Tab::Recording => recording(&mut form),
            Tab::History => history(&mut form),
            Tab::Uploads => uploads(&mut form, &config),
            Tab::Watermark => watermark(&mut form, &config),
            Tab::Shortcuts => shortcuts(&mut form, &config),
        }
        let grid = form.finish();
        let size = grid.fittingSize();
        let (pad_x, pad_y) = (28.0, 22.0);
        let content = NSSize::new(
            (size.width + 2.0 * pad_x).max(560.0),
            size.height + 2.0 * pad_y,
        );
        let container =
            NSView::initWithFrame(NSView::alloc(mtm), NSRect::new(NSPoint::ZERO, content));
        grid.setFrame(NSRect::new(
            NSPoint::new(((content.width - size.width) / 2.0).round(), pad_y),
            size,
        ));
        container.addSubview(&grid);

        // Keep the window's top edge in place while its height changes.
        let old = window.frame();
        let mut frame = window.frameRectForContentRect(NSRect::new(NSPoint::ZERO, content));
        let first = window.contentView().is_none_or(|v| v.subviews().is_empty());
        frame.origin = NSPoint::new(
            old.origin.x,
            old.origin.y + old.size.height - frame.size.height,
        );
        window.setContentView(Some(&container));
        window.setFrame_display_animate(frame, true, window.isVisible() && !first);
        self.load_values();
    }

    /// Sets every control from the current settings.
    fn load_values(&self) {
        let config = (self.ivars().host.config)();
        for bound in self.ivars().bound.borrow().iter() {
            let view = &bound.view;
            match &bound.bind {
                Bind::Check(get, _) => button(view).setState(state(get(&config))),
                Bind::LoginItem => button(view).setState(state(login_item::is_enabled())),
                Bind::Updates => {
                    button(view).setState(state(crate::updates::checks_automatically()))
                }
                Bind::Choice { get, .. } => popup(view).selectItemAtIndex(get(&config) as isize),
                Bind::Text(get, _) => {
                    text_field(view).setStringValue(&NSString::from_str(&get(&config)))
                }
                Bind::Number(get, _) => text_field(view)
                    .setStringValue(&NSString::from_str(&format_number(get(&config)))),
                Bind::Secret { name, get, .. } => {
                    let stored = get(&config) == format!("keychain:{name}")
                        && crate::upload::keychain::get(name).is_some();
                    let field = text_field(view);
                    field.setStringValue(ns_string!(""));
                    field.setPlaceholderString(Some(&NSString::from_str(if stored {
                        "Saved in the Keychain"
                    } else {
                        "Not set"
                    })));
                }
                Bind::Folder(get, _) | Bind::ImageFile(get, _) => {
                    text_field(view).setStringValue(&NSString::from_str(&get(&config)))
                }
                Bind::WatermarkPreview => {
                    let image_view = view
                        .downcast_ref::<objc2_app_kit::NSImageView>()
                        .expect("the preview is an image view");
                    let problem = text_field(&bound.control);
                    match crate::watermark::preview(&config.watermark.style) {
                        Ok(image) => {
                            let (w, h) = crate::capture::image::pixel_size(&image);
                            image_view.setImage(Some(&NSImage::initWithCGImage_size(
                                <NSImage as objc2::AllocAnyThread>::alloc(),
                                &image,
                                NSSize::new(w as f64 / 2.0, h as f64 / 2.0),
                            )));
                            problem.setHidden(true);
                        }
                        Err(e) => {
                            image_view.setImage(None);
                            problem.setStringValue(&NSString::from_str(&e));
                            problem.setHidden(false);
                        }
                    }
                }
                Bind::Shortcut(action) => {
                    if let Some(recorder) = view.downcast_ref::<ShortcutRecorder>() {
                        recorder.set_shortcut(&config.hotkeys.get(*action));
                    }
                }
                Bind::ClearShortcut(action) => {
                    button(view).setHidden(config.hotkeys.get(*action).is_empty());
                }
            }
        }
    }

    /// Reads a changed control into the settings.
    fn apply(&self, sender: &NSView) {
        let index = usize::try_from(sender.tag() - 1).ok();
        let (bind, view) = {
            let bound = self.ivars().bound.borrow();
            let Some(b) = index.and_then(|i| bound.get(i)) else {
                return;
            };
            // A control from a tab that is gone.
            if !std::ptr::eq(&*b.control, sender) {
                return;
            }
            (b.bind_copy(), b.view.clone())
        };
        let host = &self.ivars().host;
        let mut errors = Vec::new();
        let mut rebuild = false;
        match bind {
            Bind::Check(_, set) => {
                let on = button(&view).state() == NSControlStateValueOn;
                errors = (host.update)(&|c| set(c, on));
            }
            Bind::Updates => {
                crate::updates::set_checks_automatically(
                    button(&view).state() == NSControlStateValueOn,
                );
            }
            Bind::LoginItem => {
                let on = button(&view).state() == NSControlStateValueOn;
                if let Err(e) = login_item::set_enabled(on) {
                    crate::app::alert::show(
                        self.mtm(),
                        "Could not change Launch at Login",
                        &e,
                        &["OK"],
                    );
                }
            }
            Bind::Choice {
                set, rebuild: r, ..
            } => {
                let i = popup(&view).indexOfSelectedItem().max(0) as usize;
                errors = (host.update)(&|c| set(c, i));
                rebuild = r;
            }
            Bind::Text(_, set) => {
                let value = text_field(&view)
                    .stringValue()
                    .to_string()
                    .trim()
                    .to_string();
                errors = (host.update)(&|c| set(c, value.clone()));
            }
            Bind::Number(_, set) => {
                let text = text_field(&view).stringValue().to_string();
                if let Ok(n) = text.trim().parse::<f64>()
                    && n.is_finite()
                    && n >= 0.0
                {
                    errors = (host.update)(&|c| set(c, n));
                }
            }
            Bind::Secret { name, set, .. } => {
                let value = text_field(&view).stringValue().to_string();
                if !value.is_empty() {
                    match crate::upload::keychain::set(name, &value) {
                        Ok(()) => errors = (host.update)(&|c| set(c, format!("keychain:{name}"))),
                        Err(e) => {
                            crate::app::alert::show(
                                self.mtm(),
                                "Could not save to the Keychain",
                                &e,
                                &["OK"],
                            );
                        }
                    }
                }
            }
            Bind::Folder(_, set) => {
                if let Some(folder) = choose_folder(self.mtm()) {
                    errors = (host.update)(&|c| set(c, folder.clone()));
                }
            }
            Bind::ImageFile(_, set) => {
                if let Some(file) = choose_image(self.mtm()) {
                    errors = (host.update)(&|c| set(c, file.clone()));
                }
            }
            Bind::WatermarkPreview => {}
            Bind::Shortcut(_) => {}
            Bind::ClearShortcut(action) => {
                recorded(self.mtm(), action, Outcome::Clear);
                return;
            }
        }
        if rebuild {
            self.show_tab(self.ivars().tab.get());
        } else {
            self.load_values();
        }
        if !errors.is_empty() {
            self.set_status(&errors.join("\n"));
        }
    }

    fn set_status(&self, text: &str) {
        if let Some(status) = self.ivars().status.borrow().as_ref() {
            status.setStringValue(&NSString::from_str(text));
            status.setHidden(text.is_empty());
        }
    }
}

impl Bound {
    /// The binding without the view (bindings hold only fn pointers).
    fn bind_copy(&self) -> Bind {
        match &self.bind {
            Bind::Check(g, s) => Bind::Check(*g, *s),
            Bind::Choice { get, set, rebuild } => Bind::Choice {
                get: *get,
                set: *set,
                rebuild: *rebuild,
            },
            Bind::Text(g, s) => Bind::Text(*g, *s),
            Bind::Number(g, s) => Bind::Number(*g, *s),
            Bind::Secret { name, get, set } => Bind::Secret {
                name,
                get: *get,
                set: *set,
            },
            Bind::Folder(g, s) => Bind::Folder(*g, *s),
            Bind::ImageFile(g, s) => Bind::ImageFile(*g, *s),
            Bind::WatermarkPreview => Bind::WatermarkPreview,
            Bind::LoginItem => Bind::LoginItem,
            Bind::Updates => Bind::Updates,
            Bind::Shortcut(a) => Bind::Shortcut(*a),
            Bind::ClearShortcut(a) => Bind::ClearShortcut(*a),
        }
    }
}

fn button(view: &NSView) -> &NSButton {
    view.downcast_ref::<NSButton>().expect("bound to a button")
}

fn popup(view: &NSView) -> &NSPopUpButton {
    view.downcast_ref::<NSPopUpButton>()
        .expect("bound to a pop-up button")
}

fn text_field(view: &NSView) -> &NSTextField {
    view.downcast_ref::<NSTextField>()
        .expect("bound to a text field")
}

fn state(on: bool) -> isize {
    if on {
        NSControlStateValueOn
    } else {
        NSControlStateValueOff
    }
}

fn format_number(n: f64) -> String {
    if n.fract() == 0.0 {
        format!("{n:.0}")
    } else {
        format!("{n}")
    }
}

/// Asks for a folder; returns it with the home folder written as `~`.
fn choose_folder(mtm: MainThreadMarker) -> Option<String> {
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setCanChooseDirectories(true);
    panel.setCanChooseFiles(false);
    panel.setCanCreateDirectories(true);
    choose_path(&panel)
}

/// An image file, with `~` for the home folder.
fn choose_image(mtm: MainThreadMarker) -> Option<String> {
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setCanChooseDirectories(false);
    panel.setCanChooseFiles(true);
    choose_path(&panel)
}

fn choose_path(panel: &NSOpenPanel) -> Option<String> {
    panel.setPrompt(Some(ns_string!("Choose")));
    if panel.runModal() != objc2_app_kit::NSModalResponseOK {
        return None;
    }
    let path = panel.URL()?.path()?.to_string();
    let home = std::env::var("HOME").unwrap_or_default();
    Some(match path.strip_prefix(&home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => {
            format!("~{rest}")
        }
        _ => path,
    })
}

/// Builds a two-column grid: labels on the left, controls on the right.
struct Form<'a> {
    owner: &'a SettingsWindow,
    mtm: MainThreadMarker,
    rows: Vec<(Retained<NSView>, Retained<NSView>, f64)>,
    next_padding: f64,
}

const FIELD_WIDTH: f64 = 300.0;

impl<'a> Form<'a> {
    fn new(owner: &'a SettingsWindow, mtm: MainThreadMarker) -> Self {
        Self {
            owner,
            mtm,
            rows: Vec::new(),
            next_padding: 0.0,
        }
    }

    /// Space before the next row, between groups.
    fn gap(&mut self) {
        self.next_padding = 14.0;
    }

    fn row(&mut self, label: &str, view: Retained<NSView>) {
        let label: Retained<NSView> = if label.is_empty() {
            NSGridCell::emptyContentView(self.mtm)
        } else {
            into_view(NSTextField::labelWithString(
                &NSString::from_str(&tr(label)),
                self.mtm,
            ))
        };
        self.rows.push((label, view, self.next_padding));
        self.next_padding = 0.0;
    }

    /// Registers `control` for `bind` and wires its action.
    fn bind(&mut self, control: &NSView, bind: Bind) {
        let mut bound = self.owner.ivars().bound.borrow_mut();
        bound.push(Bound {
            control: control.retain(),
            view: control.retain(),
            bind,
        });
        let tag = bound.len() as isize;
        // SAFETY: every bound view is an NSControl.
        unsafe {
            let _: () = msg_send![control, setTag: tag];
            let _: () = msg_send![control, setTarget: self.owner];
            let _: () = msg_send![control, setAction: sel!(changed:)];
        }
    }

    fn check(&mut self, label: &str, title: &str, get: Get<bool>, set: Set<bool>) {
        let check = checkbox(self.mtm, title);
        self.bind(&check, Bind::Check(get, set));
        self.row(label, into_view(check));
    }

    fn login_item(&mut self, label: &str) {
        let check = checkbox(self.mtm, "Launch CleanShotZ at login");
        self.bind(&check, Bind::LoginItem);
        self.row(label, into_view(check));
    }

    fn updates(&mut self, label: &str) {
        let check = checkbox(self.mtm, "Check for updates automatically");
        self.bind(&check, Bind::Updates);
        self.row(label, into_view(check));
    }

    fn choice(&mut self, label: &str, options: &[&str], get: Get<usize>, set: Set<usize>) {
        self.choice_with(label, options, get, set, false);
    }

    fn choice_with(
        &mut self,
        label: &str,
        options: &[&str],
        get: Get<usize>,
        set: Set<usize>,
        rebuild: bool,
    ) {
        let popup = NSPopUpButton::new(self.mtm);
        for option in options {
            popup.addItemWithTitle(&NSString::from_str(&tr(option)));
        }
        self.bind(&popup, Bind::Choice { get, set, rebuild });
        self.row(label, into_view(popup));
    }

    fn text(&mut self, label: &str, placeholder: &str, get: Get<String>, set: Set<String>) {
        let field = text_field_with(self.mtm, placeholder, FIELD_WIDTH);
        self.bind(&field, Bind::Text(get, set));
        self.row(label, into_view(field));
    }

    fn number(&mut self, label: &str, unit: &str, get: Get<f64>, set: Set<f64>) {
        let field = text_field_with(self.mtm, "", 64.0);
        field.setAlignment(objc2_app_kit::NSTextAlignment::Right);
        self.bind(&field, Bind::Number(get, set));
        let unit = NSTextField::labelWithString(&NSString::from_str(&tr(unit)), self.mtm);
        let row = hstack(&[into_view(field), into_view(unit)]);
        self.row(label, row);
    }

    fn secret(&mut self, label: &str, name: &'static str, get: Get<String>, set: Set<String>) {
        let field = NSSecureTextField::new(self.mtm);
        field
            .widthAnchor()
            .constraintEqualToConstant(FIELD_WIDTH)
            .setActive(true);
        if let Some(cell) = field.cell() {
            cell.setSendsActionOnEndEditing(true);
        }
        self.bind(&field, Bind::Secret { name, get, set });
        self.row(label, into_view(field));
    }

    fn folder(&mut self, label: &str, get: Get<String>, set: Set<String>) {
        let path = NSTextField::labelWithString(ns_string!(""), self.mtm);
        path.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByTruncatingMiddle);
        path.widthAnchor()
            .constraintLessThanOrEqualToConstant(220.0)
            .setActive(true);
        path.setContentCompressionResistancePriority_forOrientation(
            250.0,
            NSLayoutConstraintOrientation::Horizontal,
        );
        let choose = push_button(self.mtm, "Choose…");
        // The label shows the value; the button changes it.
        self.owner.ivars().bound.borrow_mut().push(Bound {
            control: into_view(choose.clone()),
            view: into_view(path.clone()),
            bind: Bind::Folder(get, set),
        });
        let tag = self.owner.ivars().bound.borrow().len() as isize;
        choose.setTag(tag);
        // SAFETY: `changed:` is defined on the owner.
        unsafe {
            choose.setTarget(Some(self.owner));
            choose.setAction(Some(sel!(changed:)));
        }
        self.row(label, hstack(&[into_view(path), into_view(choose)]));
    }

    fn image_file(&mut self, label: &str, get: Get<String>, set: Set<String>) {
        self.folder(label, get, set);
        if let Some(bound) = self.owner.ivars().bound.borrow_mut().last_mut()
            && let Bind::Folder(get, set) = bound.bind
        {
            bound.bind = Bind::ImageFile(get, set);
        }
    }

    fn watermark_preview(&mut self) {
        let image = objc2_app_kit::NSImageView::new(self.mtm);
        image.setImageScaling(objc2_app_kit::NSImageScaling::ScaleProportionallyUpOrDown);
        image
            .widthAnchor()
            .constraintEqualToConstant(320.0)
            .setActive(true);
        image
            .heightAnchor()
            .constraintEqualToConstant(180.0)
            .setActive(true);
        let problem = NSTextField::wrappingLabelWithString(ns_string!(""), self.mtm);
        problem.setFont(Some(&NSFont::systemFontOfSize(
            NSFont::smallSystemFontSize(),
        )));
        problem.setTextColor(Some(&NSColor::secondaryLabelColor()));
        problem.setPreferredMaxLayoutWidth(FIELD_WIDTH);
        self.owner.ivars().bound.borrow_mut().push(Bound {
            control: into_view(problem.clone()),
            view: into_view(image.clone()),
            bind: Bind::WatermarkPreview,
        });
        let stack = NSStackView::stackViewWithViews(
            &NSArray::from_retained_slice(&[into_view(image), into_view(problem)]),
            self.mtm,
        );
        stack.setOrientation(objc2_app_kit::NSUserInterfaceLayoutOrientation::Vertical);
        stack.setAlignment(objc2_app_kit::NSLayoutAttribute::Leading);
        // No label: rows line up on their first baseline, which here is
        // the problem text under the picture.
        self.row("", into_view(stack));
    }

    fn shortcut(&mut self, action: Action, current: &str) {
        let recorder = ShortcutRecorder::new(self.mtm, action, current);
        let recorder_view = into_view(Retained::into_super(recorder.clone()));
        self.owner.ivars().bound.borrow_mut().push(Bound {
            control: recorder_view.clone(),
            view: recorder_view,
            bind: Bind::Shortcut(recorder.action()),
        });
        let clear = NSButton::new(self.mtm);
        clear.setBordered(false);
        clear.setImage(
            NSImage::imageWithSystemSymbolName_accessibilityDescription(
                ns_string!("xmark.circle.fill"),
                Some(ns_string!("Remove shortcut")),
            )
            .as_deref(),
        );
        clear.setToolTip(Some(ns_string!("Remove shortcut")));
        self.bind(&clear, Bind::ClearShortcut(action));
        let label = format!("{}:", tr(action.title()).trim_end_matches('…'));
        self.row(
            &label,
            hstack(&[into_view(Retained::into_super(recorder)), into_view(clear)]),
        );
    }

    fn button(&mut self, label: &str, title: &str, action: objc2::runtime::Sel) {
        let button = push_button(self.mtm, title);
        // SAFETY: the action is defined on the owner.
        unsafe {
            button.setTarget(Some(self.owner));
            button.setAction(Some(action));
        }
        self.row(label, into_view(button));
    }

    /// Small grey text under the previous control.
    fn note(&mut self, text: &str) {
        let note = NSTextField::wrappingLabelWithString(&NSString::from_str(&tr(text)), self.mtm);
        note.setFont(Some(&NSFont::systemFontOfSize(
            NSFont::smallSystemFontSize(),
        )));
        note.setTextColor(Some(&NSColor::secondaryLabelColor()));
        note.setPreferredMaxLayoutWidth(FIELD_WIDTH);
        self.row("", into_view(note));
    }

    /// Red text for errors (hidden while empty).
    fn status(&mut self) {
        let status = NSTextField::wrappingLabelWithString(ns_string!(""), self.mtm);
        status.setFont(Some(&NSFont::systemFontOfSize(
            NSFont::smallSystemFontSize(),
        )));
        status.setTextColor(Some(&NSColor::systemRedColor()));
        status.setPreferredMaxLayoutWidth(FIELD_WIDTH);
        status.setHidden(true);
        *self.owner.ivars().status.borrow_mut() = Some(status.clone());
        self.row("", into_view(status));
    }

    fn finish(self) -> Retained<NSGridView> {
        let rows: Vec<Retained<NSArray<NSView>>> = self
            .rows
            .iter()
            .map(|(label, view, _)| NSArray::from_retained_slice(&[label.clone(), view.clone()]))
            .collect();
        let grid = NSGridView::gridViewWithViews(&NSArray::from_retained_slice(&rows), self.mtm);
        grid.setRowSpacing(8.0);
        grid.setColumnSpacing(10.0);
        grid.setRowAlignment(NSGridRowAlignment::FirstBaseline);
        grid.columnAtIndex(0)
            .setXPlacement(NSGridCellPlacement::Trailing);
        grid.columnAtIndex(1)
            .setXPlacement(NSGridCellPlacement::Leading);
        for (i, (_, _, padding)) in self.rows.iter().enumerate() {
            if *padding > 0.0 {
                grid.rowAtIndex(i as isize).setTopPadding(*padding);
            }
        }
        grid
    }
}

fn into_view<T: AsRef<NSView>>(view: Retained<T>) -> Retained<NSView> {
    (*view).as_ref().retain()
}

fn checkbox(mtm: MainThreadMarker, title: &str) -> Retained<NSButton> {
    // SAFETY: target and action are set when the checkbox is bound.
    unsafe {
        NSButton::checkboxWithTitle_target_action(&NSString::from_str(&tr(title)), None, None, mtm)
    }
}

fn push_button(mtm: MainThreadMarker, title: &str) -> Retained<NSButton> {
    // SAFETY: target and action are set by the caller.
    unsafe {
        NSButton::buttonWithTitle_target_action(&NSString::from_str(&tr(title)), None, None, mtm)
    }
}

fn text_field_with(mtm: MainThreadMarker, placeholder: &str, width: f64) -> Retained<NSTextField> {
    let field = NSTextField::new(mtm);
    field.setPlaceholderString(Some(&NSString::from_str(&tr(placeholder))));
    field
        .widthAnchor()
        .constraintEqualToConstant(width)
        .setActive(true);
    // Apply when the field loses focus too, not only on Return.
    if let Some(cell) = field.cell() {
        cell.setSendsActionOnEndEditing(true);
    }
    field
}

fn hstack(views: &[Retained<NSView>]) -> Retained<NSView> {
    let stack =
        NSStackView::stackViewWithViews(&NSArray::from_retained_slice(views), views[0].mtm());
    stack.setSpacing(6.0);
    stack.setAlignment(objc2_app_kit::NSLayoutAttribute::FirstBaseline);
    into_view(stack)
}

// The tabs. Choice getters map settings to menu positions.

fn general(f: &mut Form, config: &Config) {
    f.login_item("Startup:");
    if crate::updates::available() {
        f.updates("Updates:");
    }
    f.gap();
    f.check(
        "After capture:",
        "Copy to the clipboard",
        |c| c.capture.copy_to_clipboard,
        |c, v| c.capture.copy_to_clipboard = v,
    );
    f.check(
        "",
        "Save to a file",
        |c| c.capture.save_to_file,
        |c, v| c.capture.save_to_file = v,
    );
    f.check(
        "",
        "Play a sound",
        |c| c.capture.play_sound,
        |c, v| c.capture.play_sound = v,
    );
    pipeline_choice(f, config);
    f.gap();
    f.folder(
        "Save to:",
        |c| c.capture.save_directory.clone(),
        |c, v| c.capture.save_directory = v,
    );
    f.text(
        "File name:",
        "CleanShotZ {date} at {time}",
        |c| c.capture.file_name.clone(),
        |c, v| {
            if !v.is_empty() {
                c.capture.file_name = v
            }
        },
    );
    f.note("{date} and {time} are replaced with the capture's date and time.");
    f.gap();
    f.button(
        "Settings file:",
        "Open in Text Editor",
        sel!(openSettingsFile:),
    );
    f.note(
        "Every setting is also in this file. After editing it, choose Reload Settings in the menu.",
    );
}

/// "Run pipeline" with the pipelines from the settings file.
fn pipeline_choice(f: &mut Form, config: &Config) {
    if config.pipelines.is_empty() {
        f.note("Or run a pipeline of steps instead: add one to [pipelines] in the settings file.");
        return;
    }
    let mut options = vec!["None"];
    options.extend(config.pipelines.keys().map(String::as_str));
    f.choice(
        "Instead, run:",
        &options,
        |c| {
            c.pipelines
                .keys()
                .position(|k| *k == c.capture.pipeline)
                .map_or(0, |i| i + 1)
        },
        |c, i| {
            c.capture.pipeline = match i {
                0 => String::new(),
                i => c.pipelines.keys().nth(i - 1).cloned().unwrap_or_default(),
            }
        },
    );
    f.note("A pipeline replaces copying, saving and the thumbnail above.");
}

fn screenshots(f: &mut Form) {
    f.choice(
        "Format:",
        &["PNG", "JPEG", "WebP"],
        |c| match c.capture.format {
            ImageFormat::Png => 0,
            ImageFormat::Jpeg => 1,
            ImageFormat::Webp => 2,
        },
        |c, i| {
            c.capture.format = [ImageFormat::Png, ImageFormat::Jpeg, ImageFormat::Webp][i.min(2)]
        },
    );
    f.number(
        "JPEG quality:",
        "%",
        |c| (c.capture.jpeg_quality() * 100.0).round(),
        |c, v| c.capture.jpeg_quality = (v / 100.0).clamp(0.0, 1.0),
    );
    f.number(
        "WebP quality:",
        "% (100 is lossless)",
        |c| (c.capture.quality(ImageFormat::Webp) * 100.0).round(),
        |c, v| c.capture.webp_quality = (v / 100.0).clamp(0.0, 1.0),
    );
    f.check(
        "",
        "Save Retina screenshots at 1x",
        |c| c.capture.downscale_retina,
        |c, v| c.capture.downscale_retina = v,
    );
    f.check(
        "",
        "Crop the notch from full-screen apps",
        |c| c.capture.crop_notch,
        |c, v| c.capture.crop_notch = v,
    );
    f.check(
        "",
        "Convert colors to sRGB",
        |c| c.capture.convert_to_srgb,
        |c, v| c.capture.convert_to_srgb = v,
    );
    f.gap();
    f.check(
        "Capture:",
        "Show the mouse pointer",
        |c| c.capture.show_cursor,
        |c, v| c.capture.show_cursor = v,
    );
    f.check(
        "",
        "Freeze the screen while selecting",
        |c| c.capture.freeze_screen,
        |c, v| c.capture.freeze_screen = v,
    );
    f.check(
        "",
        "Show the magnifier",
        |c| c.capture.show_magnifier,
        |c, v| c.capture.show_magnifier = v,
    );
    f.check(
        "",
        "Hide desktop icons and widgets",
        |c| c.capture.hide_desktop_icons,
        |c, v| c.capture.hide_desktop_icons = v,
    );
    f.check(
        "",
        "Include window shadows",
        |c| c.capture.window_shadow,
        |c, v| c.capture.window_shadow = v,
    );
    f.number(
        "Self-timer:",
        "seconds",
        |c| c.capture.self_timer_seconds() as f64,
        |c, v| c.capture.self_timer_seconds = (v as u32).clamp(1, 60),
    );
    f.gap();
    f.check(
        "Text recognition:",
        "Keep line breaks",
        |c| c.text.keep_line_breaks,
        |c, v| c.text.keep_line_breaks = v,
    );
    f.check(
        "",
        "Correct words with a dictionary",
        |c| c.text.language_correction,
        |c, v| c.text.language_correction = v,
    );
    f.text(
        "Languages:",
        "System languages",
        |c| c.text.languages.join(", "),
        |c, v| {
            c.text.languages = v
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        },
    );
    f.note("Codes such as en-US, de-DE, ja-JP, in order of preference.");
}

fn quick_access(f: &mut Form) {
    f.check(
        "",
        "Show a thumbnail after each capture",
        |c| c.quick_access.enabled,
        |c, v| c.quick_access.enabled = v,
    );
    f.choice(
        "Position:",
        &["Bottom left", "Bottom right"],
        |c| (c.quick_access.position == Side::Right) as usize,
        |c, i| c.quick_access.position = if i == 1 { Side::Right } else { Side::Left },
    );
    f.number(
        "Close after:",
        "seconds (0 keeps them)",
        |c| c.quick_access.auto_close_seconds as f64,
        |c, v| c.quick_access.auto_close_seconds = v as u32,
    );
    f.check(
        "",
        "Close after dragging into an app",
        |c| c.quick_access.close_after_drag,
        |c, v| c.quick_access.close_after_drag = v,
    );
}

fn recording(f: &mut Form) {
    f.choice(
        "Format:",
        &["MP4", "GIF"],
        |c| (c.recording.format == VideoFormat::Gif) as usize,
        |c, i| {
            c.recording.format = if i == 1 {
                VideoFormat::Gif
            } else {
                VideoFormat::Mp4
            }
        },
    );
    f.choice(
        "Video codec:",
        &["Automatic", "H.264", "HEVC"],
        |c| match c.recording.codec {
            Codec::Auto => 0,
            Codec::H264 => 1,
            Codec::Hevc => 2,
        },
        |c, i| c.recording.codec = [Codec::Auto, Codec::H264, Codec::Hevc][i.min(2)],
    );
    f.choice(
        "Quality:",
        &["Low", "Medium", "High"],
        |c| match c.recording.quality {
            Quality::Low => 0,
            Quality::Medium => 1,
            Quality::High => 2,
        },
        |c, i| c.recording.quality = [Quality::Low, Quality::Medium, Quality::High][i.min(2)],
    );
    f.number(
        "Frame rate:",
        "fps",
        |c| c.recording.fps() as f64,
        |c, v| c.recording.fps = (v as u32).clamp(1, 120),
    );
    f.check(
        "",
        "Record Retina screens at 1x",
        |c| c.recording.downscale_retina,
        |c, v| c.recording.downscale_retina = v,
    );
    f.number(
        "Countdown:",
        "seconds",
        |c| c.recording.countdown_seconds as f64,
        |c, v| c.recording.countdown_seconds = (v as u32).min(10),
    );
    f.check(
        "",
        "Choose the camera and microphone before recording",
        |c| c.recording.choose_devices,
        |c, v| c.recording.choose_devices = v,
    );
    f.check(
        "Studio:",
        "Record in Studio mode",
        |c| c.recording.studio,
        |c, v| c.recording.studio = v,
    );
    f.note(
        "The pointer, clicks, keys and camera are kept apart, so zooms, the cursor, a background and the camera can be changed after recording.",
    );
    f.gap();
    f.check(
        "Show:",
        "Mouse pointer",
        |c| c.recording.show_cursor,
        |c, v| c.recording.show_cursor = v,
    );
    f.check(
        "",
        "Clicks",
        |c| c.recording.show_clicks,
        |c, v| c.recording.show_clicks = v,
    );
    f.check(
        "",
        "Cursor highlight",
        |c| c.recording.highlight_cursor,
        |c, v| c.recording.highlight_cursor = v,
    );
    f.choice(
        "Keystrokes:",
        &["Off", "Shortcuts only", "All keys"],
        |c| match c.recording.keystrokes {
            KeystrokeMode::Off => 0,
            KeystrokeMode::Shortcuts => 1,
            KeystrokeMode::All => 2,
        },
        |c, i| {
            c.recording.keystrokes = [
                KeystrokeMode::Off,
                KeystrokeMode::Shortcuts,
                KeystrokeMode::All,
            ][i.min(2)]
        },
    );
    f.gap();
    f.check(
        "Audio:",
        "System audio",
        |c| c.recording.system_audio,
        |c, v| c.recording.system_audio = v,
    );
    f.check(
        "",
        "Microphone",
        |c| c.recording.microphone,
        |c, v| c.recording.microphone = v,
    );
    f.check(
        "",
        "Keep them on separate tracks",
        |c| c.recording.separate_audio_tracks,
        |c, v| c.recording.separate_audio_tracks = v,
    );
    f.note("Otherwise they are mixed into one track, which every player plays.");
    f.gap();
    f.check(
        "Notifications:",
        "Hide notification banners in recordings",
        |c| c.recording.hide_notifications,
        |c, v| c.recording.hide_notifications = v,
    );
    f.gap();
    f.check(
        "Camera:",
        "Show the camera",
        |c| c.recording.camera,
        |c, v| c.recording.camera = v,
    );
    f.choice(
        "Camera shape:",
        &["Circle", "Rounded rectangle"],
        |c| (c.recording.camera_shape == CameraShape::Rounded) as usize,
        |c, i| {
            c.recording.camera_shape = if i == 1 {
                CameraShape::Rounded
            } else {
                CameraShape::Circle
            }
        },
    );
    f.number(
        "Camera size:",
        "points",
        |c| c.recording.camera_size as f64,
        |c, v| c.recording.camera_size = (v as u32).clamp(80, 600),
    );
    f.gap();
    f.number(
        "GIF frame rate:",
        "fps",
        |c| c.recording.gif_fps() as f64,
        |c, v| c.recording.gif_fps = (v as u32).clamp(1, 50),
    );
    f.number(
        "GIF width:",
        "pixels at most",
        |c| c.recording.gif_max_width as f64,
        |c, v| c.recording.gif_max_width = (v as u32).clamp(100, 3840),
    );
}

fn history(f: &mut Form) {
    f.check(
        "",
        "Keep a history of captures",
        |c| c.history.enabled,
        |c, v| c.history.enabled = v,
    );
    f.number(
        "Keep for:",
        "days (0 keeps them; starred ones are always kept)",
        |c| c.history.retention_days as f64,
        |c, v| c.history.retention_days = v as u32,
    );
    f.check(
        "",
        "Recognize text so it can be searched",
        |c| c.history.recognize_text,
        |c, v| c.history.recognize_text = v,
    );
    f.gap();
    f.button("", "Open History…", sel!(openHistory:));
}

fn watermark(f: &mut Form, config: &Config) {
    use crate::watermark::{Kind, Position};
    f.check(
        "Add to new:",
        "Screenshots",
        |c| c.watermark.screenshots,
        |c, v| c.watermark.screenshots = v,
    );
    f.check(
        "",
        "Recordings and Studio exports",
        |c| c.watermark.recordings,
        |c, v| c.watermark.recordings = v,
    );
    f.gap();
    f.choice_with(
        "Show:",
        &["Text", "Image"],
        |c| (c.watermark.style.kind == Kind::Image) as usize,
        |c, v| {
            c.watermark.style.kind = if v == 1 { Kind::Image } else { Kind::Text };
        },
        true,
    );
    if config.watermark.style.kind == Kind::Image {
        f.image_file(
            "Image:",
            |c| c.watermark.style.image.clone(),
            |c, v| c.watermark.style.image = v,
        );
        f.note("A PNG with a transparent background looks best.");
    } else {
        f.text(
            "Text:",
            "© Your Name",
            |c| c.watermark.style.text.clone(),
            |c, v| c.watermark.style.text = v,
        );
        f.text(
            "Color:",
            "#FFFFFF",
            |c| c.watermark.style.color.clone(),
            |c, v| {
                if !v.trim().is_empty() {
                    c.watermark.style.color = v
                }
            },
        );
        f.check(
            "",
            "Dark background behind the text",
            |c| c.watermark.style.background,
            |c, v| c.watermark.style.background = v,
        );
    }
    f.gap();
    let positions: Vec<&str> = Position::ALL.iter().map(|p| p.title()).collect();
    f.choice(
        "Position:",
        &positions,
        |c| {
            Position::ALL
                .iter()
                .position(|p| *p == c.watermark.style.position)
                .unwrap_or(8)
        },
        |c, v| c.watermark.style.position = Position::ALL[v.min(8)],
    );
    f.number(
        "Size:",
        "% of the width",
        |c| c.watermark.style.size,
        |c, v| c.watermark.style.size = v.clamp(1.0, 100.0),
    );
    f.number(
        "Opacity:",
        "%",
        |c| (c.watermark.style.opacity * 100.0).round(),
        |c, v| c.watermark.style.opacity = (v / 100.0).clamp(0.05, 1.0),
    );
    f.number(
        "Margin:",
        "% of the shorter side",
        |c| c.watermark.style.margin,
        |c, v| c.watermark.style.margin = v.clamp(0.0, 25.0),
    );
    f.gap();
    f.watermark_preview();
    f.note("For a single capture, use Add Watermark in the editor or the Watermark checkbox in the video editor.");
}

fn uploads(f: &mut Form, config: &Config) {
    f.choice_with(
        "Upload to:",
        &[
            "Nowhere (off)",
            "HTTP endpoint",
            "S3-compatible storage",
            "Script",
            "Imgur",
        ],
        |c| match c.upload.destination {
            Destination::None => 0,
            Destination::Http => 1,
            Destination::S3 => 2,
            Destination::Script => 3,
            Destination::Imgur => 4,
        },
        |c, i| {
            c.upload.destination = [
                Destination::None,
                Destination::Http,
                Destination::S3,
                Destination::Script,
                Destination::Imgur,
            ][i.min(4)]
        },
        true,
    );
    if config.upload.destination == Destination::None {
        f.note("Uploaded captures get a link that is copied to the clipboard.");
        return;
    }
    f.check(
        "",
        "Upload every capture automatically",
        |c| c.upload.auto_upload,
        |c, v| c.upload.auto_upload = v,
    );
    f.gap();
    match config.upload.destination {
        Destination::Imgur => {
            f.text(
                "Client ID:",
                "From api.imgur.com/oauth2/addclient",
                |c| c.upload.imgur.client_id.clone(),
                |c, v| c.upload.imgur.client_id = v,
            );
            f.note(
                "Imgur asks every app for its own client ID. Register one (choose \"anonymous usage without user authorization\"; no callback URL needed) and paste it here. Uploads are anonymous and public to anyone with the link.",
            );
        }
        Destination::Http => {
            f.text(
                "URL:",
                "https://example.com/upload",
                |c| c.upload.http.url.clone(),
                |c, v| c.upload.http.url = v,
            );
            f.choice(
                "Method:",
                &["POST (form upload)", "PUT (file as body)"],
                |c| c.upload.http.method.eq_ignore_ascii_case("PUT") as usize,
                |c, i| c.upload.http.method = if i == 1 { "PUT" } else { "POST" }.into(),
            );
            f.text(
                "File field:",
                "file",
                |c| c.upload.http.field.clone(),
                |c, v| c.upload.http.field = if v.is_empty() { "file".into() } else { v },
            );
            f.text(
                "Link:",
                "text",
                |c| c.upload.http.link.clone(),
                |c, v| c.upload.http.link = if v.is_empty() { "text".into() } else { v },
            );
            f.note("text (the response), json:path.to.link, or header:Location.");
            f.secret(
                "Authorization:",
                "http-authorization",
                |c| {
                    c.upload
                        .http
                        .headers
                        .get("Authorization")
                        .cloned()
                        .unwrap_or_default()
                },
                |c, v| {
                    c.upload.http.headers.insert("Authorization".into(), v);
                },
            );
            f.note("The whole header value, for example Bearer abc123. Other headers and form fields can be set in the settings file.");
        }
        Destination::S3 => {
            f.text(
                "Bucket:",
                "",
                |c| c.upload.s3.bucket.clone(),
                |c, v| c.upload.s3.bucket = v,
            );
            f.text(
                "Region:",
                "us-east-1",
                |c| c.upload.s3.region.clone(),
                |c, v| c.upload.s3.region = if v.is_empty() { "us-east-1".into() } else { v },
            );
            f.text(
                "Endpoint:",
                "Amazon S3",
                |c| c.upload.s3.endpoint.clone(),
                |c, v| c.upload.s3.endpoint = v,
            );
            f.note(
                "For R2, MinIO and others, for example https://<account>.r2.cloudflarestorage.com.",
            );
            f.text(
                "Access key ID:",
                "",
                |c| c.upload.s3.access_key_id.clone(),
                |c, v| c.upload.s3.access_key_id = v,
            );
            f.secret(
                "Secret access key:",
                "s3-secret",
                |c| c.upload.s3.secret_access_key.clone(),
                |c, v| c.upload.s3.secret_access_key = v,
            );
            f.text(
                "Folder:",
                "screenshots/",
                |c| c.upload.s3.prefix.clone(),
                |c, v| c.upload.s3.prefix = v,
            );
            f.text(
                "Public URL:",
                "The bucket's URL",
                |c| c.upload.s3.public_url.clone(),
                |c, v| c.upload.s3.public_url = v,
            );
            f.text(
                "ACL:",
                "None",
                |c| c.upload.s3.acl.clone(),
                |c, v| c.upload.s3.acl = v,
            );
        }
        Destination::Script => {
            f.text(
                "Command:",
                "~/bin/upload.sh",
                |c| c.upload.script.command.clone(),
                |c, v| c.upload.script.command = v,
            );
            f.note(
                "Runs with the file's path as its argument. The first line it prints is the link.",
            );
        }
        Destination::None => {}
    }
}

fn shortcuts(f: &mut Form, config: &Config) {
    for action in Hotkeys::ACTIONS {
        f.shortcut(action, &config.hotkeys.get(action));
    }
    f.gap();
    f.note("Click a shortcut, then press the new keys. Escape cancels; Delete removes it.");
    f.status();
}
