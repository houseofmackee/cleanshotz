//! Collects a Studio recording's events while it records: the cursor
//! (sampled 120 times a second), its image when it changes, clicks and
//! keys. Main thread only. Times are kept as raw clock readings and turned
//! into video time when the recording ends and the first frame is known.

use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::ptr::NonNull;
use std::time::Instant;

use block2::RcBlock;
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSCursor, NSEvent, NSEventMask, NSEventType, NSScreen};
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::CGImage;
use objc2_foundation::{NSRect, NSTimer};

use super::events::{
    Button, CameraTrack, Click, CursorImage, CursorSample, Events, KeyPress, Timeline, VERSION,
};
use crate::capture::geometry::{Point, Rect};
use crate::capture::image;
use crate::config::ImageFormat;

/// Cursor samples per second.
const RATE: f64 = 120.0;
/// Every how many samples the cursor image is checked.
const IMAGE_EVERY: u32 = 12;

struct Shape {
    hash: u64,
    image: CFRetained<CGImage>,
    hotspot: Point,
    size: (f64, f64),
}

struct Collector {
    started: Instant,
    /// The recorded area, global points with a top-left origin.
    area: Rect,
    /// Height of the main screen, to turn Cocoa coordinates around.
    primary_height: f64,
    cursor: Vec<(f64, Point, u32)>,
    clicks: Vec<(f64, Point, Button, bool)>,
    keys: Vec<(f64, String)>,
    shapes: Vec<Shape>,
    current_shape: u32,
    ticks: u32,
    pauses: Vec<(f64, Option<f64>)>,
    timer: Retained<NSTimer>,
    monitors: Vec<Retained<AnyObject>>,
}

thread_local! {
    static ACTIVE: RefCell<Option<Collector>> = const { RefCell::new(None) };
}

/// Starts collecting for a recording of `area` (global points, top-left
/// origin).
pub fn start(mtm: MainThreadMarker, area: Rect) {
    stop_collecting();
    let primary_height = NSScreen::screens(mtm)
        .firstObject()
        .map_or(0.0, |s| s.frame().size.height);
    let block = RcBlock::new(|_timer: NonNull<NSTimer>| tick());
    // SAFETY: the block has the expected signature; the timer is
    // invalidated when collecting stops.
    let timer =
        unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(1.0 / RATE, true, &block) };
    let mask = NSEventMask::LeftMouseDown
        | NSEventMask::LeftMouseUp
        | NSEventMask::RightMouseDown
        | NSEventMask::RightMouseUp;
    let mut monitors = Vec::new();
    // Clicks in other apps...
    let global = RcBlock::new(|event: NonNull<NSEvent>| {
        // SAFETY: AppKit passes a valid event.
        note_click(unsafe { event.as_ref() });
    });
    if let Some(m) = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(mask, &global) {
        monitors.push(m);
    }
    // ...and in CleanShotZ's own windows.
    let local = RcBlock::new(|event: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit passes a valid event.
        note_click(unsafe { event.as_ref() });
        event.as_ptr()
    });
    // SAFETY: the handler returns the event unchanged.
    if let Some(m) = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &local) }
    {
        monitors.push(m);
    }
    ACTIVE.with(|a| {
        *a.borrow_mut() = Some(Collector {
            started: Instant::now(),
            area,
            primary_height,
            cursor: Vec::new(),
            clicks: Vec::new(),
            keys: Vec::new(),
            shapes: Vec::new(),
            current_shape: 0,
            ticks: 0,
            pauses: Vec::new(),
            timer,
            monitors,
        });
    });
    tick();
}

impl Collector {
    fn now(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    /// The pointer, in points from the top-left of the recorded area.
    fn pointer(&self) -> Point {
        let p = NSEvent::mouseLocation();
        Point::new(p.x - self.area.x, self.primary_height - p.y - self.area.y)
    }

    fn paused(&self) -> bool {
        self.pauses.last().is_some_and(|(_, end)| end.is_none())
    }

    /// The id of the current cursor image, adding it if it is new.
    fn check_shape(&mut self) {
        // Deprecated in favor of ScreenCaptureKit's own cursor, but still the
        // only public way to see another app's cursor shape.
        #[allow(deprecated)]
        let cursor = NSCursor::currentSystemCursor().unwrap_or_else(NSCursor::arrowCursor);
        let ns = cursor.image();
        let size = ns.size();
        let mut rect = NSRect::new(objc2_foundation::NSPoint::ZERO, size);
        // SAFETY: a valid image; no context or hints needed.
        let Some(cg) = (unsafe { ns.CGImageForProposedRect_context_hints(&mut rect, None, None) })
        else {
            return;
        };
        let Some((_, _, pixels)) = image::to_pixels(&cg) else {
            return;
        };
        let mut hasher = DefaultHasher::new();
        pixels.hash(&mut hasher);
        let hash = hasher.finish();
        let hotspot = cursor.hotSpot();
        let id = match self.shapes.iter().position(|s| s.hash == hash) {
            Some(i) => i,
            None => {
                self.shapes.push(Shape {
                    hash,
                    image: cg.into(),
                    hotspot: Point::new(hotspot.x, hotspot.y),
                    size: (size.width, size.height),
                });
                self.shapes.len() - 1
            }
        };
        self.current_shape = id as u32;
    }
}

fn tick() {
    ACTIVE.with(|a| {
        let mut active = a.borrow_mut();
        let Some(c) = active.as_mut() else {
            return;
        };
        if c.paused() {
            return;
        }
        if c.ticks.is_multiple_of(IMAGE_EVERY) {
            c.check_shape();
        }
        c.ticks += 1;
        let (t, p, shape) = (c.now(), c.pointer(), c.current_shape);
        c.cursor.push((t, p, shape));
    });
}

fn note_click(event: &NSEvent) {
    let (button, down) = match event.r#type() {
        NSEventType::LeftMouseDown => (Button::Left, true),
        NSEventType::LeftMouseUp => (Button::Left, false),
        NSEventType::RightMouseDown => (Button::Right, true),
        NSEventType::RightMouseUp => (Button::Right, false),
        _ => return,
    };
    ACTIVE.with(|a| {
        if let Some(c) = a.borrow_mut().as_mut()
            && !c.paused()
        {
            let (t, p) = (c.now(), c.pointer());
            c.clicks.push((t, p, button, down));
        }
    });
}

