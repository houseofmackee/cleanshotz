//! A recording from selection to Quick Access: pick what to record, count
//! down, record with a control bar on screen, then finish the file.

use std::cell::RefCell;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBox, NSBoxType, NSButton, NSColor, NSFont, NSFontWeightMedium, NSImage, NSPanel, NSScreen,
    NSTextField, NSTitlePosition,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString, NSTimer};

use super::engine::{self, Recorder};
use super::media;
use super::overlays::{Camera, KeyDisplay};
use crate::capture::flow::{overlay_displays, screen_for};
use crate::capture::geometry::{Point, Rect};
use crate::capture::overlay::{self, Mode, Selection};
use crate::capture::sck::{self, CaptureError, Content};
use crate::capture::{countdown, naming, output};
use crate::config::{CaptureSettings, KeystrokeMode, RecordingSettings, VideoFormat};
use crate::platform::permissions::{Permission, Status};
use crate::ui::{panels, toast};

/// What to record.
#[derive(Clone, Copy, Debug)]
enum Target {
    /// Display-local points, top-left origin.
    Area {
        display_id: u32,
        rect: Rect,
    },
    Window {
        window_id: u32,
    },
}

struct Active {
    recorder: Recorder,
    camera: Option<Camera>,
    keys: Option<KeyDisplay>,
    controls: Retained<NSPanel>,
    time_label: Retained<NSTextField>,
    pause_button: Retained<NSButton>,
    border: Option<Retained<NSPanel>>,
    timer: Retained<NSTimer>,
    started: Instant,
    paused_at: Option<Instant>,
    paused_total: Duration,
    finish: Finish,
}

/// Where the finished recording goes.
#[derive(Clone)]
struct Finish {
    /// The file being written.
    video: PathBuf,
    /// For GIF recordings, the GIF to convert to.
    gif: Option<PathBuf>,
    saved: bool,
    scale: f64,
    gif_fps: u32,
    gif_max_width: u32,
}

enum State {
    Idle,
    /// Selecting, counting down or starting.
    Preparing,
    Recording(Box<Active>),
    Finishing,
}

thread_local! {
    static STATE: RefCell<State> = const { RefCell::new(State::Idle) };
    static CONTROLLER: RefCell<Option<Retained<ControlTarget>>> = const { RefCell::new(None) };
}

pub fn is_busy() -> bool {
    STATE.with(|s| !matches!(*s.borrow(), State::Idle))
}

pub fn is_recording() -> bool {
    STATE.with(|s| matches!(*s.borrow(), State::Recording(_)))
}

/// Starts a recording, or stops the one in progress.
/// Starts or stops a recording. With `area` (a display id and a
/// display-local rect) recording starts there without asking.
pub fn toggle(
    mtm: MainThreadMarker,
    capture: CaptureSettings,
    recording: RecordingSettings,
    area: Option<(u32, Rect)>,
) -> Result<(), CaptureError> {
    if is_recording() {
        stop(mtm, false);
        return Ok(());
    }
    if is_busy() {
        return Ok(());
    }
    if let Some((display_id, rect)) = area {
        STATE.with(|s| *s.borrow_mut() = State::Preparing);
        start_target(mtm, Target::Area { display_id, rect }, capture, recording);
        return Ok(());
    }
    let content = Content::fetch()?;
    let displays = overlay_displays(mtm, &content, &capture, capture.show_magnifier)?;
    let options = overlay::Options {
        mode: Mode::Area,
        magnifier: capture.show_magnifier,
        freeze: false,
        windows: content.pickable_windows(),
        click_selects_display: true,
        hint: Some(
            "Drag to record an area · Click to record the screen · Space for a window · Esc to cancel"
                .into(),
        ),
    };
    STATE.with(|s| *s.borrow_mut() = State::Preparing);
    overlay::begin(mtm, displays, options, move |selection| {
        let target = match selection {
            Some(Selection::Area {
                display_id, rect, ..
            }) => Target::Area { display_id, rect },
            Some(Selection::Window { window_id }) => Target::Window { window_id },
            None => {
                STATE.with(|s| *s.borrow_mut() = State::Idle);
                return;
            }
        };
        start_target(mtm, target, capture, recording);
    });
    Ok(())
}

