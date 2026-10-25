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
    NSAutoresizingMaskOptions, NSBackingStoreType, NSBezelStyle, NSButton, NSControlStateValueOff,
    NSControlStateValueOn, NSFont, NSPopUpButton, NSSlider, NSTextField, NSView, NSWindow,
    NSWindowDelegate, NSWindowStyleMask,
};
use objc2_av_foundation::{AVPlayer, AVPlayerItem, AVVideoComposition};
use objc2_av_kit::{AVPlayerView, AVPlayerViewControlsStyle};
use objc2_core_media::CMTime;
use objc2_foundation::{NSNotification, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

use super::compositor::Renderer;
use super::events::Events;
use super::project::{ClickEffect, OutputSize, Project};
use super::track::ZoomTrack;
use super::zoom::{Focus, Zoom};
use super::{media, package};
use crate::editor::model::{Aspect, Background, Fill};
use crate::editor::presets;

const INSPECTOR: f64 = 300.0;
const ROW: f64 = 30.0;
/// The zoom track's height.
const TRACK: f64 = 52.0;
const MIN_HEIGHT: f64 = 600.0;
/// Room to lay out the inspector in; it is then cut to what its sections
/// use (it scrolls in shorter windows).
const INSPECTOR_HEIGHT: f64 = 2000.0;

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
    camera: Option<CameraControls>,
    keys: Option<KeyControls>,
    watermark: WatermarkControls,
    captions: CaptionControls,
    zoom_level: Retained<NSSlider>,
    zoom_follow: Retained<NSButton>,
    zoom_delete: Retained<NSButton>,
    motion_blur: Retained<NSButton>,
    output: Retained<NSPopUpButton>,
    export: Retained<NSButton>,
    status: Retained<NSTextField>,
}

struct CameraControls {
    show: Retained<NSButton>,
    corner: Retained<NSPopUpButton>,
    size: Retained<NSSlider>,
    shape: Retained<NSPopUpButton>,
}

struct WatermarkControls {
    show: Retained<NSButton>,
    position: Retained<NSPopUpButton>,
    size: Retained<NSSlider>,
}

struct CaptionControls {
    language: Retained<NSPopUpButton>,
    make: Retained<NSButton>,
    status: Retained<NSTextField>,
    show: Retained<NSButton>,
    place: Retained<NSPopUpButton>,
    size: Retained<NSSlider>,
    edit: Retained<NSButton>,
    export: Retained<NSButton>,
}

struct KeyControls {
    show: Retained<NSButton>,
    place: Retained<NSPopUpButton>,
    size: Retained<NSSlider>,
}

pub struct Ivars {
    id: u64,
    package: PathBuf,
    events: Events,
    video: (f64, f64),
    duration: f64,
    project: RefCell<Project>,
    renderer: RefCell<Arc<Renderer>>,
    choices: RefCell<Vec<BackgroundChoice>>,
    window: OnceCell<Retained<NSWindow>>,
    player: OnceCell<Retained<AVPlayer>>,
    item: OnceCell<Retained<AVPlayerItem>>,
    ui: OnceCell<Ui>,
    track: OnceCell<Retained<ZoomTrack>>,
    /// The playhead observer, removed when the window closes.
    observer: OnceCell<Retained<AnyObject>>,
    /// The zoom selected in the track.
    selected: std::cell::Cell<Option<usize>>,
    /// The caption languages in the language menu, in order.
    languages: RefCell<Vec<String>>,
    /// Captions are being made.
    transcribing: std::cell::Cell<bool>,
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
                // SAFETY: pausing a player and removing its observer.
                unsafe {
                    player.pause();
                    if let Some(observer) = self.ivars().observer.get() {
                        player.removeTimeObserver(observer);
                    }
                }
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

        #[unsafe(method(zoomChanged:))]
        fn zoom_changed(&self, _sender: &AnyObject) {
            self.apply_zoom_controls();
        }

        #[unsafe(method(deleteZoom:))]
        fn delete_zoom_action(&self, _sender: &AnyObject) {
            self.delete_zoom();
        }

