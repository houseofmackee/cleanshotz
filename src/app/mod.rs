//! The menu bar application: `NSApplication` setup, the status item menu,
//! global hotkeys and first-launch onboarding.

pub(crate) mod alert;

use std::cell::{OnceCell, RefCell};
use std::path::PathBuf;
use std::process::Command;

use objc2::rc::Retained;
use objc2::runtime::{NSObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSControlStateValueOff,
    NSControlStateValueOn, NSEventModifierFlags, NSImage, NSMenu, NSMenuDelegate, NSMenuItem,
    NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{NSArray, NSNotification, NSObjectProtocol, NSString, NSURL, ns_string};

use crate::action::Action;
use crate::capture::{self, flow};
use crate::config::Config;
use crate::platform::hotkeys::Hotkeys;
use crate::platform::login_item;
use crate::platform::permissions::{Permission, Status};
use crate::shortcut::Shortcut;

pub fn run(config: Config, config_path: PathBuf) {
    let mtm = MainThreadMarker::new().expect("the app must start on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    // Menu bar only: no Dock icon. Also set in Info.plist (LSUIElement), but
    // this covers running the bare binary during development.
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let delegate = AppDelegate::new(mtm, config, config_path);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.run();
}

pub struct Ivars {
    config_path: PathBuf,
    config: RefCell<Config>,
    status_item: OnceCell<Retained<NSStatusItem>>,
    hotkeys: OnceCell<RefCell<Hotkeys>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and AppDelegate does
    // not implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            capture::output::clear_temp_dir();
            self.configure_quick_access();
            self.install_status_item();
            self.install_hotkeys();
            self.run_onboarding_if_needed();
            tracing::info!("CleanShotZ started");
        }

        /// `cleanshotz://<command>` URLs, for example
        /// `open cleanshotz://capture-area`.
        #[unsafe(method(application:openURLs:))]
        fn open_urls(&self, _application: &NSApplication, urls: &NSArray<NSURL>) {
            for url in urls.iter() {
                let command = url.host().map(|h| h.to_string()).unwrap_or_default();
                match Action::from_url_command(&command) {
                    Some(action) => {
                        tracing::info!(?action, "URL command");
                        self.perform(action);
                    }
                    None => tracing::warn!(url = %url.absoluteString().unwrap_or_default(), "unknown URL command"),
                }
            }
        }
    }

    unsafe impl NSMenuDelegate for AppDelegate {
        /// Rebuilt on every open so permission and login item states are
        /// always current.
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, menu: &NSMenu) {
            self.populate_menu(menu);
        }
    }

    impl AppDelegate {
        #[unsafe(method(performAction:))]
        fn perform_action(&self, sender: &NSMenuItem) {
            if let Some(action) = Action::from_tag(sender.tag()) {
                self.perform(action);
            }
        }
    }
);