/// Counts down if set to, then starts recording `target`.
fn start_target(
    mtm: MainThreadMarker,
    target: Target,
    capture: CaptureSettings,
    recording: RecordingSettings,
) {
    let seconds = recording.countdown_seconds.min(10);
    let begin_now = move || {
        if let Err(e) = begin(mtm, target, &capture, &recording) {
            STATE.with(|s| *s.borrow_mut() = State::Idle);
            tracing::error!(error = %e, "could not start recording");
            crate::app::alert::show(mtm, "Could not start recording", &e, &["OK"]);
        }
    };
    match (seconds, countdown_place(mtm, target)) {
        (0, _) | (_, None) => begin_now(),
        (seconds, Some((screen, rect))) => countdown::start(mtm, &screen, rect, seconds, begin_now),
    }
}

/// The screen and display-local rect to center the countdown on.
fn countdown_place(mtm: MainThreadMarker, target: Target) -> Option<(Retained<NSScreen>, Rect)> {
    match target {
        Target::Area { display_id, rect } => screen_for(mtm, display_id).map(|s| (s, rect)),
        Target::Window { .. } => {
            let screen = crate::capture::screen_under_mouse(mtm)?;
            let f = screen.frame();
            Some((screen, Rect::new(0.0, 0.0, f.size.width, f.size.height)))
        }
    }
}

fn even(pixels: f64) -> usize {
    ((pixels / 2.0).round() as usize * 2).max(2)
}

fn begin(
    mtm: MainThreadMarker,
    target: Target,
    capture: &CaptureSettings,
    settings: &RecordingSettings,
) -> Result<(), String> {
    // The camera and keystroke overlays must exist before the content is
    // fetched, so the recording filter can include them.
    let (camera, keys) = match target {
        Target::Area { display_id, rect } => {
            let screen = screen_for(mtm, display_id).ok_or("the display is no longer available")?;
            let f = screen.frame();
            let area = NSRect::new(
                NSPoint::new(
                    f.origin.x + rect.x,
                    f.origin.y + f.size.height - rect.y - rect.height,
                ),
                NSSize::new(rect.width, rect.height),
            );
            start_overlays(mtm, area, settings)
        }
        Target::Window { .. } => {
            if settings.camera || settings.keystrokes != KeystrokeMode::Off {
                toast::show(
                    mtm,
                    "The camera and keystrokes are not shown in window recordings",
                );
            }
            (None, None)
        }
    };
    // On failure `begin_recording` stops the overlays itself.
    begin_recording(mtm, target, capture, settings, camera, keys)
}

/// Starts the camera and keystroke overlays the settings ask for. Problems
/// (such as missing permissions) are shown and the recording goes ahead
/// without that overlay.
fn start_overlays(
    mtm: MainThreadMarker,
    area: NSRect,
    settings: &RecordingSettings,
) -> (Option<Camera>, Option<KeyDisplay>) {
    let camera = settings.camera.then(|| {
        Camera::start(
            mtm,
            area,
            settings.camera_size as f64,
            settings.camera_shape,
        )
        .inspect_err(|e| {
            tracing::warn!(error = %e, "camera not shown");
            toast::show(mtm, e);
        })
        .ok()
    });
    let keys = (settings.keystrokes != KeystrokeMode::Off).then(|| {
        KeyDisplay::start(mtm, area, settings.keystrokes)
            .inspect_err(|e| {
                tracing::warn!(error = %e, "keystrokes not shown");
                toast::show(mtm, e);
            })
            .ok()
    });
    (camera.flatten(), keys.flatten())
}