        #[unsafe(method(regenerateZooms:))]
        fn regenerate_zooms(&self, _sender: &AnyObject) {
            self.regenerate();
        }

        #[unsafe(method(makeCaptions:))]
        fn make_captions_action(&self, _sender: &AnyObject) {
            self.make_captions();
        }

        #[unsafe(method(editCaptions:))]
        fn edit_captions_action(&self, _sender: &AnyObject) {
            super::captions::open(self.mtm(), self.ivars().id);
        }

        #[unsafe(method(exportSubtitles:))]
        fn export_subtitles_action(&self, _sender: &AnyObject) {
            self.export_subtitles();
        }
    }
);

impl StudioEditor {
    pub fn open(mtm: MainThreadMarker, id: u64, package: &Path) -> Result<Retained<Self>, String> {
        let super::Loaded {
            events,
            project,
            video: (w, h),
            duration,
        } = super::load(package)?;
        let renderer = Arc::new(Renderer::new(package, &events, &project, (w, h), duration)?);
        let this = Self::alloc(mtm).set_ivars(Ivars {
            id,
            package: package.to_path_buf(),
            events,
            video: (w, h),
            duration,
            project: RefCell::new(project),
            renderer: RefCell::new(renderer),
            choices: RefCell::new(Vec::new()),
            window: OnceCell::new(),
            player: OnceCell::new(),
            item: OnceCell::new(),
            ui: OnceCell::new(),
            track: OnceCell::new(),
            observer: OnceCell::new(),
            selected: std::cell::Cell::new(None),
            languages: RefCell::new(Vec::new()),
            transcribing: std::cell::Cell::new(false),
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
        let frame = NSRect::new(
            NSPoint::ZERO,
            NSSize::new(pw + INSPECTOR, (ph + TRACK).max(MIN_HEIGHT)),
        );
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
        window.setContentMinSize(NSSize::new(480.0 + INSPECTOR, MIN_HEIGHT));
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
                    NSPoint::new(0.0, TRACK),
                    NSSize::new(frame.size.width - INSPECTOR, height - TRACK),
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
        let track = ZoomTrack::new(
            mtm,
            self.ivars().id,
            NSRect::new(
                NSPoint::ZERO,
                NSSize::new(frame.size.width - INSPECTOR, TRACK),
            ),
        );
        track.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
        content.addSubview(&track);
        let _ = self.ivars().track.set(track);
        // Move the playhead along with playback.
        let id = self.ivars().id;
        let block = block2::RcBlock::new(move |_time: CMTime| {
            if let Some(track) =
                super::registry::get(id).and_then(|e| e.ivars().track.get().cloned())
            {
                track.setNeedsDisplay(true);
            }
        });
        // SAFETY: a main-queue observer with a block of the right signature;
        // removed when the window closes.
        let observer = unsafe {
            player.addPeriodicTimeObserverForInterval_queue_usingBlock(
                CMTime::with_seconds(1.0 / 30.0, 600),
                None,
                &block,
            )
        };
        let _ = self.ivars().observer.set(observer);

        // The inspector scrolls when the window is short.
        let scroll = objc2_app_kit::NSScrollView::initWithFrame(
            objc2_app_kit::NSScrollView::alloc(mtm),
            NSRect::new(
                NSPoint::new(frame.size.width - INSPECTOR, 0.0),
                NSSize::new(INSPECTOR, height),
            ),
        );
        scroll.setHasVerticalScroller(true);
        scroll.setDrawsBackground(false);
        scroll.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewMinXMargin
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        let inspector = NSView::initWithFrame(
            NSView::alloc(mtm),
            NSRect::new(NSPoint::ZERO, NSSize::new(INSPECTOR, INSPECTOR_HEIGHT)),
        );
        scroll.setDocumentView(Some(&inspector));
        content.addSubview(&scroll);
        let (ui, used) = self.build_inspector(mtm, &inspector, INSPECTOR_HEIGHT);
        // The controls keep their distance from the top.
        inspector.setFrameSize(NSSize::new(INSPECTOR, used));
        // Start at the top.
        let clip = scroll.contentView();
        clip.scrollToPoint(NSPoint::new(0.0, (used - height).max(0.0)));
        scroll.reflectScrolledClipView(&clip);
        let _ = self.ivars().ui.set(ui);
        self.load_languages();
        let _ = self.ivars().player.set(player);
        let _ = self.ivars().item.set(item);
        let _ = self.ivars().window.set(window.clone());
        self.sync_controls();

        window.center();
        crate::ui::bring_forward(mtm, &window);
        Ok(())
    }

    /// Lays out the controls from the top of `inspector`; also returns the
    /// height they use.
    fn build_inspector(&self, mtm: MainThreadMarker, inspector: &NSView, height: f64) -> (Ui, f64) {
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

        heading("Zoom", y);
        y -= ROW + 2.0;
        label("Level:", y);
        let zoom_level = slider(super::zoom::MIN_LEVEL, super::zoom::MAX_LEVEL, y);
        // SAFETY: the action exists on the editor.
        unsafe { zoom_level.setAction(Some(sel!(zoomChanged:))) };
        y -= ROW;
        let zoom_follow = check("Follow the cursor", y);
        // SAFETY: the action exists on the editor.
        unsafe { zoom_follow.setAction(Some(sel!(zoomChanged:))) };
        y -= ROW;
        let button = |title: &str, action, y: f64, left: f64, w: f64| {
            // SAFETY: the action exists on the editor.
            let b = unsafe {
                NSButton::buttonWithTitle_target_action(
                    &NSString::from_str(&crate::i18n::tr(title)),
                    Some(target),
                    Some(action),
                    mtm,
                )
            };
            place(&b, NSRect::new(NSPoint::new(left, y), NSSize::new(w, 28.0)));
            b
        };
        let zoom_delete = button("Delete Zoom", sel!(deleteZoom:), y, x + 96.0, width - 96.0);
        y -= ROW + 2.0;
        button(
            "Make Automatic Zooms Again",
            sel!(regenerateZooms:),
            y,
            x,
            width,
        );
        y -= ROW;
        let motion_blur = check("Motion blur", y);
        y -= ROW;
        let hint = NSTextField::wrappingLabelWithString(
            &NSString::from_str(&t!(
                "Drag zooms in the track under the video. Double-click it to add one."
            )),
            mtm,
        );
        hint.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        hint.setTextColor(Some(&objc2_app_kit::NSColor::secondaryLabelColor()));
        place(
            &hint,
            NSRect::new(NSPoint::new(x, y - 10.0), NSSize::new(width, 30.0)),
        );
        y -= ROW + 20.0;

        let camera = self.ivars().events.camera.is_some().then(|| {
            heading("Camera", y);
            y -= ROW + 2.0;
            let show = check("Show the camera", y);
            y -= ROW;
            label("Corner:", y);
            let corner = popup(
                &super::project::Corner::ALL
                    .iter()
                    .map(|c| crate::i18n::tr(c.title()))
                    .collect::<Vec<_>>(),
                y,
            );
            y -= ROW;
            label("Size:", y);
            let size = slider(0.12, 0.45, y);
            y -= ROW;
            label("Shape:", y);
            let shape = popup(&crate::i18n::labels(&["Circle", "Rounded Square"]), y);
            y -= ROW + 14.0;
            CameraControls {
                show,
                corner,
                size,
                shape,
            }
        });
        let keys = (!self.ivars().events.keys.is_empty()).then(|| {
            heading("Keys", y);
            y -= ROW + 2.0;
            let show = check("Show pressed keys", y);
            y -= ROW;
            label("Place:", y);
            let place = popup(&crate::i18n::labels(&["Bottom", "Top"]), y);
            y -= ROW;
            label("Size:", y);
            let size = slider(0.6, 2.0, y);
            y -= ROW + 14.0;
            KeyControls { show, place, size }
        });

        heading("Watermark", y);
        y -= ROW + 2.0;
        let watermark = {
            let show = check("Show the watermark", y);
            y -= ROW;
            label("Position:", y);
            let position = popup(
                &crate::watermark::Position::ALL
                    .iter()
                    .map(|p| crate::i18n::tr(p.title()))
                    .collect::<Vec<_>>(),
                y,
            );
            y -= ROW;
            label("Size:", y);
            let size = slider(5.0, 40.0, y);
            y -= ROW + 14.0;
            WatermarkControls {
                show,
                position,
                size,
            }
        };

        heading("Captions", y);
        y -= ROW + 2.0;
        let captions = {
            label("Language:", y);
            let language = popup(&[t!("Loading…")], y);
            y -= ROW;
            let make = button(
                "Make Captions",
                sel!(makeCaptions:),
                y,
                x + 96.0,
                width - 96.0,
            );
            y -= ROW;
            let status = NSTextField::wrappingLabelWithString(&NSString::from_str(""), mtm);
            status.setFont(Some(&NSFont::systemFontOfSize(11.0)));
            status.setTextColor(Some(&objc2_app_kit::NSColor::secondaryLabelColor()));
            place(
                &status,
                NSRect::new(NSPoint::new(x, y - 8.0), NSSize::new(width, 30.0)),
            );
            y -= ROW + 4.0;
            let show = check("Show captions", y);
            y -= ROW;
            label("Place:", y);
            let place_menu = popup(&crate::i18n::labels(&["Bottom", "Top"]), y);
            y -= ROW;
            label("Size:", y);
            let size = slider(0.6, 2.0, y);
            y -= ROW;
            let edit = button(
                "Edit Captions…",
                sel!(editCaptions:),
                y,
                x + 96.0,
                width - 96.0,
            );
            y -= ROW + 2.0;
            let export = button(
                "Export Subtitles…",
                sel!(exportSubtitles:),
                y,
                x + 96.0,
                width - 96.0,
            );
            y -= ROW + 14.0;
            CaptionControls {
                language,
                make,
                status,
                show,
                place: place_menu,
                size,
                edit,
                export,
            }
        };

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

        let ui = Ui {
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
            camera,
            keys,
            watermark,
            captions,
            zoom_level,
            zoom_follow,
            zoom_delete,
            motion_blur,
            output,
            export,
            status,
        };
        (ui, height - y + 16.0)
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
        ui.motion_blur.setState(on(project.motion_blur));
        if let Some(c) = &ui.camera {
            let style = project.camera;
            c.show.setState(on(style.visible));
            c.corner.selectItemAtIndex(
                super::project::Corner::ALL
                    .iter()
                    .position(|x| *x == style.corner)
                    .unwrap_or(0) as isize,
            );
            c.size.setDoubleValue(style.size);
            c.shape
                .selectItemAtIndex((style.shape == crate::config::CameraShape::Rounded) as isize);
        }
        let captions = project.captions.as_ref();
        let can_caption = crate::captions::available() && !self.ivars().transcribing.get();
        ui.captions.language.setEnabled(can_caption);
        ui.captions.make.setEnabled(can_caption);
        ui.captions
            .make
            .setTitle(&NSString::from_str(&if captions.is_some() {
                t!("Make Captions Again")
            } else {
                t!("Make Captions")
            }));
        for control in [
            &*ui.captions.show as &objc2_app_kit::NSControl,
            &ui.captions.edit,
            &ui.captions.export,
        ] {
            control.setEnabled(captions.is_some());
        }
        let shown = captions.is_some_and(|c| c.visible);
        ui.captions.place.setEnabled(shown);
        ui.captions.size.setEnabled(shown);
        ui.captions.show.setState(on(shown));
        if let Some(c) = captions {
            ui.captions.place.selectItemAtIndex(c.top as isize);
            ui.captions.size.setDoubleValue(c.size);
        }
        let watermark = project.watermark.as_ref();
        ui.watermark.show.setState(on(watermark.is_some()));
        let style = watermark
            .cloned()
            .unwrap_or_else(|| crate::watermark::settings().style);
        ui.watermark.position.selectItemAtIndex(
            crate::watermark::Position::ALL
                .iter()
                .position(|p| *p == style.position)
                .unwrap_or(8) as isize,
        );
        ui.watermark.size.setDoubleValue(style.size);
        ui.watermark.position.setEnabled(watermark.is_some());
        ui.watermark.size.setEnabled(watermark.is_some());
        if let Some(k) = &ui.keys {
            k.show.setState(on(project.keys.visible));
            k.place.selectItemAtIndex(project.keys.top as isize);
            k.size.setDoubleValue(project.keys.size);
        }
        let selected = self
            .ivars()
            .selected
            .get()
            .and_then(|i| project.zooms.get(i));
        for control in [
            &*ui.zoom_level as &objc2_app_kit::NSControl,
            &ui.zoom_follow,
            &ui.zoom_delete,
        ] {
            control.setEnabled(selected.is_some());
        }
        if let Some(zoom) = selected {
            ui.zoom_level.setDoubleValue(zoom.level);
            ui.zoom_follow
                .setState(on(zoom.focus == super::zoom::Focus::Cursor));
        }
        if let Some(track) = self.ivars().track.get() {
            track.setNeedsDisplay(true);
        }
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
        project.motion_blur = ui.motion_blur.state() == NSControlStateValueOn;
        if let Some(c) = &ui.camera {
            project.camera.visible = c.show.state() == NSControlStateValueOn;
            project.camera.corner = super::project::Corner::ALL
                .get(c.corner.indexOfSelectedItem().max(0) as usize)
                .copied()
                .unwrap_or(super::project::Corner::BottomRight);
            project.camera.size = (c.size.doubleValue() * 100.0).round() / 100.0;
            project.camera.shape = if c.shape.indexOfSelectedItem() == 1 {
                crate::config::CameraShape::Rounded
            } else {
                crate::config::CameraShape::Circle
            };
        }
        if let Some(c) = project.captions.as_mut() {
            c.visible = ui.captions.show.state() == NSControlStateValueOn;
            c.top = ui.captions.place.indexOfSelectedItem() == 1;
            c.size = (ui.captions.size.doubleValue() * 10.0).round() / 10.0;
        }
        project.watermark = if ui.watermark.show.state() == NSControlStateValueOn {
            // Turned on: the watermark from the settings.
            let mut style = project
                .watermark
                .take()
                .unwrap_or_else(|| crate::watermark::settings().style);
            if let Some(position) = crate::watermark::Position::ALL
                .get(ui.watermark.position.indexOfSelectedItem().max(0) as usize)
            {
                style.position = *position;
            }
            style.size = ui.watermark.size.doubleValue().round();
            match crate::watermark::mark(&style) {
                Ok(_) => Some(style),
                Err(e) => {
                    crate::ui::toast::show(mtm, &e);
                    None
                }
            }
        } else {
            None
        };
        if let Some(k) = &ui.keys {
            project.keys.visible = k.show.state() == NSControlStateValueOn;
            project.keys.top = k.place.indexOfSelectedItem() == 1;
            project.keys.size = (k.size.doubleValue() * 10.0).round() / 10.0;
        }
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
            ivars.duration,
        )?);
        package::save_project(&ivars.package, &project)?;
        save_watermark(&ivars.package, project.watermark.as_ref());
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

    /// Fills the caption language menu, in the background (asking for the
    /// languages takes a moment).
    fn load_languages(&self) {
        if !crate::captions::available() {
            if let Some(ui) = self.ivars().ui.get() {
                ui.captions.language.removeAllItems();
                ui.captions
                    .language
                    .addItemWithTitle(&NSString::from_str(&t!("Not available")));
                ui.captions
                    .status
                    .setStringValue(&NSString::from_str(&t!("Captions need macOS 26 or later.")));
            }
            return;
        }
        let id = self.ivars().id;
        std::thread::spawn(move || {
            let locales = crate::captions::locales();
            dispatch2::DispatchQueue::main().exec_async(move || {
                if let Some(editor) = super::registry::get(id) {
                    editor.show_languages(&locales);
                }
            });
        });
    }

    fn show_languages(&self, locales: &crate::captions::Locales) {
        let Some(ui) = self.ivars().ui.get() else {
            return;
        };
        let current = objc2_foundation::NSLocale::currentLocale();
        let name = |id: &str| {
            current
                .localizedStringForLocaleIdentifier(&NSString::from_str(id))
                .to_string()
        };
        let mut languages = locales.supported.clone();
        languages.sort_by_key(|id| name(id));
        ui.captions.language.removeAllItems();
        for id in &languages {
            let mut title = name(id);
            if locales.installed.contains(id) {
                title.push_str(" ✓");
            }
            ui.captions
                .language
                .addItemWithTitle(&NSString::from_str(&title));
        }
        // The recording's captions' language, else the Mac's.
        let chosen = self
            .ivars()
            .project
            .borrow()
            .captions
            .as_ref()
            .map(|c| c.language.clone())
            .filter(|l| languages.contains(l))
            .or_else(|| {
                crate::captions::preferred(locales, &current.localeIdentifier().to_string())
            });
        if let Some(index) = chosen.and_then(|c| languages.iter().position(|l| *l == c)) {
            ui.captions.language.selectItemAtIndex(index as isize);
        }
        ui.captions
            .language
            .setToolTip(Some(&NSString::from_str(&t!(
                "✓ means the language is on this Mac; others are downloaded the first time"
            ))));
        *self.ivars().languages.borrow_mut() = languages;
        if !self.ivars().transcribing.get() {
            self.show_caption_count();
        }
    }

    fn set_caption_status(&self, text: &str) {
        if let Some(ui) = self.ivars().ui.get() {
            ui.captions.status.setStringValue(&NSString::from_str(text));
        }
    }

    /// Shows how many captions there are (nothing without captions).
    fn show_caption_count(&self) {
        let count = self.caption_cues().len();
        self.set_caption_status(&match count {
            0 => String::new(),
            1 => t!("1 caption"),
            n => t!("{n} captions", n),
        });
    }

    /// Transcribes the recording in the chosen language, in the background.
    fn make_captions(&self) {
        let mtm = self.mtm();
        let Some(ui) = self.ivars().ui.get() else {
            return;
        };
        let index = ui.captions.language.indexOfSelectedItem().max(0) as usize;
        let Some(locale) = self.ivars().languages.borrow().get(index).cloned() else {
            return;
        };
        if self.ivars().project.borrow().captions.is_some()
            && !crate::editor::skip_confirmations()
            && crate::app::alert::show(
                mtm,
                "Make the captions again?",
                "Any changes you made to the captions are replaced.",
                &["Make Again", "Cancel"],
            ) != 0
        {
            return;
        }
        self.ivars().transcribing.set(true);
        self.set_caption_status(&t!("Making captions…"));
        self.sync_controls();
        let id = self.ivars().id;
        let screen = package::screen(&self.ivars().package);
        std::thread::spawn(move || {
            let progress = move |fraction: f64| {
                let percent = (fraction * 100.0).round();
                dispatch2::DispatchQueue::main().exec_async(move || {
                    if let Some(editor) = super::registry::get(id) {
                        editor.set_caption_status(&t!(
                            "Downloading the language for captions: {percent}%",
                            percent
                        ));
                    }
                });
            };
            let result = crate::captions::transcribe(&screen, &locale, &progress);
            dispatch2::DispatchQueue::main().exec_async(move || {
                if let Some(editor) = super::registry::get(id) {
                    editor.captions_made(locale, result);
                }
            });
        });
    }

    fn captions_made(&self, locale: String, result: Result<Vec<crate::captions::Word>, String>) {
        self.ivars().transcribing.set(false);
        match result {
            Ok(words) => {
                let cues = crate::captions::make_cues(&words);
                if cues.is_empty() {
                    self.set_caption_status(&t!("No speech was found in the recording."));
                } else {
                    let mut project = self.ivars().project.borrow().clone();
                    let style = project.captions.take().unwrap_or_default();
                    project.captions = Some(super::project::Captions {
                        language: locale,
                        cues,
                        visible: true,
                        ..style
                    });
                    match self.set_project(project) {
                        Ok(()) => self.show_caption_count(),
                        Err(e) => self.set_caption_status(&e),
                    }
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "captions not made");
                self.set_caption_status(&e);
            }
        }
        super::captions::refresh(self.ivars().id);
        self.sync_controls();
    }

    /// The window's title: the recording's name.
    pub fn window_title(&self) -> Option<String> {
        self.ivars().window.get().map(|w| w.title().to_string())
    }

    /// Plays the preview from `t` seconds (a caption in the captions
    /// window).
    pub fn play_from(&self, t: f64) {
        self.seek(t);
        if let Some(player) = self.ivars().player.get() {
            // SAFETY: plain player control on the main thread.
            unsafe { player.play() };
        }
    }

    /// The captions' cues, for the captions window.
    pub fn caption_cues(&self) -> Vec<crate::captions::Cue> {
        self.ivars()
            .project
            .borrow()
            .captions
            .as_ref()
            .map(|c| c.cues.clone())
            .unwrap_or_default()
    }

    /// Replaces the captions' cues (edited in the captions window).
    pub fn set_caption_cues(&self, cues: Vec<crate::captions::Cue>) {
        let mut project = self.ivars().project.borrow().clone();
        if let Some(c) = project.captions.as_mut() {
            c.cues = cues;
            match self.set_project(project) {
                Ok(()) => self.show_caption_count(),
                Err(e) => tracing::error!(error = %e, "caption change not applied"),
            }
        }
    }

    /// Saves the captions as SRT, or WebVTT for a `.vtt` name.
    fn export_subtitles(&self) {
        let mtm = self.mtm();
        let cues = self.caption_cues();
        if cues.is_empty() {
            return;
        }
        let stem = self
            .ivars()
            .package
            .file_stem()
            .map_or_else(|| "Captions".into(), |s| s.to_string_lossy().into_owned());
        let panel = objc2_app_kit::NSSavePanel::savePanel(mtm);
        panel.setNameFieldStringValue(&NSString::from_str(&format!("{stem}.srt")));
        panel.setMessage(Some(&NSString::from_str(&t!(
            "Saved as SRT, or as WebVTT when the name ends in .vtt."
        ))));
        objc2_app_kit::NSApplication::sharedApplication(mtm).activate();
        if panel.runModal() != objc2_app_kit::NSModalResponseOK {
            return;
        }
        let Some(path) = panel.URL().and_then(|u| u.path()) else {
            return;
        };
        let path = std::path::PathBuf::from(path.to_string());
        let vtt = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("vtt"));
        let text = if vtt {
            crate::captions::vtt(&cues)
        } else {
            crate::captions::srt(&cues)
        };
        match std::fs::write(&path, text) {
            Ok(()) => crate::ui::toast::show(mtm, &t!("Subtitles saved")),
            Err(e) => {
                crate::app::alert::show(
                    mtm,
                    "Could not save the subtitles",
                    &format!("{}: {e}", path.display()),
                    &["OK"],
                );
            }
        }
    }

