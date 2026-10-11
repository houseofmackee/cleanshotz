//! The Studio editor: a live preview of the recording as it will be
//! exported, and an inspector for the background, cursor and output.
//! Every change is saved in the package's `project.json` at once.

use std::cell::{OnceCell, RefCell};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSAutoresizingMaskOptions, NSBackingStoreType, NSBezelStyle, NSButton,
    NSControlStateValueOff, NSControlStateValueOn, NSFont, NSPopUpButton, NSSlider, NSTextField,
    NSView, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_av_foundation::{AVPlayer, AVPlayerItem, AVVideoComposition};
use objc2_av_kit::{AVPlayerView, AVPlayerViewControlsStyle};
use objc2_core_media::CMTime;
use objc2_foundation::{NSNotification, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

use super::compositor::Renderer;
use super::events::Events;
use super::project::{ClickEffect, OutputSize, Project};
use super::{media, package};
use crate::editor::model::{Aspect, Background, Fill};
use crate::editor::presets;

const INSPECTOR: f64 = 300.0;
const ROW: f64 = 30.0;

/// A choice in the background menu.
#[derive(Clone)]
enum BackgroundChoice {
    None,
    Fill(Fill),
    Preset(usize),
    Wallpaper,
}

struct Ui {
    background: Retained<NSPopUpButton>,
    padding: Retained<NSSlider>,
    corners: Retained<NSSlider>,
    shadow: Retained<NSButton>,
    aspect: Retained<NSPopUpButton>,
    cursor: Retained<NSButton>,
    cursor_size: Retained<NSSlider>,
    smoothing: Retained<NSSlider>,
    hide_idle: Retained<NSButton>,
    clicks: Retained<NSPopUpButton>,
    output: Retained<NSPopUpButton>,
    export: Retained<NSButton>,
    status: Retained<NSTextField>,
}

pub struct Ivars {
    id: u64,
    package: PathBuf,
    events: Events,
    video: (f64, f64),
    project: RefCell<Project>,
    renderer: RefCell<Arc<Renderer>>,
    choices: RefCell<Vec<BackgroundChoice>>,
    window: OnceCell<Retained<NSWindow>>,
    player: OnceCell<Retained<AVPlayer>>,
    item: OnceCell<Retained<AVPlayerItem>>,
    ui: OnceCell<Ui>,
}

impl Ivars {
    pub fn id_value(&self) -> u64 {
        self.id
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    pub struct StudioEditor;

    unsafe impl NSObjectProtocol for StudioEditor {}

    unsafe impl NSWindowDelegate for StudioEditor {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            if let Some(player) = self.ivars().player.get() {
                // SAFETY: pausing a player.
                unsafe { player.pause() };
            }
            super::registry::remove(self.ivars().id);
        }
    }

    impl StudioEditor {
        #[unsafe(method(changed:))]
        fn changed(&self, sender: &AnyObject) {
            self.apply_controls(sender);
        }

        #[unsafe(method(export:))]
        fn export_action(&self, _sender: &AnyObject) {
            self.export();
        }
    }
);

impl StudioEditor {
    pub fn open(mtm: MainThreadMarker, id: u64, package: &Path) -> Result<Retained<Self>, String> {
        let events = package::load_events(package)?;
        let project = package::load_project(package);
        let (w, h, _) = media::screen_info(package);
        if w <= 0.0 || h <= 0.0 {
            return Err(format!("{} has no screen video", package.display()));
        }
        let renderer = Arc::new(Renderer::new(package, &events, &project, (w, h))?);
        let this = Self::alloc(mtm).set_ivars(Ivars {
            id,
            package: package.to_path_buf(),
            events,
            video: (w, h),
            project: RefCell::new(project),
            renderer: RefCell::new(renderer),
            choices: RefCell::new(Vec::new()),
            window: OnceCell::new(),
            player: OnceCell::new(),
            item: OnceCell::new(),
            ui: OnceCell::new(),
        });
        // SAFETY: NSObject's designated initializer.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        this.build(mtm)?;
        Ok(this)
    }

    fn build(&self, mtm: MainThreadMarker) -> Result<(), String> {
        let renderer = self.ivars().renderer.borrow().clone();
        let (ow, oh) = renderer.output();
        let visible = objc2_app_kit::NSScreen::mainScreen(mtm).map_or(
            NSRect::new(NSPoint::ZERO, NSSize::new(1440.0, 900.0)),
            |s| s.visibleFrame(),
        );
        let fit = (visible.size.width * 0.8 - INSPECTOR) / ow as f64;
        let fit = fit.min(visible.size.height * 0.8 / oh as f64).min(0.6);
        let (pw, ph) = ((ow as f64 * fit).max(480.0), (oh as f64 * fit).max(400.0));
        let frame = NSRect::new(NSPoint::ZERO, NSSize::new(pw + INSPECTOR, ph.max(560.0)));
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
        // SAFETY: the registry keeps the editor (and so the window) alive.
        unsafe { window.setReleasedWhenClosed(false) };
        let title = self
            .ivars()
            .package
            .file_stem()
            .map_or_else(|| "Studio".into(), |n| n.to_string_lossy().into_owned());
        window.setTitle(&NSString::from_str(&title));
        window.setContentMinSize(NSSize::new(480.0 + INSPECTOR, 560.0));
        window.setDelegate(Some(ProtocolObject::from_ref(self)));
        let content = window.contentView().expect("windows have a content view");
        let height = frame.size.height;

        // SAFETY: AVFoundation objects for the preview, on the main thread.
        let (player, item, view) = unsafe {
            let prepared = media::prepare(&self.ivars().package, renderer)?;
            let item = AVPlayerItem::playerItemWithAsset(&prepared.composition, mtm);
            let video: &AVVideoComposition = &prepared.video;
            item.setVideoComposition(Some(video));
            let player = AVPlayer::playerWithPlayerItem(Some(&item), mtm);
            let view = AVPlayerView::initWithFrame(
                AVPlayerView::alloc(mtm),
                NSRect::new(
                    NSPoint::ZERO,
                    NSSize::new(frame.size.width - INSPECTOR, height),
                ),
            );
            view.setControlsStyle(AVPlayerViewControlsStyle::Inline);
            view.setPlayer(Some(&player));
            (player, item, view)
        };
        view.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        content.addSubview(&view);

        let inspector = NSView::initWithFrame(
            NSView::alloc(mtm),
            NSRect::new(
                NSPoint::new(frame.size.width - INSPECTOR, 0.0),
                NSSize::new(INSPECTOR, height),
            ),
        );
        inspector.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewMinXMargin
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        content.addSubview(&inspector);
        let ui = self.build_inspector(mtm, &inspector, height);
        let _ = self.ivars().ui.set(ui);
        let _ = self.ivars().player.set(player);
        let _ = self.ivars().item.set(item);
        let _ = self.ivars().window.set(window.clone());
        self.sync_controls();

        window.center();
        NSApplication::sharedApplication(mtm).activate();
        window.makeKeyAndOrderFront(None);
        Ok(())
    }

    fn build_inspector(&self, mtm: MainThreadMarker, inspector: &NSView, height: f64) -> Ui {
        let target: &AnyObject = self.as_ref();
        let top = NSAutoresizingMaskOptions::ViewMinYMargin;
        let mut y = height - 44.0;
        let x = 16.0;
        let width = INSPECTOR - 32.0;
        let place = |view: &NSView, frame: NSRect| {
            view.setFrame(frame);
            view.setAutoresizingMask(top);
            inspector.addSubview(view);
        };
        let heading = |text: &str, y: f64| {
            let label =
                NSTextField::labelWithString(&NSString::from_str(&crate::i18n::tr(text)), mtm);
            label.setFont(Some(&NSFont::boldSystemFontOfSize(13.0)));
            place(
                &label,
                NSRect::new(NSPoint::new(x, y), NSSize::new(width, 18.0)),
            );
        };
        let label = |text: &str, y: f64| {
            let label =
                NSTextField::labelWithString(&NSString::from_str(&crate::i18n::tr(text)), mtm);
            place(
                &label,
                NSRect::new(NSPoint::new(x, y + 3.0), NSSize::new(92.0, 18.0)),
            );
        };
        let popup = |items: &[String], y: f64| {
            let p = NSPopUpButton::initWithFrame_pullsDown(
                NSPopUpButton::alloc(mtm),
                NSRect::new(NSPoint::ZERO, NSSize::new(10.0, 26.0)),
                false,
            );
            for item in items {
                p.addItemWithTitle(&NSString::from_str(item));
            }
            // SAFETY: the action exists on the editor.
            unsafe {
                p.setTarget(Some(target));
                p.setAction(Some(sel!(changed:)));
            }
            place(
                &p,
                NSRect::new(NSPoint::new(x + 96.0, y), NSSize::new(width - 96.0, 26.0)),
            );
            p
        };
        let slider = |min: f64, max: f64, y: f64| {
            // SAFETY: the action exists on the editor.
            let s = unsafe {
                NSSlider::sliderWithValue_minValue_maxValue_target_action(
                    min,
                    min,
                    max,
                    Some(target),
                    Some(sel!(changed:)),
                    mtm,
                )
            };
            // Rebuilding the preview on every step of a drag is too slow.
            s.setContinuous(false);
            place(
                &s,
                NSRect::new(NSPoint::new(x + 96.0, y), NSSize::new(width - 96.0, 24.0)),
            );
            s
        };
        let check = |text: &str, y: f64| {
            // SAFETY: the action exists on the editor.
            let b = unsafe {
                NSButton::checkboxWithTitle_target_action(
                    &NSString::from_str(&crate::i18n::tr(text)),
                    Some(target),
                    Some(sel!(changed:)),
                    mtm,
                )
            };
            place(
                &b,
                NSRect::new(
                    NSPoint::new(x + 96.0, y + 2.0),
                    NSSize::new(width - 96.0, 20.0),
                ),
            );
            b
        };

        heading("Background", y);
        y -= ROW + 2.0;
        label("Fill:", y);
        let background = popup(&self.background_titles(), y);
        y -= ROW;
        label("Padding:", y);
        let padding = slider(0.0, 200.0, y);
        y -= ROW;
        label("Corners:", y);
        let corners = slider(0.0, 40.0, y);
        y -= ROW;
        let shadow = check("Shadow", y);
        y -= ROW;
        label("Size:", y);
        let aspect = popup(
            &Aspect::ALL
                .iter()
                .map(|a| crate::i18n::tr(a.title()))
                .collect::<Vec<_>>(),
            y,
        );
        y -= ROW + 14.0;

        heading("Cursor", y);
        y -= ROW + 2.0;
        let cursor = check("Show the cursor", y);
        y -= ROW;
        label("Size:", y);
        let cursor_size = slider(0.5, 3.0, y);
        y -= ROW;
        label("Smoothing:", y);
        let smoothing = slider(0.0, 1.0, y);
        y -= ROW;
        let hide_idle = check("Hide when idle", y);
        y -= ROW;
        label("Clicks:", y);
        let clicks = popup(&crate::i18n::labels(&["None", "Ring", "Ripple"]), y);
        y -= ROW + 14.0;

        heading("Export", y);
        y -= ROW + 2.0;
        label("Output:", y);
        let output = popup(
            &OutputSize::ALL
                .iter()
                .map(|o| crate::i18n::tr(o.title()))
                .collect::<Vec<_>>(),
            y,
        );
        y -= ROW + 6.0;
        // SAFETY: the action exists on the editor.
        let export = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str(&t!("Export MP4")),
                Some(target),
                Some(sel!(export:)),
                mtm,
            )
        };
        export.setBezelStyle(NSBezelStyle::Push);
        export.setKeyEquivalent(&NSString::from_str("\r"));
        place(
            &export,
            NSRect::new(NSPoint::new(x + 96.0, y), NSSize::new(width - 96.0, 30.0)),
        );
        y -= ROW;
        let status = NSTextField::labelWithString(&NSString::from_str(""), mtm);
        status.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        place(
            &status,
            NSRect::new(NSPoint::new(x, y), NSSize::new(width, 18.0)),
        );

        Ui {
            background,
            padding,
            corners,
            shadow,
            aspect,
            cursor,
            cursor_size,
            smoothing,
            hide_idle,
            clicks,
            output,
            export,
            status,
        }
    }

    /// The background menu's titles, remembering what each item applies.
    fn background_titles(&self) -> Vec<String> {
        let mut titles = vec![t!("No Background")];
        let mut choices = vec![BackgroundChoice::None];
        for (name, fill) in presets::builtin() {
            titles.push(crate::i18n::tr(name));
            choices.push(BackgroundChoice::Fill(fill));
        }
        for (i, preset) in presets::saved().iter().enumerate() {
            titles.push(preset.name.clone());
            choices.push(BackgroundChoice::Preset(i));
        }
        titles.push(t!("Desktop Wallpaper"));
        choices.push(BackgroundChoice::Wallpaper);
        *self.ivars().choices.borrow_mut() = choices;
        titles
    }

    /// Shows the project's values in the controls.
    fn sync_controls(&self) {
        let Some(ui) = self.ivars().ui.get() else {
            return;
        };
        let project = self.ivars().project.borrow();
        let on = |b: bool| {
            if b {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            }
        };
        let bg = project.background.clone();
        let defaults = bg.clone().unwrap_or_default();
        let choice = match &bg {
            None => 0,
            Some(b) => {
                let choices = self.ivars().choices.borrow();
                let presets = presets::saved();
                choices
                    .iter()
                    .position(|c| match c {
                        BackgroundChoice::Fill(fill) => *fill == b.fill,
                        BackgroundChoice::Preset(i) => {
                            presets.get(*i).is_some_and(|p| p.matches(b))
                        }
                        BackgroundChoice::Wallpaper => matches!(&b.fill, Fill::Image { picture }
                            if *picture.origin() == crate::editor::picture::Origin::Wallpaper),
                        BackgroundChoice::None => false,
                    })
                    .unwrap_or(1)
            }
        };
        ui.background.selectItemAtIndex(choice as isize);
        ui.padding.setDoubleValue(defaults.padding);
        ui.corners.setDoubleValue(defaults.corner_radius);
        ui.shadow.setState(on(defaults.shadow));
        ui.aspect.selectItemAtIndex(
            Aspect::ALL
                .iter()
                .position(|a| *a == defaults.aspect)
                .unwrap_or(0) as isize,
        );
        for control in [
            &*ui.padding as &objc2_app_kit::NSControl,
            &ui.corners,
            &ui.shadow,
            &ui.aspect,
        ] {
            control.setEnabled(bg.is_some());
        }
        let cursor = project.cursor;
        ui.cursor.setState(on(cursor.visible));
        ui.cursor_size.setDoubleValue(cursor.size);
        ui.smoothing.setDoubleValue(cursor.smoothing);
        ui.hide_idle.setState(on(cursor.hide_when_idle));
        ui.clicks.selectItemAtIndex(match cursor.clicks {
            ClickEffect::None => 0,
            ClickEffect::Ring => 1,
            ClickEffect::Ripple => 2,
        });
        ui.output.selectItemAtIndex(
            OutputSize::ALL
                .iter()
                .position(|o| *o == project.output)
                .unwrap_or(0) as isize,
        );
    }

    /// Reads the controls into the project, saves it and updates the
    /// preview.
    fn apply_controls(&self, sender: &AnyObject) {
        let Some(ui) = self.ivars().ui.get() else {
            return;
        };
        let mtm = self.mtm();
        let mut project = self.ivars().project.borrow().clone();
        let current = project.background.clone();
        let with_fill = |fill: Fill| Background {
            fill,
            padding: ui.padding.doubleValue().round(),
            corner_radius: ui.corners.doubleValue().round(),
            shadow: ui.shadow.state() == NSControlStateValueOn,
            aspect: Aspect::ALL
                .get(ui.aspect.indexOfSelectedItem().max(0) as usize)
                .copied()
                .unwrap_or(Aspect::Auto),
            balance: false,
        };
        let menu: &AnyObject = ui.background.as_ref();
        project.background = if std::ptr::eq(sender, menu) {
            let index = ui.background.indexOfSelectedItem().max(0) as usize;
            let choice = self.ivars().choices.borrow().get(index).cloned();
            match choice {
                None | Some(BackgroundChoice::None) => None,
                Some(BackgroundChoice::Fill(fill)) => Some(with_fill(fill)),
                Some(BackgroundChoice::Preset(i)) => {
                    match presets::saved().get(i).map(|p| p.background(mtm)) {
                        Some(Ok(background)) => Some(background),
                        Some(Err(e)) => {
                            crate::ui::toast::show(mtm, &e);
                            current
                        }
                        None => current,
                    }
                }
                Some(BackgroundChoice::Wallpaper) => match presets::wallpaper(mtm) {
                    Some(picture) => Some(with_fill(Fill::Image { picture })),
                    None => {
                        crate::ui::toast::show(mtm, "The desktop picture could not be read");
                        current
                    }
                },
            }
        } else {
            current.map(|b| with_fill(b.fill))
        };
        project.cursor.visible = ui.cursor.state() == NSControlStateValueOn;
        project.cursor.size = (ui.cursor_size.doubleValue() * 4.0).round() / 4.0;
        project.cursor.smoothing = (ui.smoothing.doubleValue() * 10.0).round() / 10.0;
        project.cursor.hide_when_idle = ui.hide_idle.state() == NSControlStateValueOn;
        project.cursor.clicks = match ui.clicks.indexOfSelectedItem() {
            0 => ClickEffect::None,
            1 => ClickEffect::Ring,
            _ => ClickEffect::Ripple,
        };
        project.output = OutputSize::ALL
            .get(ui.output.indexOfSelectedItem().max(0) as usize)
            .copied()
            .unwrap_or(OutputSize::P1080);
        if let Err(e) = self.set_project(project) {
            tracing::error!(error = %e, "studio change not applied");
            crate::ui::toast::show(mtm, &e);
        }
        self.sync_controls();
    }

    /// Applies `project`: saves it, rebuilds the renderer and shows the
    /// change in the preview.
    fn set_project(&self, project: Project) -> Result<(), String> {
        let ivars = self.ivars();
        let renderer = Arc::new(Renderer::new(
            &ivars.package,
            &ivars.events,
            &project,
            ivars.video,
        )?);
        package::save_project(&ivars.package, &project)?;
        *ivars.project.borrow_mut() = project;
        *ivars.renderer.borrow_mut() = renderer.clone();
        let (Some(item), Some(player)) = (ivars.item.get(), ivars.player.get()) else {
            return Ok(());
        };
        // SAFETY: AVFoundation objects on the main thread.
        unsafe {
            let prepared = media::prepare(&ivars.package, renderer)?;
            let video: &AVVideoComposition = &prepared.video;
            item.setVideoComposition(Some(video));
            // Redraw the current frame when paused.
            if player.rate() == 0.0 {
                let now = player.currentTime();
                let zero = CMTime::with_seconds(0.0, 600);
                player.seekToTime_toleranceBefore_toleranceAfter(now, zero, zero);
            }
        }
        Ok(())
    }

    /// Renders the recording to an MP4 in the save folder and shows it in
    /// Quick Access.
    fn export(&self) {
        let Some(ui) = self.ivars().ui.get() else {
            return;
        };
        ui.export.setEnabled(false);
        ui.status
            .setStringValue(&NSString::from_str(&t!("Exporting…")));
        let settings = crate::quick_access::capture_settings();
        let dir = if settings.save_to_file {
            crate::capture::naming::expand_home(&settings.save_directory)
        } else {
            crate::capture::output::unsaved_dir()
        };
        let stem = self
            .ivars()
            .package
            .file_stem()
            .map_or_else(|| "Recording".into(), |s| s.to_string_lossy().into_owned());
        let _ = std::fs::create_dir_all(&dir);
        let output = crate::capture::naming::unique_path(&dir, &stem, "mp4");
        let package = self.ivars().package.clone();
        let renderer = super::Shared(self.ivars().renderer.borrow().clone());
        let id = self.ivars().id;
        let saved = settings.save_to_file;
        std::thread::spawn(move || {
            let started = std::time::Instant::now();
            let result = media::export(&package, renderer.0, &output)
                .and_then(|()| {
                    crate::record::media::thumbnail(&output).ok_or("no thumbnail".into())
                })
                .map(super::Shared);
            let seconds = started.elapsed().as_secs_f64();
            dispatch2::DispatchQueue::main().exec_async(move || {
                let mtm = MainThreadMarker::new().expect("runs on the main thread");
                let editor = super::registry::get(id);
                if let Some(ui) = editor.as_ref().and_then(|e| e.ivars().ui.get()) {
                    ui.export.setEnabled(true);
                }
                match result {
                    Ok(thumbnail) => {
                        tracing::info!(path = %output.display(), seconds, "studio recording exported");
                        if let Some(ui) = editor.as_ref().and_then(|e| e.ivars().ui.get()) {
                            ui.status.setStringValue(&NSString::from_str(&t!("Exported")));
                        }
                        let duration = crate::record::media::duration(&output);
                        crate::history::record_video(output.clone(), thumbnail.0.clone(), duration);
                        crate::quick_access::add_video(mtm, output, thumbnail.0, 2.0, duration, saved);
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "studio export failed");
                        if let Some(ui) = editor.as_ref().and_then(|e| e.ivars().ui.get()) {
                            ui.status.setStringValue(&NSString::from_str(&t!("Export failed")));
                        }
                        crate::app::alert::show(mtm, "The recording could not be exported", &e, &["OK"]);
                    }
                }
            });
        });
    }
}