fn begin_recording(
    mtm: MainThreadMarker,
    target: Target,
    capture: &CaptureSettings,
    settings: &RecordingSettings,
    camera: Option<Camera>,
    keys: Option<KeyDisplay>,
) -> Result<(), String> {
    let stop_overlays = |camera: Option<Camera>, keys: Option<KeyDisplay>| {
        if let Some(c) = camera {
            c.stop();
        }
        if let Some(k) = keys {
            k.stop();
        }
    };
    // All windows, so the (still transparent) keystroke display is listed.
    let content = match Content::fetch_all() {
        Ok(content) => content,
        Err(e) => {
            stop_overlays(camera, keys);
            return Err(e.to_string());
        }
    };
    let (filter, source_rect, size, cocoa_rect, ignore_shadows) = match target {
        Target::Area { display_id, rect } => {
            let display = content
                .display(display_id)
                .ok_or("the display is no longer available")?;
            let filter = sck::recording_filter(&content, &display);
            let screen = screen_for(mtm, display_id).ok_or("the display is no longer available")?;
            let f = screen.frame();
            let cocoa = NSRect::new(
                NSPoint::new(
                    f.origin.x + rect.x,
                    f.origin.y + f.size.height - rect.y - rect.height,
                ),
                NSSize::new(rect.width, rect.height),
            );
            (filter, Some(rect), (rect.width, rect.height), cocoa, false)
        }
        Target::Window { window_id } => {
            let window = content
                .window(window_id)
                .ok_or("the window is no longer available")?;
            let filter = sck::window_filter(&window);
            let size = sck::content_size(&filter);
            // SAFETY: plain property read.
            let frame = unsafe { window.frame() };
            let primary_height = NSScreen::screens(mtm)
                .firstObject()
                .map_or(0.0, |s| s.frame().size.height);
            let cocoa = NSRect::new(
                NSPoint::new(
                    frame.origin.x,
                    primary_height - frame.origin.y - frame.size.height,
                ),
                frame.size,
            );
            (filter, None, (size.width, size.height), cocoa, true)
        }
    };
    let native = sck::pixel_scale(&filter);
    let scale = if settings.downscale_retina {
        1.0
    } else {
        native
    };
    let (width, height) = (even(size.0 * scale), even(size.1 * scale));

    let microphone = settings.microphone && microphone_ready(mtm);

    // Name and place the output like a capture.
    let stem = naming::file_stem(&capture.file_name, &chrono::Local::now(), false);
    let temp = output::temp_dir();
    let save_dir = naming::expand_home(&capture.save_directory);
    let saved = capture.save_to_file;
    let unsaved = output::unsaved_dir();
    let final_dir = if saved { &save_dir } else { &unsaved };
    std::fs::create_dir_all(&temp).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(final_dir).map_err(|e| e.to_string())?;
    let (video, gif) = match settings.format {
        VideoFormat::Mp4 => (naming::unique_path(final_dir, &stem, "mp4"), None),
        VideoFormat::Gif => (
            naming::unique_path(&temp, &stem, "mp4"),
            Some(naming::unique_path(final_dir, &stem, "gif")),
        ),
    };

    let recorder = match Recorder::start(engine::Options {
        filter,
        source_rect,
        width,
        height,
        fps: settings.fps(),
        codec: settings.codec_for(width, height),
        bitrate: settings.bitrate(width, height),
        show_cursor: settings.show_cursor,
        show_clicks: settings.show_clicks,
        ignore_shadows,
        system_audio: settings.system_audio,
        microphone,
        output: video.clone(),
    }) {
        Ok(recorder) => recorder,
        Err(e) => {
            stop_overlays(camera, keys);
            return Err(e);
        }
    };

    let full_screen = matches!(target, Target::Area { display_id, rect }
        if screen_for(mtm, display_id).is_some_and(|s| s.frame().size.width <= rect.width));
    let border = (!full_screen).then(|| panels::border_panel(mtm, cocoa_rect));
    let (controls, time_label, pause_button) = control_panel(mtm, cocoa_rect);
    let timer = tick_timer();
    STATE.with(|s| {
        *s.borrow_mut() = State::Recording(Box::new(Active {
            recorder,
            camera,
            keys,
            controls,
            time_label,
            pause_button,
            border,
            timer,
            started: Instant::now(),
            paused_at: None,
            paused_total: Duration::ZERO,
            finish: Finish {
                video,
                gif,
                saved,
                scale,
                gif_fps: settings.gif_fps(),
                gif_max_width: settings.gif_max_width,
            },
        }))
    });
    Ok(())
}

/// Requests microphone access if needed. Returns whether it can be used
/// for this recording.
fn microphone_ready(mtm: MainThreadMarker) -> bool {
    if !engine::macos_at_least(15) {
        toast::show(mtm, "Microphone recording needs macOS 15 or later");
        return false;
    }
    match Permission::Microphone.status() {
        Status::Granted => true,
        Status::NotDetermined => {
            Permission::Microphone.request();
            toast::show(
                mtm,
                "Allow microphone access, then record again to include it",
            );
            false
        }
        Status::Denied => {
            toast::show(mtm, "Microphone access is off in System Settings");
            false
        }
    }
}

/// Stops the recording. `discard` deletes it instead of keeping it.
pub fn stop(mtm: MainThreadMarker, discard: bool) {
    let active = STATE.with(|s| {
        let mut state = s.borrow_mut();
        match std::mem::replace(&mut *state, State::Finishing) {
            State::Recording(active) => Some(active),
            other => {
                *state = other;
                None
            }
        }
    });
    let Some(active) = active else {
        return;
    };
    let Active {
        recorder,
        camera,
        keys,
        controls,
        border,
        timer,
        finish,
        ..
    } = *active;
    timer.invalidate();
    if let Some(camera) = camera {
        camera.stop();
    }
    if let Some(keys) = keys {
        keys.stop();
    }
    controls.orderOut(None);
    if let Some(border) = &border {
        border.orderOut(None);
    }
    if !discard {
        toast::show(mtm, "Finishing recording…");
    }
    recorder.stop(move |result| {
        let mtm = MainThreadMarker::new().expect("runs on the main thread");
        match result {
            Ok(video) if discard => {
                let _ = std::fs::remove_file(&video);
                tracing::info!("recording discarded");
                STATE.with(|s| *s.borrow_mut() = State::Idle);
            }
            Ok(_) => deliver(finish),
            Err(e) => {
                tracing::error!(error = %e, "recording failed");
                STATE.with(|s| *s.borrow_mut() = State::Idle);
                crate::app::alert::show(mtm, "The recording could not be saved", &e, &["OK"]);
            }
        }
    });
}