    pub fn duration(&self) -> f64 {
        self.ivars().duration
    }

    pub fn zooms(&self) -> Vec<Zoom> {
        self.ivars().project.borrow().zooms.clone()
    }

    pub fn selected_zoom(&self) -> Option<usize> {
        self.ivars().selected.get()
    }

    pub fn select_zoom(&self, index: Option<usize>) {
        self.ivars().selected.set(index);
        self.sync_controls();
    }

    /// The playhead, in seconds.
    pub fn current_time(&self) -> f64 {
        self.ivars()
            .player
            .get()
            // SAFETY: reading the player's time.
            .map_or(0.0, |p| unsafe { p.currentTime().seconds() })
            .max(0.0)
    }

    pub fn seek(&self, t: f64) {
        if let Some(player) = self.ivars().player.get() {
            // SAFETY: seeking a player on the main thread.
            unsafe {
                let zero = CMTime::with_seconds(0.0, 600);
                player.seekToTime_toleranceBefore_toleranceAfter(
                    CMTime::with_seconds(t, 600),
                    zero,
                    zero,
                );
            }
        }
        if let Some(track) = self.ivars().track.get() {
            track.setNeedsDisplay(true);
        }
    }

    /// Changes the zooms for display only (while dragging in the track).
    pub fn set_zooms_quietly(&self, zooms: Vec<Zoom>) {
        self.ivars().project.borrow_mut().zooms = zooms;
    }

