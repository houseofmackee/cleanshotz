//! Blocking wrappers around ScreenCaptureKit.
//!
//! ScreenCaptureKit reports results on its own dispatch queues, so waiting
//! for them on the main thread cannot deadlock. A capture takes tens of
//! milliseconds, short enough that blocking keeps the call sites simple.

use std::ptr::NonNull;
use std::sync::mpsc;
use std::time::Duration;

use block2::RcBlock;
use objc2::AllocAnyThread;
use objc2::rc::Retained;
use objc2_core_foundation::{CFArray, CFRetained, CGRect, CGSize};
use objc2_core_graphics::{
    CGImage, CGWindowLevelForKey, CGWindowLevelKey, CGWindowListCreate, CGWindowListOption,
};
use objc2_foundation::{NSArray, NSError};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCScreenshotManager, SCShareableContent, SCStreamConfiguration,
    SCWindow,
};

use super::geometry::Rect;

const TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("{0}")]
    Failed(String),
    #[error("ScreenCaptureKit did not respond in time")]
    Timeout,
    #[error("the display is no longer available")]
    DisplayGone,
    #[error("the window is no longer available")]
    WindowGone,
}

/// Moves a value into a ScreenCaptureKit completion handler's thread. The
/// wrapped Objective-C objects are immutable snapshots, safe to hand over.
struct AssertSend<T>(T);
// SAFETY: see the type's documentation.
unsafe impl<T> Send for AssertSend<T> {}

fn error_message(error: *mut NSError) -> String {
    // SAFETY: ScreenCaptureKit passes either nil or a valid NSError.
    match unsafe { error.as_ref() } {
        Some(error) => error.localizedDescription().to_string(),
        None => "unknown ScreenCaptureKit error".to_string(),
    }
}

/// The displays, windows and applications currently available to capture.
pub struct Content {
    inner: Retained<SCShareableContent>,
}

#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub id: u32,
    /// Global frame in points, top-left origin (CoreGraphics space).
    pub frame: Rect,
    pub title: String,
    pub app_name: String,
}

impl Content {
    pub fn fetch() -> Result<Content, CaptureError> {
        let (tx, rx) = mpsc::sync_channel(1);
        let handler = RcBlock::new(
            move |content: *mut SCShareableContent, error: *mut NSError| {
                // SAFETY: on success `content` is a valid object; retain it
                // because the handler only borrows it.
                let result = match unsafe { Retained::retain(content) } {
                    Some(content) => Ok(content),
                    None => Err(CaptureError::Failed(error_message(error))),
                };
                let _ = tx.send(AssertSend(result));
            },
        );
        // SAFETY: the handler has the expected signature.
        unsafe {
            SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
                false, true, &handler,
            );
        }
        let inner = rx
            .recv_timeout(TIMEOUT)
            .map_err(|_| CaptureError::Timeout)?
            .0?;
        Ok(Content { inner })
    }

    pub fn display(&self, id: u32) -> Option<Retained<SCDisplay>> {
        // SAFETY: plain property reads.
        unsafe { self.inner.displays().iter().find(|d| d.displayID() == id) }
    }

    pub fn window(&self, id: u32) -> Option<Retained<SCWindow>> {
        // SAFETY: plain property reads.
        unsafe { self.inner.windows().iter().find(|w| w.windowID() == id) }
    }

    /// Windows to leave out of display captures: our own (overlays, HUDs)
    /// and, if requested, desktop icons and widgets.
    pub fn excluded_windows(&self, hide_desktop_icons: bool) -> Retained<NSArray<SCWindow>> {
        let own_pid = std::process::id() as i32;
        let wallpaper_level = CGWindowLevelForKey(CGWindowLevelKey::DesktopWindowLevelKey) as isize;
        let normal_level = CGWindowLevelForKey(CGWindowLevelKey::NormalWindowLevelKey) as isize;
        // SAFETY: plain property reads.
        let excluded: Vec<Retained<SCWindow>> = unsafe {
            self.inner
                .windows()
                .iter()
                .filter(|w| {
                    let Some(app) = w.owningApplication() else {
                        return false;
                    };
                    if app.processID() == own_pid {
                        return true;
                    }
                    // Desktop icons (Finder) and widgets (Notification
                    // Center) sit between the wallpaper and normal windows.
                    let layer = w.windowLayer();
                    let on_desktop = layer > wallpaper_level && layer < normal_level;
                    let bundle = app.bundleIdentifier().to_string();
                    hide_desktop_icons
                        && on_desktop
                        && (bundle == "com.apple.finder"
                            || bundle == "com.apple.notificationcenterui")
                })
                .collect()
        };
        NSArray::from_retained_slice(&excluded)
    }

    /// Normal application windows that can be picked in window mode,
    /// front to back.
    pub fn pickable_windows(&self) -> Vec<WindowInfo> {
        let own_pid = std::process::id() as i32;
        let order = window_z_order();
        // SAFETY: plain property reads.
        let mut windows: Vec<(usize, WindowInfo)> = unsafe {
            self.inner
                .windows()
                .iter()
                .filter(|w| w.isOnScreen() && w.windowLayer() == 0)
                .filter_map(|w| {
                    let app = w.owningApplication()?;
                    if app.processID() == own_pid {
                        return None;
                    }
                    let f = w.frame();
                    if f.size.width < 40.0 || f.size.height < 40.0 {
                        return None;
                    }
                    let id = w.windowID();
                    let z = order.iter().position(|o| *o == id)?;
                    Some((
                        z,
                        WindowInfo {
                            id,
                            frame: Rect::new(f.origin.x, f.origin.y, f.size.width, f.size.height),
                            title: w.title().map(|t| t.to_string()).unwrap_or_default(),
                            app_name: app.applicationName().to_string(),
                        },
                    ))
                })
                .collect()
        };
        windows.sort_by_key(|(z, _)| *z);
        windows.into_iter().map(|(_, w)| w).collect()
    }
}