/// Converts (for GIFs) and reads the thumbnail off the main thread, then
/// shows the recording in Quick Access.
fn deliver(finish: Finish) {
    std::thread::spawn(move || {
        let thumbnail = media::thumbnail(&finish.video);
        let duration = media::duration(&finish.video);
        let file = match &finish.gif {
            Some(gif) => {
                let converted =
                    media::to_gif(&finish.video, gif, finish.gif_fps, finish.gif_max_width);
                let _ = std::fs::remove_file(&finish.video);
                converted.map(|()| gif.clone())
            }
            None => Ok(finish.video.clone()),
        };
        let thumbnail = thumbnail.map(AssertSend);
        dispatch2::DispatchQueue::main().exec_async(move || {
            let mtm = MainThreadMarker::new().expect("runs on the main thread");
            STATE.with(|s| *s.borrow_mut() = State::Idle);
            match (file, thumbnail) {
                (Ok(file), Some(thumbnail)) => {
                    tracing::info!(path = %file.display(), duration, "recording saved");
                    crate::history::record_video(file.clone(), thumbnail.0.clone(), duration);
                    if crate::upload::auto_upload() {
                        crate::upload::start(mtm, file.clone());
                    }
                    crate::quick_access::add_video(
                        mtm,
                        file,
                        thumbnail.into_inner(),
                        finish.scale,
                        duration,
                        finish.saved,
                    );
                }
                (Ok(file), None) => {
                    tracing::warn!(path = %file.display(), "recording saved without a thumbnail");
                    toast::show(mtm, "Recording saved");
                }
                (Err(e), _) => {
                    tracing::error!(error = %e, "GIF conversion failed");
                    crate::app::alert::show(mtm, "Could not create the GIF", &e, &["OK"]);
                }
            }
        });
    });
}

/// Moves a value to another thread. CGImage is thread-safe.
struct AssertSend<T>(T);
// SAFETY: only used for CGImage, which CoreGraphics documents as immutable
// and thread-safe.
unsafe impl<T> Send for AssertSend<T> {}
impl<T> AssertSend<T> {
    fn into_inner(self) -> T {
        self.0
    }
}

pub fn toggle_pause() {
    STATE.with(|s| {
        let mut state = s.borrow_mut();
        let State::Recording(active) = &mut *state else {
            return;
        };
        let paused = match active.paused_at.take() {
            Some(at) => {
                active.paused_total += at.elapsed();
                false
            }
            None => {
                active.paused_at = Some(Instant::now());
                true
            }
        };
        active.recorder.set_paused(paused);
        let symbol = if paused { "play.fill" } else { "pause.fill" };
        active
            .pause_button
            .setImage(symbol_image(symbol).as_deref());
        update_time(active);
    });
}

/// The system ended the stream (for example from the screen sharing menu):
/// keep what was recorded.
pub fn stream_stopped_by_system() {
    if let Some(mtm) = MainThreadMarker::new()
        && is_recording()
    {
        stop(mtm, false);
    }
}

fn update_time(active: &Active) {
    let paused_now = active.paused_at.map_or(Duration::ZERO, |at| at.elapsed());
    let elapsed = active
        .started
        .elapsed()
        .saturating_sub(active.paused_total + paused_now);
    let label = crate::quick_access::format_duration(elapsed.as_secs_f64().floor());
    active
        .time_label
        .setStringValue(&NSString::from_str(&label));
}

fn tick_timer() -> Retained<NSTimer> {
    let block = RcBlock::new(|_timer: NonNull<NSTimer>| {
        STATE.with(|s| {
            if let State::Recording(active) = &*s.borrow() {
                update_time(active);
            }
        });
    });
    // SAFETY: the block has the expected signature; the run loop owns the
    // timer until it is invalidated in `stop`.
    unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.25, true, &block) }
}

fn symbol_image(name: &str) -> Option<Retained<NSImage>> {
    NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str(name), None)
}

