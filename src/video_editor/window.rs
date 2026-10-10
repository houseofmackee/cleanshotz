//! The video editor window: a player with QuickTime-style trimming and an
//! options bar (mute, resolution, format, save).

use std::cell::{Cell, OnceCell};
use std::path::{Path, PathBuf};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSApplication, NSAutoresizingMaskOptions, NSBackingStoreType, NSButton,
    NSControlStateValueOn, NSModalResponseOK, NSPopUpButton, NSSavePanel, NSScreen, NSTextField,
    NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_av_foundation::{AVPlayer, AVPlayerItem};
use objc2_av_kit::{AVPlayerView, AVPlayerViewControlsStyle, AVPlayerViewTrimResult};
use objc2_core_media::{CMTime, CMTimeFlags};
use objc2_foundation::{
    NSNotification, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSURL,
};

use super::plan::{self, Format, Plan, Resolution};
use super::{export, registry};
use crate::record::media;
use crate::ui::toast;

const BAR_HEIGHT: f64 = 52.0;
/// Narrowest window that fits the bottom bar: the controls on the left end
/// at 554 points, Save As and Save take 194 on the right.
const MIN_WIDTH: f64 = 760.0;

pub struct Ivars {
    pub id: u64,
    source: PathBuf,
    duration: f64,
    size: (f64, f64),
    gif_fps: u32,
    gif_max_width: u32,
    window: OnceCell<Retained<NSWindow>>,
    player: OnceCell<Retained<AVPlayer>>,
    player_view: OnceCell<Retained<AVPlayerView>>,
    length: OnceCell<Retained<NSTextField>>,
    controls: OnceCell<Vec<Retained<AnyObject>>>,
    mute: Cell<bool>,
    resolution: Cell<Resolution>,
    format: Cell<Format>,
    busy: Cell<bool>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    pub struct VideoEditor;

    unsafe impl NSObjectProtocol for VideoEditor {}

    unsafe impl NSWindowDelegate for VideoEditor {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            if let Some(player) = self.ivars().player.get() {
                // SAFETY: plain player control.
                unsafe { player.pause() };
            }
            registry::remove(self.ivars().id);
        }
    }

    impl VideoEditor {
        #[unsafe(method(trim:))]
        fn trim_action(&self, _sender: &AnyObject) {
            let Some(view) = self.ivars().player_view.get() else {
                return;
            };
            // SAFETY: plain player view calls.
            unsafe {
                if !view.canBeginTrimming() {
                    return;
                }
                let id = self.ivars().id;
                let done = RcBlock::new(move |result: AVPlayerViewTrimResult| {
                    if result == AVPlayerViewTrimResult::OKButton
                        && let Some(editor) = registry::get(id)
                    {
                        editor.update_length();
                    }
                });
                view.beginTrimmingWithCompletionHandler(Some(&done));
            }
        }

        #[unsafe(method(muteChanged:))]
        fn mute_changed(&self, sender: &NSButton) {
            let mute = sender.state() == NSControlStateValueOn;
            self.ivars().mute.set(mute);
            if let Some(player) = self.ivars().player.get() {
                // SAFETY: plain player property.
                unsafe { player.setMuted(mute) };
            }
        }

        #[unsafe(method(resolutionChanged:))]
        fn resolution_changed(&self, sender: &NSPopUpButton) {
            let index = sender.indexOfSelectedItem().max(0) as usize;
            self.ivars()
                .resolution
                .set(Resolution::ALL.get(index).copied().unwrap_or(Resolution::Original));
        }

        #[unsafe(method(formatChanged:))]
        fn format_changed(&self, sender: &NSPopUpButton) {
            let format = if sender.indexOfSelectedItem() == 1 { Format::Gif } else { Format::Mp4 };
            self.ivars().format.set(format);
        }

        #[unsafe(method(save:))]
        fn save_action(&self, _sender: &AnyObject) {
            self.save(None);
        }

        #[unsafe(method(saveAs:))]
        fn save_as_action(&self, _sender: &AnyObject) {
            let ext = match self.ivars().format.get() {
                Format::Mp4 => "mp4",
                Format::Gif => "gif",
            };
            let stem = self
                .ivars()
                .source
                .file_stem()
                .map_or_else(|| "Recording".into(), |s| s.to_string_lossy().into_owned());
            let panel = NSSavePanel::savePanel(self.mtm());
            panel.setNameFieldStringValue(&NSString::from_str(&format!("{stem}.{ext}")));
            if panel.runModal() != NSModalResponseOK {
                return;
            }
            if let Some(path) = panel.URL().and_then(|u| u.path()) {
                self.save(Some(PathBuf::from(path.to_string())));
            }
        }
    }
);