/// A key as the keystroke display shows it.
pub fn note_key(label: &str) {
    ACTIVE.with(|a| {
        if let Some(c) = a.borrow_mut().as_mut()
            && !c.paused()
        {
            let t = c.now();
            c.keys.push((t, label.to_string()));
        }
    });
}

pub fn set_paused(paused: bool) {
    ACTIVE.with(|a| {
        if let Some(c) = a.borrow_mut().as_mut() {
            let now = c.now();
            if paused && !c.paused() {
                c.pauses.push((now, None));
            } else if !paused && let Some(last) = c.pauses.last_mut() {
                last.1.get_or_insert(now);
            }
        }
    });
}

fn stop_collecting() -> Option<Collector> {
    let collector = ACTIVE.with(|a| a.borrow_mut().take())?;
    collector.timer.invalidate();
    for monitor in &collector.monitors {
        // SAFETY: a monitor returned by addGlobal/LocalMonitor....
        unsafe { NSEvent::removeMonitor(monitor) };
    }
    Some(collector)
}

/// Stops collecting without saving (a discarded recording).
pub fn cancel() {
    stop_collecting();
}

/// Stops collecting and writes `events.json` and the cursor images into
/// the package `dir`. `first_frame` is when the video's first frame
/// arrived; `scale` the video's pixels per point.
pub fn finish(
    dir: &Path,
    first_frame: Option<Instant>,
    scale: f64,
    camera: Option<(&str, Instant)>,
) -> Result<Events, String> {
    let c = stop_collecting().ok_or("no Studio recording was running")?;
    let first_frame = first_frame
        .map(|f| f.saturating_duration_since(c.started).as_secs_f64())
        .unwrap_or(0.0);
    let timeline = Timeline {
        first_frame,
        pauses: c.pauses.clone(),
    };
    let cursors = dir.join("cursors");
    std::fs::create_dir_all(&cursors).map_err(|e| e.to_string())?;
    let mut cursor_images = Vec::new();
    for (id, shape) in c.shapes.iter().enumerate() {
        let file = format!("cursors/{id}.png");
        let png = image::encode(&shape.image, ImageFormat::Png, 1.0)
            .ok_or("could not encode a cursor")?;
        std::fs::write(dir.join(&file), png).map_err(|e| e.to_string())?;
        cursor_images.push(CursorImage {
            id: id as u32,
            file,
            hotspot: shape.hotspot,
            width: shape.size.0,
            height: shape.size.1,
        });
    }
    let events = Events {
        version: VERSION,
        area: c.area,
        scale,
        cursor: c
            .cursor
            .iter()
            .filter_map(|&(t, p, image)| {
                timeline.video_time(t).map(|t| CursorSample {
                    t,
                    x: p.x,
                    y: p.y,
                    image,
                })
            })
            .collect(),
        cursor_images,
        clicks: c
            .clicks
            .iter()
            .filter_map(|&(t, p, button, down)| {
                timeline.video_time(t).map(|t| Click {
                    t,
                    x: p.x,
                    y: p.y,
                    button,
                    down,
                })
            })
            .collect(),
        keys: c
            .keys
            .iter()
            .filter_map(|(t, label)| {
                timeline.video_time(*t).map(|t| KeyPress {
                    t,
                    label: label.clone(),
                })
            })
            .collect(),
        camera: camera.map(|(file, started)| CameraTrack {
            file: file.to_string(),
            offset: started.saturating_duration_since(c.started).as_secs_f64() - first_frame,
        }),
    };
    let json = serde_json::to_vec(&events).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("events.json"), json).map_err(|e| e.to_string())?;
    tracing::info!(
        samples = events.cursor.len(),
        clicks = events.clicks.len(),
        keys = events.keys.len(),
        cursors = events.cursor_images.len(),
        "studio events saved"
    );
    Ok(events)
}