/// On-screen window ids, front to back. ScreenCaptureKit does not document
/// an order for its window list, so this comes from the window server.
fn window_z_order() -> Vec<u32> {
    let Some(list) = CGWindowListCreate(CGWindowListOption::OptionOnScreenOnly, 0) else {
        return Vec::new();
    };
    let list: &CFArray = &list;
    (0..list.count())
        // SAFETY: CGWindowListCreate returns an array of CGWindowID values
        // stored directly in the pointer-sized slots.
        .map(|i| unsafe { list.value_at_index(i) } as usize as u32)
        .collect()
}

/// A filter for a whole display, minus `excluded` windows.
pub fn display_filter(
    display: &SCDisplay,
    excluded: &NSArray<SCWindow>,
) -> Retained<SCContentFilter> {
    // SAFETY: valid display and window array.
    unsafe {
        SCContentFilter::initWithDisplay_excludingWindows(
            SCContentFilter::alloc(),
            display,
            excluded,
        )
    }
}

/// A filter for a single window, independent of what covers it.
pub fn window_filter(window: &SCWindow) -> Retained<SCContentFilter> {
    // SAFETY: valid window.
    unsafe { SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), window) }
}

/// Points-to-pixels scale of the filter's content (2.0 on Retina displays).
pub fn pixel_scale(filter: &SCContentFilter) -> f64 {
    // SAFETY: plain property read (macOS 14+).
    f64::from(unsafe { filter.pointPixelScale() })
}

/// The filter's content size in points (for window filters this includes
/// the shadow when shadows are captured).
pub fn content_size(filter: &SCContentFilter) -> CGSize {
    // SAFETY: plain property read (macOS 14+).
    unsafe { filter.contentRect() }.size
}

pub struct CaptureOptions {
    /// Region in display-local points (top-left origin); `None` for all.
    pub source_rect: Option<Rect>,
    /// Output size in pixels.
    pub width: usize,
    pub height: usize,
    pub show_cursor: bool,
    /// Window captures only: include the window's shadow.
    pub shadow: bool,
}

pub fn capture(
    filter: &SCContentFilter,
    options: &CaptureOptions,
) -> Result<CFRetained<CGImage>, CaptureError> {
    // SAFETY: setting plain configuration properties.
    let config = unsafe {
        let config = SCStreamConfiguration::new();
        config.setWidth(options.width);
        config.setHeight(options.height);
        config.setShowsCursor(options.show_cursor);
        config.setIgnoreShadowsSingleWindow(!options.shadow);
        config.setShouldBeOpaque(false);
        config.setScalesToFit(false);
        if let Some(r) = options.source_rect {
            config.setSourceRect(CGRect::new(
                objc2_core_foundation::CGPoint::new(r.x, r.y),
                CGSize::new(r.width, r.height),
            ));
        }
        config
    };

    let (tx, rx) = mpsc::sync_channel(1);
    let handler = RcBlock::new(move |image: *mut CGImage, error: *mut NSError| {
        let result = match NonNull::new(image) {
            // SAFETY: a valid image that the handler only borrows; retain it.
            Some(image) => Ok(unsafe { CFRetained::retain(image) }),
            None => Err(CaptureError::Failed(error_message(error))),
        };
        let _ = tx.send(result);
    });
    // SAFETY: valid filter and configuration; the handler has the expected signature.
    unsafe {
        SCScreenshotManager::captureImageWithFilter_configuration_completionHandler(
            filter,
            &config,
            Some(&handler),
        );
    }
    rx.recv_timeout(TIMEOUT)
        .map_err(|_| CaptureError::Timeout)?
}