    /// Applies the zooms changed while dragging to the preview.
    pub fn commit_zooms(&self) {
        let project = self.ivars().project.borrow().clone();
        self.apply(project);
    }

    /// Adds a two-second zoom on the cursor at `t` (shortened to fit before
    /// the next one).
    pub fn add_zoom(&self, t: f64) {
        let mut project = self.ivars().project.borrow().clone();
        let next = project
            .zooms
            .iter()
            .filter(|z| z.start > t)
            .map(|z| z.start)
            .fold(self.duration(), f64::min);
        if project.zooms.iter().any(|z| t >= z.start && t <= z.end) || next - t < 0.5 {
            return;
        }
        project.zooms.push(Zoom {
            start: t,
            end: (t + 2.0).min(next),
            level: 2.0,
            focus: Focus::Cursor,
            auto: false,
        });
        project.zooms.sort_by(|a, b| a.start.total_cmp(&b.start));
        let index = project.zooms.iter().position(|z| z.start == t);
        self.ivars().selected.set(index);
        self.apply(project);
    }

    pub fn delete_zoom(&self) {
        let Some(index) = self.ivars().selected.take() else {
            return;
        };
        let mut project = self.ivars().project.borrow().clone();
        if index < project.zooms.len() {
            project.zooms.remove(index);
            self.apply(project);
        }
    }

