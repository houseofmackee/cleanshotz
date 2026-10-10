//! Still captures: fullscreen, area and window.

pub mod countdown;
pub mod flow;
pub mod geometry;
pub mod image;
pub mod naming;
pub mod output;
pub mod overlay;
pub mod sck;

use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{NSEvent, NSScreen};
use objc2_foundation::{NSNumber, NSString};

use self::geometry::Rect;
use self::output::Captured;
use self::sck::{CaptureError, CaptureOptions, Content};
use crate::config::CaptureSettings;

/// The CoreGraphics display id of an `NSScreen`.
pub fn screen_display_id(screen: &NSScreen) -> Option<u32> {
    screen
        .deviceDescription()
        .objectForKey(&NSString::from_str("NSScreenNumber"))
        .and_then(|n| n.downcast::<NSNumber>().ok())
        .map(|n| n.as_u32())
}

/// The screen the mouse pointer is on.
pub fn screen_under_mouse(mtm: MainThreadMarker) -> Option<Retained<NSScreen>> {
    let mouse = NSEvent::mouseLocation();
    let screens = NSScreen::screens(mtm);
    let under = screens.iter().find(|s| {
        let f = s.frame();
        mouse.x >= f.origin.x
            && mouse.x < f.origin.x + f.size.width
            && mouse.y >= f.origin.y
            && mouse.y < f.origin.y + f.size.height
    });
    under.or_else(|| NSScreen::mainScreen(mtm))
}

fn output_scale(settings: &CaptureSettings, native: f64) -> f64 {
    if settings.downscale_retina {
        1.0
    } else {
        native
    }
}

/// Captures the display under the mouse pointer.
pub fn capture_fullscreen(
    mtm: MainThreadMarker,
    settings: &CaptureSettings,
) -> Result<(), CaptureError> {
    let screen = screen_under_mouse(mtm).ok_or(CaptureError::DisplayGone)?;
    let display_id = screen_display_id(&screen).ok_or(CaptureError::DisplayGone)?;
    let content = Content::fetch()?;
    let display = content
        .display(display_id)
        .ok_or(CaptureError::DisplayGone)?;
    let filter = sck::display_filter(
        &display,
        &content.excluded_windows(settings.hide_desktop_icons),
    );
    let scale = output_scale(settings, sck::pixel_scale(&filter));
    let size = sck::content_size(&filter);
    let image = sck::capture(
        &filter,
        &CaptureOptions {
            source_rect: None,
            width: (size.width * scale).round() as usize,
            height: (size.height * scale).round() as usize,
            show_cursor: settings.show_cursor,
            shadow: false,
        },
    )?;
    output::deliver(mtm, Captured { image, scale }, settings);
    Ok(())
}

/// Captures `rect` (display-local points, top-left origin) of a display as
/// it looks now.
pub fn capture_rect(
    mtm: MainThreadMarker,
    display_id: u32,
    rect: Rect,
    settings: &CaptureSettings,
) -> Result<(), CaptureError> {
    let content = Content::fetch()?;
    let display = content
        .display(display_id)
        .ok_or(CaptureError::DisplayGone)?;
    let filter = sck::display_filter(
        &display,
        &content.excluded_windows(settings.hide_desktop_icons),
    );
    let scale = output_scale(settings, sck::pixel_scale(&filter));
    let image = sck::capture(
        &filter,
        &CaptureOptions {
            source_rect: Some(rect),
            width: (rect.width * scale).round() as usize,
            height: (rect.height * scale).round() as usize,
            show_cursor: settings.show_cursor,
            shadow: false,
        },
    )?;
    output::deliver(mtm, Captured { image, scale }, settings);
    Ok(())
}

/// Crops a frozen display snapshot instead of capturing again.
pub fn deliver_from_snapshot(
    mtm: MainThreadMarker,
    snapshot: &objc2_core_graphics::CGImage,
    snapshot_scale: f64,
    rect: Rect,
    settings: &CaptureSettings,
) -> Result<(), CaptureError> {
    let failed = || CaptureError::Failed("could not crop the frozen screen image".into());
    let mut image = image::crop(snapshot, rect, snapshot_scale).ok_or_else(failed)?;
    let scale = output_scale(settings, snapshot_scale);
    if scale != snapshot_scale {
        let width = (rect.width * scale).round() as usize;
        let height = (rect.height * scale).round() as usize;
        image = image::resize(&image, width, height).ok_or_else(failed)?;
    }
    output::deliver(mtm, Captured { image, scale }, settings);
    Ok(())
}

/// Captures one window by id, on a transparent background.
pub fn capture_window(
    mtm: MainThreadMarker,
    window_id: u32,
    settings: &CaptureSettings,
) -> Result<(), CaptureError> {
    let content = Content::fetch()?;
    let window = content.window(window_id).ok_or(CaptureError::WindowGone)?;
    let filter = sck::window_filter(&window);
    let native = sck::pixel_scale(&filter);
    let size = sck::content_size(&filter);
    // ScreenCaptureKit's content size excludes the shadow and, when asked
    // for exactly that size, shrinks window plus shadow to fit. Capture at
    // native resolution, unscaled, onto a larger canvas instead, then trim
    // the empty margin.
    let margin = if settings.window_shadow { 120.0 } else { 0.0 };
    let failed = |what: &str| CaptureError::Failed(format!("the window capture {what}"));
    let mut image = sck::capture(
        &filter,
        &CaptureOptions {
            source_rect: None,
            width: ((size.width + 2.0 * margin) * native).round() as usize,
            height: ((size.height + 2.0 * margin) * native).round() as usize,
            show_cursor: false,
            shadow: settings.window_shadow,
        },
    )?;
    if margin > 0.0 {
        image = image::trim_transparent(&image).ok_or_else(|| failed("was empty"))?;
    }
    let scale = output_scale(settings, native);
    if scale != native {
        let (width, height) = image::pixel_size(&image);
        let factor = scale / native;
        image = image::resize(
            &image,
            (width as f64 * factor).round() as usize,
            (height as f64 * factor).round() as usize,
        )
        .ok_or_else(|| failed("could not be resized"))?;
    }
    output::deliver(mtm, Captured { image, scale }, settings);
    Ok(())
}