const BAR: NSSize = NSSize {
    width: 220.0,
    height: 44.0,
};

fn control_panel(
    mtm: MainThreadMarker,
    area: NSRect,
) -> (Retained<NSPanel>, Retained<NSTextField>, Retained<NSButton>) {
    let visible = NSScreen::screens(mtm)
        .iter()
        .find(|s| {
            let f = s.frame();
            let c = Point::new(
                area.origin.x + area.size.width / 2.0,
                area.origin.y + area.size.height / 2.0,
            );
            Rect::new(f.origin.x, f.origin.y, f.size.width, f.size.height).contains(c)
        })
        .or_else(|| NSScreen::mainScreen(mtm))
        .map_or(area, |s| s.visibleFrame());
    let panel = panels::floating_panel(
        mtm,
        NSRect::new(panels::control_origin(area, visible, BAR), BAR),
    );
    panel.setHasShadow(true);

    let background = NSBox::new(mtm);
    background.setBoxType(NSBoxType::Custom);
    background.setTitlePosition(NSTitlePosition::NoTitle);
    background.setBorderWidth(0.0);
    background.setCornerRadius(BAR.height / 2.0);
    background.setFillColor(&NSColor::colorWithWhite_alpha(0.1, 0.9));
    background.setFrame(NSRect::new(NSPoint::ZERO, BAR));
    panel.setContentView(Some(&background));

    let dot = NSTextField::labelWithString(&NSString::from_str("●"), mtm);
    dot.setTextColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
        1.0, 0.25, 0.2, 1.0,
    )));
    dot.setFrame(NSRect::new(
        NSPoint::new(18.0, 13.0),
        NSSize::new(16.0, 18.0),
    ));
    background.addSubview(&dot);

    let time = NSTextField::labelWithString(&NSString::from_str("0:00"), mtm);
    // SAFETY: reading an AppKit constant.
    time.setFont(Some(&NSFont::monospacedDigitSystemFontOfSize_weight(
        15.0,
        unsafe { NSFontWeightMedium },
    )));
    time.setTextColor(Some(&NSColor::whiteColor()));
    time.setFrame(NSRect::new(
        NSPoint::new(36.0, 12.0),
        NSSize::new(64.0, 20.0),
    ));
    background.addSubview(&time);

    let target = ControlTarget::shared(mtm);
    let target: &AnyObject = target.as_ref();
    let button = |symbol: &str, tip: &str, action, x: f64| {
        let image = symbol_image(symbol).unwrap_or_default();
        // SAFETY: the action exists on ControlTarget, which lives for the
        // whole process.
        let b = unsafe {
            NSButton::buttonWithImage_target_action(&image, Some(target), Some(action), mtm)
        };
        b.setBordered(false);
        b.setContentTintColor(Some(&NSColor::whiteColor()));
        b.setToolTip(Some(&NSString::from_str(tip)));
        b.setFrame(NSRect::new(NSPoint::new(x, 8.0), NSSize::new(32.0, 28.0)));
        background.addSubview(&b);
        b
    };
    let pause = button("pause.fill", "Pause or resume", sel!(togglePause:), 104.0);
    button("stop.fill", "Stop and save", sel!(stopRecording:), 140.0);
    button("trash", "Stop and discard", sel!(discardRecording:), 176.0);
    panel.orderFrontRegardless();
    (panel, time, pause)
}

define_class!(
    /// Target for the control bar's buttons.
    // SAFETY: NSObject has no subclassing requirements; no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    struct ControlTarget;

    impl ControlTarget {
        #[unsafe(method(togglePause:))]
        fn toggle_pause_action(&self, _sender: &AnyObject) {
            toggle_pause();
        }

        #[unsafe(method(stopRecording:))]
        fn stop_action(&self, _sender: &AnyObject) {
            stop(self.mtm(), false);
        }

        #[unsafe(method(discardRecording:))]
        fn discard_action(&self, _sender: &AnyObject) {
            stop(self.mtm(), true);
        }
    }
);

impl ControlTarget {
    fn shared(mtm: MainThreadMarker) -> Retained<Self> {
        CONTROLLER.with(|c| {
            c.borrow_mut()
                .get_or_insert_with(|| {
                    let this = Self::alloc(mtm).set_ivars(());
                    // SAFETY: NSObject's designated initializer.
                    unsafe { msg_send![super(this), init] }
                })
                .clone()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_even() {
        assert_eq!(even(1601.0), 1602);
        assert_eq!(even(1599.4), 1600);
        assert_eq!(even(0.2), 2);
    }
}