    /// Replaces the automatic zooms with freshly made ones; zooms added or
    /// changed by hand stay.
    fn regenerate(&self) {
        let ivars = self.ivars();
        let mut project = ivars.project.borrow().clone();
        let scale = if ivars.events.scale > 0.0 {
            ivars.events.scale
        } else {
            1.0
        };
        let made = super::zoom::automatic(
            &ivars.events,
            (ivars.video.0 / scale, ivars.video.1 / scale),
            ivars.duration,
        );
        project.zooms.retain(|z| !z.auto);
        let kept = project.zooms.clone();
        project.zooms.extend(
            made.into_iter()
                .filter(|m| !kept.iter().any(|k| m.start < k.end && k.start < m.end)),
        );
        project.zooms.sort_by(|a, b| a.start.total_cmp(&b.start));
        ivars.selected.set(None);
        self.apply(project);
    }

    /// Reads the selected zoom's controls.
    fn apply_zoom_controls(&self) {
        let (Some(ui), Some(index)) = (self.ivars().ui.get(), self.ivars().selected.get()) else {
            return;
        };
        let mut project = self.ivars().project.borrow().clone();
        let Some(zoom) = project.zooms.get_mut(index) else {
            return;
        };
        zoom.level = (ui.zoom_level.doubleValue() * 4.0).round() / 4.0;
        zoom.focus = if ui.zoom_follow.state() == NSControlStateValueOn {
            Focus::Cursor
        } else {
            // Stay where the view is in the middle of the zoom.
            let middle = (zoom.start + zoom.end) / 2.0;
            let view = self.ivars().renderer.borrow().view(middle);
            Focus::Fixed {
                x: view.center.x,
                y: view.center.y,
            }
        };
        zoom.auto = false;
        self.apply(project);
    }

    fn apply(&self, project: Project) {
        if let Err(e) = self.set_project(project) {
            tracing::error!(error = %e, "studio change not applied");
            crate::ui::toast::show(self.mtm(), &e);
        }
        self.sync_controls();
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

/// Keeps a copy of the watermark's mark in the package, for when the logo
/// file moves; removes it when there is no watermark.
fn save_watermark(package: &std::path::Path, style: Option<&crate::watermark::Style>) {
    let path = package.join(package::WATERMARK);
    match style.map(crate::watermark::mark) {
        Some(Ok(mark)) => {
            if let Some(png) =
                crate::capture::image::encode(&mark, crate::config::ImageFormat::Png, 1.0)
                && let Err(e) = std::fs::write(&path, png)
            {
                tracing::warn!(error = %e, "the watermark was not copied into the recording");
            }
        }
        Some(Err(_)) => {}
        None => {
            let _ = std::fs::remove_file(&path);
        }
    }
}