impl AppDelegate {
    fn new(mtm: MainThreadMarker, config: Config, config_path: PathBuf) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            config_path,
            config: RefCell::new(config),
            status_item: OnceCell::new(),
            hotkeys: OnceCell::new(),
        });
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }

    fn install_status_item(&self) {
        let mtm = self.mtm();
        let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        if let Some(button) = item.button(mtm) {
            let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
                ns_string!("camera.viewfinder"),
                Some(ns_string!("CleanShotZ")),
            );
            if let Some(image) = image {
                image.setTemplate(true);
                button.setImage(Some(&image));
            } else {
                button.setTitle(ns_string!("CSZ"));
            }
        }
        let menu = NSMenu::new(mtm);
        menu.setAutoenablesItems(false);
        menu.setDelegate(Some(ProtocolObject::from_ref(self)));
        item.setMenu(Some(&menu));
        let _ = self.ivars().status_item.set(item);
    }

    fn install_hotkeys(&self) {
        let this = self.retain();
        let hotkeys = Hotkeys::new(move |id| {
            if let Some(action) = Action::from_tag(id as isize) {
                tracing::debug!(?action, "hotkey pressed");
                this.perform(action);
            }
        });
        let _ = self.ivars().hotkeys.set(RefCell::new(hotkeys));
        let errors = self.register_hotkeys();
        if !errors.is_empty() {
            self.show_hotkey_errors(&errors);
        }
    }

    /// (Re)registers all configured hotkeys and returns human-readable errors.
    fn register_hotkeys(&self) -> Vec<String> {
        let Some(hotkeys) = self.ivars().hotkeys.get() else {
            return Vec::new();
        };
        let mut hotkeys = hotkeys.borrow_mut();
        hotkeys.unregister_all();
        let config = self.ivars().config.borrow();
        let mut errors = Vec::new();
        for (action, text) in config.hotkeys.bindings() {
            let result = Shortcut::parse(text)
                .map_err(|e| e.to_string())
                .and_then(|s| {
                    hotkeys
                        .register(&s, action.tag() as u32)
                        .map_err(|e| e.to_string())
                });
            match result {
                Ok(()) => tracing::info!(?action, shortcut = text, "hotkey registered"),
                Err(e) => {
                    tracing::warn!(?action, shortcut = text, error = %e, "hotkey not registered");
                    errors.push(format!("{} ({text}): {e}", action.title()));
                }
            }
        }
        errors
    }

    fn show_hotkey_errors(&self, errors: &[String]) {
        alert::show(
            self.mtm(),
            "Some shortcuts could not be registered",
            &format!(
                "{}\n\nEdit the [hotkeys] section of the settings file, then choose Reload Settings.",
                errors.join("\n")
            ),
            &["OK"],
        );
    }

    fn run_onboarding_if_needed(&self) {
        if std::env::var_os("CLEANSHOTZ_SKIP_ONBOARDING").is_some()
            || self.ivars().config.borrow().general.onboarding_completed
        {
            return;
        }
        if Permission::ScreenRecording.status() != Status::Granted {
            let choice = alert::show(
                self.mtm(),
                "Welcome to CleanShotZ",
                "CleanShotZ needs Screen Recording permission to capture your screen.\n\n\
                 After granting it in System Settings, macOS may ask you to quit and reopen the app.\n\n\
                 Other permissions (Accessibility, Microphone, Camera) are only needed for some \
                 features and can be granted later from the Permissions menu.",
                &["Grant Access", "Later"],
            );
            if choice == 0 {
                Permission::ScreenRecording.request();
            }
        }
        self.update_config(|c| c.general.onboarding_completed = true);
    }

    fn update_config(&self, change: impl FnOnce(&mut Config)) {
        let ivars = self.ivars();
        let mut config = ivars.config.borrow_mut();
        change(&mut config);
        if let Err(e) = config.save(&ivars.config_path) {
            tracing::error!(error = %e, "could not save settings");
        }
    }

    fn reload_settings(&self) {
        let ivars = self.ivars();
        match Config::load_or_create(&ivars.config_path) {
            Ok(config) => {
                *ivars.config.borrow_mut() = config;
                self.configure_quick_access();
                let errors = self.register_hotkeys();
                if !errors.is_empty() {
                    self.show_hotkey_errors(&errors);
                }
                tracing::info!("settings reloaded");
            }
            Err(e) => {
                tracing::error!(error = %e, "could not reload settings");
                alert::show(
                    self.mtm(),
                    "Settings were not reloaded",
                    &format!("{e}\n\nThe previous settings are still in use."),
                    &["OK"],
                );
            }
        }
    }

    fn perform(&self, action: Action) {
        if let Some(milestone) = action.pending_milestone() {
            tracing::info!(?action, milestone, "action not implemented yet");
            alert::show(
                self.mtm(),
                &format!("{} is not available yet", action.title()),
                &format!("This feature is planned for milestone {milestone} (see PLAN.md)."),
                &["OK"],
            );
            return;
        }
        let mtm = self.mtm();
        match action {
            Action::CaptureArea
            | Action::CapturePreviousArea
            | Action::CaptureFullscreen
            | Action::CaptureWindow
            | Action::SelfTimer => self.capture(action),
            Action::OpenSettingsFile => {
                // `open -t` uses the default text editor regardless of how
                // .toml files are associated.
                let path = &self.ivars().config_path;
                if let Err(e) = Command::new("/usr/bin/open").arg("-t").arg(path).spawn() {
                    tracing::error!(error = %e, "could not open settings file");
                }
            }
            Action::ReloadSettings => self.reload_settings(),
            Action::RestoreRecentlyClosed => crate::quick_access::restore_recently_closed(mtm),
            Action::UnlockPins => crate::pin::unlock_all(),
            Action::CloseAllPins => crate::pin::close_all(),
            Action::ToggleLaunchAtLogin => {
                let enable = !login_item::is_enabled();
                if let Err(e) = login_item::set_enabled(enable) {
                    tracing::error!(error = %e, enable, "could not change login item");
                    alert::show(mtm, "Could not change Launch at Login", &e, &["OK"]);
                }
            }
            Action::PermissionScreenRecording => Permission::ScreenRecording.request(),
            Action::PermissionAccessibility => Permission::Accessibility.request(),
            Action::PermissionMicrophone => Permission::Microphone.request(),
            Action::PermissionCamera => Permission::Camera.request(),
            Action::About => {
                let app = NSApplication::sharedApplication(mtm);
                app.activate();
                app.orderFrontStandardAboutPanel(None);
            }
            Action::Quit => NSApplication::sharedApplication(mtm).terminate(None),
            _ => unreachable!("{action:?} has a pending milestone"),
        }
    }

    fn configure_quick_access(&self) {
        let config = self.ivars().config.borrow();
        crate::quick_access::configure(config.quick_access.clone(), config.capture.clone());
    }

    fn capture(&self, action: Action) {
        let mtm = self.mtm();
        if Permission::ScreenRecording.status() != Status::Granted {
            let choice = alert::show(
                mtm,
                "Screen Recording permission is needed",
                "Allow CleanShotZ in System Settings > Privacy & Security > Screen & System Audio \
                 Recording, then try again. macOS may ask you to reopen the app.",
                &["Open System Settings", "Cancel"],
            );
            if choice == 0 {
                Permission::ScreenRecording.request();
            }
            return;
        }
        let settings = self.ivars().config.borrow().capture.clone();
        let state_path = crate::state::state_path();
        let result = match action {
            Action::CaptureFullscreen => capture::capture_fullscreen(mtm, &settings),
            Action::CapturePreviousArea => {
                flow::capture_previous_area(mtm, settings, state_path, show_capture_error)
            }
            Action::CaptureArea => flow::start(
                mtm,
                flow::Kind::Area,
                settings,
                state_path,
                show_capture_error,
            ),
            Action::CaptureWindow => flow::start(
                mtm,
                flow::Kind::Window,
                settings,
                state_path,
                show_capture_error,
            ),
            Action::SelfTimer => flow::start(
                mtm,
                flow::Kind::SelfTimer,
                settings,
                state_path,
                show_capture_error,
            ),
            _ => unreachable!("{action:?} is not a capture action"),
        };
        if let Err(e) = result {
            show_capture_error(mtm, e);
        }
    }

    fn populate_menu(&self, menu: &NSMenu) {
        let mtm = self.mtm();
        menu.removeAllItems();

        for action in [
            Action::CaptureArea,
            Action::CapturePreviousArea,
            Action::CaptureFullscreen,
            Action::CaptureWindow,
            Action::SelfTimer,
            Action::CaptureScrolling,
            Action::CaptureText,
        ] {
            menu.addItem(&self.menu_item(action));
        }
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&self.menu_item(Action::RecordScreen));
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&self.menu_item(Action::OpenHistory));
        let restore = self.menu_item(Action::RestoreRecentlyClosed);
        restore.setEnabled(crate::quick_access::has_recently_closed());
        menu.addItem(&restore);
        if crate::pin::count() > 0 {
            if crate::pin::any_locked() {
                menu.addItem(&self.menu_item(Action::UnlockPins));
            }
            menu.addItem(&self.menu_item(Action::CloseAllPins));
        }
        menu.addItem(&NSMenuItem::separatorItem(mtm));

        let permissions = NSMenu::new(mtm);
        permissions.setAutoenablesItems(false);
        for (action, permission) in [
            (
                Action::PermissionScreenRecording,
                Permission::ScreenRecording,
            ),
            (Action::PermissionAccessibility, Permission::Accessibility),
            (Action::PermissionMicrophone, Permission::Microphone),
            (Action::PermissionCamera, Permission::Camera),
        ] {
            let item = self.menu_item(action);
            let granted = permission.status() == Status::Granted;
            item.setState(state(granted));
            item.setToolTip(Some(&NSString::from_str(if granted {
                "Granted"
            } else {
                "Not granted: click to request or open System Settings"
            })));
            permissions.addItem(&item);
        }
        let permissions_item = titled_item(mtm, "Permissions");
        permissions_item.setSubmenu(Some(&permissions));
        menu.addItem(&permissions_item);

        let login = self.menu_item(Action::ToggleLaunchAtLogin);
        login.setState(state(login_item::is_enabled()));
        menu.addItem(&login);
        menu.addItem(&self.menu_item(Action::OpenSettingsFile));
        menu.addItem(&self.menu_item(Action::ReloadSettings));
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&self.menu_item(Action::About));
        let quit = self.menu_item(Action::Quit);
        quit.setKeyEquivalent(ns_string!("q"));
        quit.setKeyEquivalentModifierMask(NSEventModifierFlags::Command);
        menu.addItem(&quit);
    }

    fn menu_item(&self, action: Action) -> Retained<NSMenuItem> {
        let item = titled_item(self.mtm(), action.title());
        // SAFETY: `performAction:` is defined on AppDelegate with the
        // expected `(id sender)` signature, and the delegate (the target)
        // lives for the whole process.
        unsafe {
            item.setAction(Some(sel!(performAction:)));
            item.setTarget(Some(self));
        }
        item.setTag(action.tag());

        let config = self.ivars().config.borrow();
        if let Some(shortcut) = config
            .hotkeys
            .for_action(action)
            .and_then(|s| Shortcut::parse(s).ok())
        {
            item.setKeyEquivalent(&NSString::from_str(&shortcut.key.equivalent.to_string()));
            item.setKeyEquivalentModifierMask(modifier_flags(&shortcut));
        }
        item
    }
}

fn show_capture_error(mtm: MainThreadMarker, error: capture::sck::CaptureError) {
    tracing::error!(error = %error, "capture failed");
    alert::show(mtm, "Capture failed", &error.to_string(), &["OK"]);
}

fn titled_item(mtm: MainThreadMarker, title: &str) -> Retained<NSMenuItem> {
    // SAFETY: a nil action is valid.
    unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            None,
            ns_string!(""),
        )
    }
}

fn state(on: bool) -> isize {
    if on {
        NSControlStateValueOn
    } else {
        NSControlStateValueOff
    }
}

fn modifier_flags(shortcut: &Shortcut) -> NSEventModifierFlags {
    let m = shortcut.modifiers;
    let mut flags = NSEventModifierFlags::empty();
    for (on, flag) in [
        (m.command, NSEventModifierFlags::Command),
        (m.shift, NSEventModifierFlags::Shift),
        (m.option, NSEventModifierFlags::Option),
        (m.control, NSEventModifierFlags::Control),
    ] {
        if on {
            flags |= flag;
        }
    }
    flags
}
