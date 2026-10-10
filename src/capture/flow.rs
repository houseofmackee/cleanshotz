//! Interactive capture flows: snapshot the displays, show the overlay, then
//! capture what the user picked.

use std::path::{Path, PathBuf};

use objc2::MainThreadMarker;
use objc2_app_kit::NSScreen;

use super::geometry::{Point, Rect};
use super::overlay::{self, Mode, OverlayDisplay, Selection};
use super::sck::{self, CaptureError, CaptureOptions, Content};
use super::{capture_rect, capture_window, countdown, deliver_from_snapshot, screen_display_id};
use crate::config::CaptureSettings;
use crate::state::{SavedArea, State};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Area,
    Window,
    SelfTimer,
}

/// Called with errors that happen after the overlay closes (the flow is
/// asynchronous from the caller's point of view).
pub type ErrorHandler = fn(MainThreadMarker, CaptureError);

pub fn start(
    mtm: MainThreadMarker,
    kind: Kind,
    settings: CaptureSettings,
    state_path: PathBuf,
    on_error: ErrorHandler,
) -> Result<(), CaptureError> {
    if overlay::is_active() {
        return Ok(());
    }
    let freeze = settings.freeze_screen && kind != Kind::SelfTimer;
    let content = Content::fetch()?;
    let displays = overlay_displays(mtm, &content, &settings, freeze || settings.show_magnifier)?;
    let options = overlay::Options {
        mode: if kind == Kind::Window {
            Mode::Window
        } else {
            Mode::Area
        },
        magnifier: settings.show_magnifier,
        freeze,
        windows: content.pickable_windows(),
    };
    overlay::begin(mtm, displays, options, move |selection| {
        if let Err(e) = on_selection(mtm, selection, kind, &settings, &state_path, on_error) {
            on_error(mtm, e);
        }
    });
    Ok(())
}

/// Captures the last selected area again, or starts an area selection if
/// there is none yet.
pub fn capture_previous_area(
    mtm: MainThreadMarker,
    settings: CaptureSettings,
    state_path: PathBuf,
    on_error: ErrorHandler,
) -> Result<(), CaptureError> {
    match State::load(&state_path).last_area {
        Some(area) => capture_rect(mtm, area.display_id, area.rect(), &settings),
        None => start(mtm, Kind::Area, settings, state_path, on_error),
    }
}

fn on_selection(
    mtm: MainThreadMarker,
    selection: Option<Selection>,
    kind: Kind,
    settings: &CaptureSettings,
    state_path: &Path,
    on_error: ErrorHandler,
) -> Result<(), CaptureError> {
    let Some(selection) = selection else {
        tracing::info!("capture cancelled");
        return Ok(());
    };
    if let Selection::Area {
        display_id, rect, ..
    } = &selection
    {
        let mut state = State::load(state_path);
        state.last_area = Some(SavedArea::new(*display_id, *rect));
        state.save(state_path);
    }

    if kind == Kind::SelfTimer {
        let (screen, rect) = match &selection {
            Selection::Area {
                display_id, rect, ..
            } => (screen_for(mtm, *display_id), *rect),
            Selection::Window { .. } => (super::screen_under_mouse(mtm), Rect::default()),
        };
        let screen = screen.ok_or(CaptureError::DisplayGone)?;
        let settings = settings.clone();
        countdown::start(
            mtm,
            &screen,
            rect,
            settings.self_timer_seconds(),
            move || {
                if let Err(e) = capture_selection(mtm, selection, &settings) {
                    on_error(mtm, e);
                }
            },
        );
        return Ok(());
    }
    capture_selection(mtm, selection, settings)
}

fn capture_selection(
    mtm: MainThreadMarker,
    selection: Selection,
    settings: &CaptureSettings,
) -> Result<(), CaptureError> {
    match selection {
        Selection::Area {
            display_id,
            rect,
            frozen: Some((snapshot, scale)),
        } => deliver_from_snapshot(mtm, display_id, &snapshot, scale, rect, settings),
        Selection::Area {
            display_id, rect, ..
        } => capture_rect(mtm, display_id, rect, settings),
        Selection::Window { window_id } => capture_window(mtm, window_id, settings),
    }
}

fn screen_for(mtm: MainThreadMarker, display_id: u32) -> Option<objc2::rc::Retained<NSScreen>> {
    NSScreen::screens(mtm)
        .iter()
        .find(|s| screen_display_id(s) == Some(display_id))
}

/// One overlay display per screen, with a snapshot when `snapshot` is set.
fn overlay_displays(
    mtm: MainThreadMarker,
    content: &Content,
    settings: &CaptureSettings,
    snapshot: bool,
) -> Result<Vec<OverlayDisplay>, CaptureError> {
    let excluded = content.excluded_windows(settings.hide_desktop_icons);
    let mut displays = Vec::new();
    for screen in NSScreen::screens(mtm).iter() {
        let Some(id) = screen_display_id(&screen) else {
            continue;
        };
        let Some(display) = content.display(id) else {
            continue;
        };
        let filter = sck::display_filter(&display, &excluded);
        let scale = sck::pixel_scale(&filter);
        let size = sck::content_size(&filter);
        let snapshot = if snapshot {
            Some(sck::capture(
                &filter,
                &CaptureOptions {
                    source_rect: None,
                    width: (size.width * scale).round() as usize,
                    height: (size.height * scale).round() as usize,
                    show_cursor: false,
                    shadow: false,
                },
            )?)
        } else {
            None
        };
        // SAFETY: plain property read.
        let origin = unsafe { display.frame() }.origin;
        displays.push(OverlayDisplay {
            id,
            screen,
            cg_origin: Point::new(origin.x, origin.y),
            scale,
            snapshot,
        });
    }
    if displays.is_empty() {
        return Err(CaptureError::DisplayGone);
    }
    Ok(displays)
}