impl VideoEditor {
    pub fn open(
        mtm: MainThreadMarker,
        id: u64,
        source: &Path,
        gif_fps: u32,
        gif_max_width: u32,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            id,
            source: source.to_path_buf(),
            duration: media::duration(source),
            size: media::size(source),
            gif_fps,
            gif_max_width,
            window: OnceCell::new(),
            player: OnceCell::new(),
            player_view: OnceCell::new(),
            length: OnceCell::new(),
            controls: OnceCell::new(),
            mute: Cell::new(false),
            resolution: Cell::new(Resolution::Original),
            format: Cell::new(Format::Mp4),
            busy: Cell::new(false),
        });
        // SAFETY: NSObject's designated initializer.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        this.build(mtm);
        this
    }

    fn build(&self, mtm: MainThreadMarker) {
        let (w, h) = self.ivars().size;
        let visible = NSScreen::mainScreen(mtm).map_or(
            NSRect::new(NSPoint::ZERO, NSSize::new(1440.0, 900.0)),
            |s| s.visibleFrame(),
        );
        // Fit the video (in points, assuming Retina) in most of the screen.
        let (vw, vh) = if w > 0.0 && h > 0.0 {
            (w / 2.0, h / 2.0)
        } else {
            (800.0, 500.0)
        };
        let fit = (visible.size.width * 0.75 / vw)
            .min(visible.size.height * 0.75 / vh)
            .min(1.0);
        let width = (vw * fit).max(MIN_WIDTH);
        let height = (vh * fit).max(300.0) + BAR_HEIGHT;
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
        // SAFETY: the registry keeps the editor (and so the window) alive.
        unsafe { window.setReleasedWhenClosed(false) };
        let title = self
            .ivars()
            .source
            .file_name()
            .map_or_else(|| "Video".into(), |n| n.to_string_lossy().into_owned());
        window.setTitle(&NSString::from_str(&title));
        window.setContentMinSize(NSSize::new(MIN_WIDTH, 360.0));
        window.setDelegate(Some(ProtocolObject::from_ref(self)));
        let content = window.contentView().expect("windows have a content view");

        // SAFETY: creating a player for a local file.
        let (player, view) = unsafe {
            let url =
                NSURL::fileURLWithPath(&NSString::from_str(&self.ivars().source.to_string_lossy()));
            let item = AVPlayerItem::playerItemWithURL(&url, mtm);
            let player = AVPlayer::playerWithPlayerItem(Some(&item), mtm);
            let view = AVPlayerView::initWithFrame(
                AVPlayerView::alloc(mtm),
                NSRect::new(
                    NSPoint::new(0.0, BAR_HEIGHT),
                    NSSize::new(width, height - BAR_HEIGHT),
                ),
            );
            view.setControlsStyle(AVPlayerViewControlsStyle::Inline);
            view.setPlayer(Some(&player));
            (player, view)
        };
        view.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        content.addSubview(&view);

        let controls = self.build_bar(mtm, &content, width);
        let _ = self.ivars().controls.set(controls);
        let _ = self.ivars().player.set(player);
        let _ = self.ivars().player_view.set(view);
        let _ = self.ivars().window.set(window.clone());
        self.update_length();

        window.center();
        NSApplication::sharedApplication(mtm).activate();
        window.makeKeyAndOrderFront(None);
        // Front even when macOS declines to activate the app (opened from
        // the command line while the terminal is active).
        window.orderFrontRegardless();
    }

    fn build_bar(
        &self,
        mtm: MainThreadMarker,
        content: &objc2_app_kit::NSView,
        width: f64,
    ) -> Vec<Retained<AnyObject>> {
        let target: &AnyObject = self.as_ref();
        let left = NSAutoresizingMaskOptions::ViewMaxXMargin;
        let right = NSAutoresizingMaskOptions::ViewMinXMargin;
        let y = 12.0;
        let place = |view: &objc2_app_kit::NSView, x: f64, w: f64, mask| {
            view.setFrame(NSRect::new(NSPoint::new(x, y), NSSize::new(w, 28.0)));
            view.setAutoresizingMask(mask);
            content.addSubview(view);
        };
        // SAFETY: the actions exist on VideoEditor, which outlives its controls.
        let button = |title: &str, action| unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str(title),
                Some(target),
                Some(action),
                mtm,
            )
        };
        let popup = |items: &[&str], action| {
            let p = NSPopUpButton::initWithFrame_pullsDown(
                NSPopUpButton::alloc(mtm),
                NSRect::new(NSPoint::ZERO, NSSize::new(120.0, 28.0)),
                false,
            );
            for item in items {
                p.addItemWithTitle(&NSString::from_str(item));
            }
            // SAFETY: the action exists on VideoEditor.
            unsafe {
                p.setTarget(Some(target));
                p.setAction(Some(action));
            }
            p
        };

        let trim = button("Trim…", sel!(trim:));
        place(&trim, 12.0, 80.0, left);
        // SAFETY: the action exists on VideoEditor.
        let mute = unsafe {
            NSButton::checkboxWithTitle_target_action(
                &NSString::from_str("Mute"),
                Some(target),
                Some(sel!(muteChanged:)),
                mtm,
            )
        };
        place(&mute, 104.0, 70.0, left);
        let titles: Vec<&str> = Resolution::ALL.iter().map(|r| r.title()).collect();
        let resolution = popup(&titles, sel!(resolutionChanged:));
        place(&resolution, 178.0, 130.0, left);
        let format = popup(&["MP4", "GIF"], sel!(formatChanged:));
        place(&format, 314.0, 80.0, left);
        let length = NSTextField::labelWithString(&NSString::from_str(""), mtm);
        length.setTextColor(Some(&objc2_app_kit::NSColor::secondaryLabelColor()));
        place(&length, 404.0, 150.0, left);
        // Labels draw at the top of their frame; line the text up with
        // the buttons' titles.
        length.setFrameOrigin(NSPoint::new(404.0, y - 6.0));
        let save = button("Save", sel!(save:));
        place(&save, width - 92.0, 80.0, right);
        let save_as = button("Save As…", sel!(saveAs:));
        place(&save_as, width - 194.0, 96.0, right);
        let _ = self.ivars().length.set(length);

        let any = |v: &objc2_app_kit::NSView| -> Retained<AnyObject> { v.retain().into() };
        vec![
            any(&trim),
            any(&mute),
            any(&resolution),
            any(&format),
            any(&save),
            any(&save_as),
        ]
    }

    /// The trim range chosen in the player, in seconds.
    fn trim_range(&self) -> (f64, f64) {
        let seconds = |t: CMTime| {
            // SAFETY: pure CMTime query.
            t.flags
                .contains(CMTimeFlags::Valid)
                .then(|| unsafe { t.seconds() })
        };
        let (start, end) = self
            .ivars()
            .player
            .get()
            // SAFETY: plain player item property reads.
            .and_then(|p| unsafe { p.currentItem() })
            .map_or((None, None), |item| unsafe {
                (
                    seconds(item.reversePlaybackEndTime()),
                    seconds(item.forwardPlaybackEndTime()),
                )
            });
        plan::trim_range(self.ivars().duration, start, end)
    }

    fn update_length(&self) {
        let (start, end) = self.trim_range();
        let (w, h) = self.ivars().size;
        let text = format!("{} · {}×{}", format_length(end - start), w as u32, h as u32);
        if let Some(label) = self.ivars().length.get() {
            label.setStringValue(&NSString::from_str(&text));
        }
    }

    fn set_busy(&self, busy: bool) {
        self.ivars().busy.set(busy);
        for control in self.ivars().controls.get().into_iter().flatten() {
            if let Some(control) = control.downcast_ref::<objc2_app_kit::NSControl>() {
                control.setEnabled(!busy);
            }
        }
    }

    /// Exports over the source (Save, MP4), next to it (Save, GIF), or to
    /// `target` (Save As). Runs in the background, then closes on Save.
    fn save(&self, target: Option<PathBuf>) {
        if self.ivars().busy.get() {
            return;
        }
        let ivars = self.ivars();
        let (start, end) = self.trim_range();
        let plan = Plan {
            start,
            end,
            mute: ivars.mute.get(),
            resolution: ivars.resolution.get(),
            format: ivars.format.get(),
        };
        let source = ivars.source.clone();
        let save_as = target.is_some();
        if !save_as && !plan.changes_anything(ivars.duration, ivars.size) {
            if let Some(window) = ivars.window.get() {
                window.close();
            }
            return;
        }
        let output = target.unwrap_or_else(|| match plan.format {
            Format::Mp4 => source.clone(),
            Format::Gif => {
                let dir = source
                    .parent()
                    .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
                let stem = source
                    .file_stem()
                    .map_or_else(|| "Recording".into(), |s| s.to_string_lossy().into_owned());
                crate::capture::naming::unique_path(&dir, &stem, "gif")
            }
        });
        let preset = plan::preset(plan.resolution, ivars.size);
        let gif = (
            ivars.gif_fps,
            plan::gif_width(plan.resolution, ivars.size, ivars.gif_max_width),
        );
        let id = ivars.id;
        self.set_busy(true);
        toast::show(self.mtm(), "Exporting…");
        std::thread::spawn(move || {
            let result = export::export(&source, &plan, preset, gif, &output);
            let thumbnail = result
                .as_ref()
                .ok()
                .and_then(|()| media::thumbnail(&output));
            let duration = media::duration(&output);
            let thumbnail = thumbnail.map(super::AssertSend);
            dispatch2::DispatchQueue::main().exec_async(move || {
                let mtm = MainThreadMarker::new().expect("runs on the main thread");
                let Some(editor) = registry::get(id) else {
                    return;
                };
                editor.set_busy(false);
                match result {
                    Ok(()) => {
                        tracing::info!(path = %output.display(), "video exported");
                        let thumbnail = thumbnail.map(super::AssertSend::into_inner);
                        if output == source {
                            if let Some(thumbnail) = thumbnail {
                                crate::history::refresh(
                                    output.clone(),
                                    thumbnail.clone(),
                                    Some(duration),
                                );
                                crate::quick_access::refresh_video(&output, thumbnail, duration);
                            }
                        } else if let Some(thumbnail) = thumbnail {
                            crate::history::record_video(
                                output.clone(),
                                thumbnail.clone(),
                                duration,
                            );
                            crate::quick_access::add_video(
                                mtm,
                                output.clone(),
                                thumbnail,
                                2.0,
                                duration,
                                true,
                            );
                        }
                        if save_as {
                            toast::show(mtm, "Saved");
                        } else if let Some(window) = editor.ivars().window.get() {
                            window.close();
                        }
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "video export failed");
                        let alert = NSAlert::new(mtm);
                        alert.setMessageText(&NSString::from_str("Could not export the video"));
                        alert.setInformativeText(&NSString::from_str(&e));
                        alert.runModal();
                    }
                }
            });
        });
    }
}

/// `m:ss.s`
fn format_length(seconds: f64) -> String {
    let tenths = (seconds.max(0.0) * 10.0).round() as u64;
    format!("{}:{:02}.{}", tenths / 600, tenths / 10 % 60, tenths % 10)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_lengths_with_tenths() {
        assert_eq!(format_length(2.04), "0:02.0");
        assert_eq!(format_length(65.35), "1:05.4");
    }
}
