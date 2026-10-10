//! The menu bar application: `NSApplication` setup, the status item menu,
//! global hotkeys and first-launch onboarding.

pub(crate) mod alert;
pub mod server;

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
use crate::command::{Desktop, Region, Request};
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

    app.setMainMenu(Some(&main_menu(mtm)));
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
            crate::history::prune();
            self.install_status_item();
            self.install_hotkeys();
            self.run_onboarding_if_needed();
            let delegate = self.retain();
            server::start(self.mtm(), move |url| delegate.handle_url(url));
            tracing::info!("CleanShotZ started");
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn will_terminate(&self, _notification: &NSNotification) {
            server::stop();
        }

        #[unsafe(method(applicationShouldTerminate:))]
        fn should_terminate(&self, _app: &NSApplication) -> objc2_app_kit::NSApplicationTerminateReply {
            if crate::editor::skip_confirmations() || !crate::editor::has_unsaved_changes() {
                return objc2_app_kit::NSApplicationTerminateReply::TerminateNow;
            }
            let choice = alert::show(
                self.mtm(),
                "Quit and discard annotations?",
                "An editor window has changes that were not saved or copied.",
                &["Cancel", "Quit"],
            );
            if choice == 1 {
                objc2_app_kit::NSApplicationTerminateReply::TerminateNow
            } else {
                objc2_app_kit::NSApplicationTerminateReply::TerminateCancel
            }
        }

        /// `cleanshotz://<command>` URLs, for example
        /// `open cleanshotz://capture-area`.
        #[unsafe(method(application:openURLs:))]
        fn open_urls(&self, _application: &NSApplication, urls: &NSArray<NSURL>) {
            for url in urls.iter() {
                let command = url.host().map(|h| h.to_string()).unwrap_or_default();
                // Debug builds: cleanshotz://dev-show-key?code=8&flags=1048576
                // shows a key in the keystroke display, for testing it
                // without Input Monitoring permission.
                #[cfg(debug_assertions)]
                if command == "dev-show-key" {
                    let code = query_value(&url, "code").and_then(|v| v.parse().ok()).unwrap_or(0);
                    let flags = query_value(&url, "flags").and_then(|v| v.parse().ok()).unwrap_or(0);
                    crate::record::overlays::simulate_key(code, flags);
                    continue;
                }
                let text = url.absoluteString().map(|s| s.to_string()).unwrap_or_default();
                if let Err(e) = self.handle_url(&text) {
                    alert::show(self.mtm(), "CleanShotZ could not run the URL", &format!("{text}\n\n{e}"), &["OK"]);
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
                "{}\n\nChoose other shortcuts in Settings > Shortcuts.",
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
                crate::settings::refresh(self.mtm());
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
        let mtm = self.mtm();
        match action {
            Action::CaptureArea
            | Action::CapturePreviousArea
            | Action::CaptureFullscreen
            | Action::CaptureWindow
            | Action::CaptureText
            | Action::CaptureScrolling
            | Action::RecordScreen
            | Action::SelfTimer => {
                let request = match action {
                    Action::CaptureArea => Request::CaptureArea {
                        region: None,
                        after: None,
                    },
                    Action::CapturePreviousArea => Request::CapturePreviousArea { after: None },
                    Action::CaptureFullscreen => Request::CaptureFullscreen { after: None },
                    Action::CaptureWindow => Request::CaptureWindow { after: None },
                    Action::SelfTimer => Request::SelfTimer { after: None },
                    Action::CaptureText => Request::CaptureText {
                        file: None,
                        region: None,
                        line_breaks: None,
                    },
                    Action::CaptureScrolling => Request::ScrollingCapture {
                        region: None,
                        autoscroll: false,
                    },
                    _ => Request::RecordScreen { region: None },
                };
                // Errors were already shown.
                let _ = self.handle(request);
            }
            Action::OpenSettingsFile => {
                // `open -t` uses the default text editor regardless of how
                // .toml files are associated.
                let path = &self.ivars().config_path;
                if let Err(e) = Command::new("/usr/bin/open").arg("-t").arg(path).spawn() {
                    tracing::error!(error = %e, "could not open settings file");
                }
            }
            Action::OpenSettings => self.open_settings(crate::settings::Tab::General),
            Action::ReloadSettings => self.reload_settings(),
            Action::OpenHistory => crate::history::open_window(mtm),
            Action::OpenInEditor => crate::editor::open_with_panel(mtm),
            Action::AnnotateClipboard => crate::editor::open_clipboard(mtm),
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
            Action::PermissionInputMonitoring => Permission::InputMonitoring.request(),
            Action::About => {
                let app = NSApplication::sharedApplication(mtm);
                app.activate();
                app.orderFrontStandardAboutPanel(None);
            }
            Action::Quit => NSApplication::sharedApplication(mtm).terminate(None),
        }
    }

    fn configure_quick_access(&self) {
        let config = self.ivars().config.borrow();
        crate::quick_access::configure(config.quick_access.clone(), config.capture.clone());
        crate::ocr::configure(config.text.clone());
        crate::video_editor::configure(config.recording.clone());
        crate::history::configure(config.history);
        crate::upload::configure(config.upload.clone());
    }

    /// Runs a `cleanshotz://` URL from Launch Services or the command line.
    fn handle_url(&self, url: &str) -> Result<(), String> {
        let request = crate::command::parse_url(url)?;
        tracing::info!(?request, "command");
        self.handle(request)
    }

    /// Runs a request. Errors in the request itself are returned; errors
    /// in an interactive capture that it starts are shown later.
    fn handle(&self, request: Request) -> Result<(), String> {
        let mtm = self.mtm();
        let mut settings = self.ivars().config.borrow().capture.clone();
        let state_path = crate::state::state_path();
        let error = |e: capture::sck::CaptureError| {
            let message = e.to_string();
            show_capture_error(mtm, e);
            message
        };
        let interactive = |kind, settings| {
            flow::start(mtm, kind, settings, state_path.clone(), show_capture_error).map_err(error)
        };
        match request {
            Request::CaptureArea { region, after } => {
                self.require_screen_recording()?;
                settings.after = after;
                match region {
                    Some(region) => {
                        let (display_id, rect) = resolve_region(mtm, region)?;
                        capture::capture_rect(mtm, display_id, rect, &settings).map_err(error)
                    }
                    None => interactive(flow::Kind::Area, settings),
                }
            }
            Request::CapturePreviousArea { after } => {
                self.require_screen_recording()?;
                settings.after = after;
                flow::capture_previous_area(mtm, settings, state_path.clone(), show_capture_error)
                    .map_err(error)
            }
            Request::CaptureFullscreen { after } => {
                self.require_screen_recording()?;
                settings.after = after;
                capture::capture_fullscreen(mtm, &settings).map_err(error)
            }
            Request::CaptureWindow { after } => {
                self.require_screen_recording()?;
                settings.after = after;
                interactive(flow::Kind::Window, settings)
            }
            Request::SelfTimer { after } => {
                self.require_screen_recording()?;
                settings.after = after;
                interactive(flow::Kind::SelfTimer, settings)
            }
            Request::CaptureText {
                file: Some(file),
                line_breaks,
                ..
            } => {
                let image = capture::image::load(&file)
                    .ok_or_else(|| format!("{} is not an image", file.display()))?;
                crate::ocr::copy_text(image, line_breaks);
                Ok(())
            }
            Request::CaptureText {
                file: None,
                region,
                line_breaks,
            } => {
                self.require_screen_recording()?;
                settings.line_breaks = line_breaks;
                match region {
                    Some(region) => {
                        let (display_id, rect) = resolve_region(mtm, region)?;
                        // Full resolution, as for a selected area.
                        settings.downscale_retina = false;
                        settings.show_cursor = false;
                        let captured =
                            capture::grab_rect(display_id, rect, &settings).map_err(error)?;
                        crate::ocr::copy_text(captured.image, line_breaks);
                        Ok(())
                    }
                    None => interactive(flow::Kind::Text, settings),
                }
            }
            Request::ScrollingCapture { region, autoscroll } => {
                self.require_screen_recording()?;
                let area = region.map(|r| resolve_region(mtm, r)).transpose()?;
                crate::scroll::start(mtm, settings, area, autoscroll).map_err(error)
            }
            Request::RecordScreen { region } => {
                self.require_screen_recording()?;
                let area = region.map(|r| resolve_region(mtm, r)).transpose()?;
                let recording = self.ivars().config.borrow().recording.clone();
                crate::record::toggle(mtm, settings, recording, area).map_err(error)
            }
            Request::Pin { file } => {
                let Some(file) = file.or_else(|| choose_image(mtm)) else {
                    return Ok(());
                };
                let image = capture::image::load(&file)
                    .ok_or_else(|| format!("{} is not an image", file.display()))?;
                let scale = capture::image::scale_for_path(&file);
                let item = crate::quick_access::Item::new(image, scale, None, file, true);
                crate::pin::pin(mtm, &item);
                Ok(())
            }
            Request::OpenAnnotate { file: Some(file) } => {
                crate::editor::open_file(mtm, &file);
                Ok(())
            }
            Request::OpenAnnotate { file: None } => {
                crate::editor::open_with_panel(mtm);
                Ok(())
            }
            Request::OpenFromClipboard => {
                crate::editor::open_clipboard(mtm);
                Ok(())
            }
            Request::AddQuickAccessOverlay { file } => add_to_quick_access(mtm, file),
            Request::DesktopIcons(change) => {
                match change {
                    Desktop::Toggle => crate::desktop_icons::toggle(mtm),
                    Desktop::Hide => crate::desktop_icons::set_hidden(mtm, true),
                    Desktop::Show => crate::desktop_icons::set_hidden(mtm, false),
                }
                Ok(())
            }
            Request::OpenHistory => {
                crate::history::open_window(mtm);
                Ok(())
            }
            Request::RestoreRecentlyClosed => {
                crate::quick_access::restore_recently_closed(mtm);
                Ok(())
            }
            Request::OpenSettings { tab } => {
                match tab.as_deref() {
                    Some("about") => self.perform(Action::About),
                    tab => {
                        self.open_settings(crate::settings::Tab::for_url(tab.unwrap_or("general")))
                    }
                }
                Ok(())
            }
            Request::Quit => {
                // After replying, so the command line hears back.
                dispatch2::DispatchQueue::main().exec_async(move || {
                    let mtm = MainThreadMarker::new().expect("runs on the main thread");
                    NSApplication::sharedApplication(mtm).terminate(None);
                });
                Ok(())
            }
        }
    }

    fn open_settings(&self, tab: crate::settings::Tab) {
        crate::settings::open(self.mtm(), tab, || {
            let (a, b, c) = (self.retain(), self.retain(), self.retain());
            crate::settings::Host {
                config: Box::new(move || a.ivars().config.borrow().clone()),
                update: Box::new(move |change| {
                    b.update_config(|config| change(config));
                    b.configure_quick_access();
                    b.register_hotkeys()
                }),
                suspend_hotkeys: Box::new(move |suspend| {
                    if suspend {
                        if let Some(hotkeys) = c.ivars().hotkeys.get() {
                            hotkeys.borrow_mut().unregister_all();
                        }
                        Vec::new()
                    } else {
                        c.register_hotkeys()
                    }
                }),
            }
        });
    }

    fn require_screen_recording(&self) -> Result<(), String> {
        if Permission::ScreenRecording.status() == Status::Granted {
            return Ok(());
        }
        let choice = alert::show(
            self.mtm(),
            "Screen Recording permission is needed",
            "Allow CleanShotZ in System Settings > Privacy & Security > Screen & System Audio \
             Recording, then try again. macOS may ask you to reopen the app.",
            &["Open System Settings", "Cancel"],
        );
        if choice == 0 {
            Permission::ScreenRecording.request();
        }
        Err("CleanShotZ does not have Screen Recording permission".into())
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
        let record = self.menu_item(Action::RecordScreen);
        if crate::record::is_recording() {
            record.setTitle(ns_string!("Stop Recording"));
        }
        menu.addItem(&record);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&self.menu_item(Action::OpenHistory));
        menu.addItem(&self.menu_item(Action::OpenInEditor));
        menu.addItem(&self.menu_item(Action::AnnotateClipboard));
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
            (
                Action::PermissionInputMonitoring,
                Permission::InputMonitoring,
            ),
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
        let settings = self.menu_item(Action::OpenSettings);
        settings.setKeyEquivalent(ns_string!(","));
        settings.setKeyEquivalentModifierMask(NSEventModifierFlags::Command);
        menu.addItem(&settings);
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

/// The display id and display-local rect (top-left origin) for a region
/// from a request.
fn resolve_region(
    mtm: MainThreadMarker,
    region: Region,
) -> Result<(u32, capture::geometry::Rect), String> {
    let screen = match region.display {
        Some(n) => objc2_app_kit::NSScreen::screens(mtm)
            .iter()
            .nth(n - 1)
            .ok_or_else(|| format!("there is no display {n}"))?,
        None => capture::screen_under_mouse(mtm).ok_or("no display is available")?,
    };
    let id = capture::screen_display_id(&screen).ok_or("no display is available")?;
    let size = screen.frame().size;
    let rect = region
        .on_display((size.width, size.height))
        .ok_or("the area is not on the display")?;
    Ok((id, rect))
}

/// Asks for an image file.
fn choose_image(mtm: MainThreadMarker) -> Option<PathBuf> {
    let panel = objc2_app_kit::NSOpenPanel::openPanel(mtm);
    panel.setCanChooseDirectories(false);
    NSApplication::sharedApplication(mtm).activate();
    if panel.runModal() != objc2_app_kit::NSModalResponseOK {
        return None;
    }
    panel.URL()?.path().map(|p| PathBuf::from(p.to_string()))
}

/// Shows an image or video file in Quick Access.
fn add_to_quick_access(mtm: MainThreadMarker, file: PathBuf) -> Result<(), String> {
    if crate::video_editor::is_video(&file) {
        let thumbnail = crate::record::media::thumbnail(&file)
            .ok_or_else(|| format!("{} is not a video CleanShotZ can read", file.display()))?;
        let duration = crate::record::media::duration(&file);
        let scale =
            objc2_app_kit::NSScreen::mainScreen(mtm).map_or(2.0, |s| s.backingScaleFactor());
        crate::quick_access::add_video(mtm, file, thumbnail, scale, duration, true);
    } else {
        let image = capture::image::load(&file)
            .ok_or_else(|| format!("{} is not an image", file.display()))?;
        let scale = capture::image::scale_for_path(&file);
        crate::quick_access::add(
            mtm,
            capture::output::Delivered {
                image,
                scale,
                source: None,
                file,
                saved: true,
                png: None,
            },
        );
    }
    Ok(())
}

/// The value of query parameter `name` in `url`, percent-decoded.
fn query_value(url: &NSURL, name: &str) -> Option<String> {
    let components =
        objc2_foundation::NSURLComponents::componentsWithURL_resolvingAgainstBaseURL(url, false)?;
    components
        .queryItems()?
        .iter()
        .find(|item| item.name().to_string() == name)
        .and_then(|item| item.value())
        .map(|v| v.to_string())
}

/// The app has no visible menu bar, but the main menu still routes key
/// equivalents: the Edit menu gives text fields and the editor Cut, Copy,
/// Paste, Select All, Undo and Redo.
fn main_menu(mtm: MainThreadMarker) -> Retained<NSMenu> {
    let item = |title: &str, action: objc2::runtime::Sel, key: &str, shift: bool| {
        // SAFETY: standard responder-chain selectors with a nil target.
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(title),
                Some(action),
                &NSString::from_str(key),
            )
        };
        if shift {
            item.setKeyEquivalentModifierMask(
                NSEventModifierFlags::Command | NSEventModifierFlags::Shift,
            );
        }
        item
    };
    let edit = NSMenu::new(mtm);
    edit.setTitle(ns_string!("Edit"));
    edit.addItem(&item("Undo", sel!(undo:), "z", false));
    edit.addItem(&item("Redo", sel!(redo:), "z", true));
    edit.addItem(&NSMenuItem::separatorItem(mtm));
    edit.addItem(&item("Cut", sel!(cut:), "x", false));
    edit.addItem(&item("Copy", sel!(copy:), "c", false));
    edit.addItem(&item("Paste", sel!(paste:), "v", false));
    edit.addItem(&item("Select All", sel!(selectAll:), "a", false));

    let app_menu = NSMenu::new(mtm);
    app_menu.addItem(&item("Quit CleanShotZ", sel!(terminate:), "q", false));

    let main = NSMenu::new(mtm);
    for (title, submenu) in [("CleanShotZ", app_menu), ("Edit", edit)] {
        let top = titled_item(mtm, title);
        top.setSubmenu(Some(&submenu));
        main.addItem(&top);
    }
    main
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
