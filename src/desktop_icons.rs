//! Hiding desktop icons and widgets without touching Finder: each screen
//! gets a window showing the wallpaper, just above the icons and below
//! every other window. Nothing is restarted or written to Finder's
//! preferences, and quitting the app shows the icons again.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AllocAnyThread, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSImage, NSPanel, NSScreen, NSView, NSWindowCollectionBehavior,
    NSWindowStyleMask, NSWorkspace,
};
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{CGImage, kCGDesktopIconWindowLevel};
use objc2_quartz_core::kCAGravityResizeAspectFill;

use crate::capture::sck::{self, CaptureOptions, Content};
use crate::capture::screen_display_id;

thread_local! {
    static COVERS: RefCell<Vec<Retained<NSPanel>>> = const { RefCell::new(Vec::new()) };
}

pub fn is_hidden() -> bool {
    COVERS.with(|c| !c.borrow().is_empty())
}

pub fn set_hidden(mtm: MainThreadMarker, hidden: bool) {
    if hidden {
        // Rebuild, so a second "hide" picks up new screens and wallpapers.
        show_icons();
        let content = Content::fetch_all().ok();
        let covers: Vec<_> = NSScreen::screens(mtm)
            .iter()
            .map(|screen| cover(mtm, &screen, content.as_ref()))
            .collect();
        COVERS.with(|c| *c.borrow_mut() = covers);
        tracing::info!("desktop icons hidden");
    } else if is_hidden() {
        show_icons();
        tracing::info!("desktop icons shown");
    }
}

pub fn toggle(mtm: MainThreadMarker) {
    set_hidden(mtm, !is_hidden());
}

fn show_icons() {
    for cover in COVERS.with(|c| std::mem::take(&mut *c.borrow_mut())) {
        cover.orderOut(None);
    }
}

fn cover(mtm: MainThreadMarker, screen: &NSScreen, content: Option<&Content>) -> Retained<NSPanel> {
    let frame = screen.frame();
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        frame,
        NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
        NSBackingStoreType::Buffered,
        false,
    );
    // SAFETY: COVERS keeps a strong reference.
    unsafe { panel.setReleasedWhenClosed(false) };
    // Above the icons (Finder, at the icon level) and widgets (one or two
    // above it), far below normal windows.
    panel.setLevel((kCGDesktopIconWindowLevel + 10) as isize);
    panel.setHasShadow(false);
    panel.setHidesOnDeactivate(false);
    panel.setBackgroundColor(Some(&NSColor::blackColor()));
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );

    let view = NSView::new(mtm);
    view.setWantsLayer(true);
    if let Some(layer) = view.layer() {
        // SAFETY: reading a QuartzCore constant.
        layer.setContentsGravity(unsafe { kCAGravityResizeAspectFill });
        let contents = wallpaper_capture(screen, content).or_else(|| wallpaper_file(screen));
        // SAFETY: CALayer accepts a CGImage or an NSImage as contents.
        unsafe { layer.setContents(contents.as_deref()) };
    }
    panel.setContentView(Some(&view));
    panel.orderFrontRegardless();
    panel
}

/// The wallpaper as currently drawn, including dynamic wallpapers.
fn wallpaper_capture(screen: &NSScreen, content: Option<&Content>) -> Option<Retained<AnyObject>> {
    let content = content?;
    let display = content.display(screen_display_id(screen)?)?;
    let filter = sck::wallpaper_filter(content, &display)?;
    let scale = sck::pixel_scale(&filter);
    let size = sck::content_size(&filter);
    let image: CFRetained<CGImage> = sck::capture(
        &filter,
        &CaptureOptions {
            source_rect: None,
            width: (size.width * scale).round() as usize,
            height: (size.height * scale).round() as usize,
            show_cursor: false,
            shadow: false,
        },
    )
    .map_err(|e| tracing::warn!(error = %e, "could not capture the wallpaper"))
    .ok()?;
    let raw = CFRetained::into_raw(image).cast::<AnyObject>();
    // SAFETY: CoreFoundation types are Objective-C objects; ownership of
    // the +1 reference moves to the Retained.
    unsafe { Retained::from_raw(raw.as_ptr()) }
}

/// The wallpaper image file, for when the screen can't be captured.
fn wallpaper_file(screen: &NSScreen) -> Option<Retained<AnyObject>> {
    let url = NSWorkspace::sharedWorkspace().desktopImageURLForScreen(screen)?;
    let image = NSImage::initWithContentsOfURL(NSImage::alloc(), &url)?;
    // SAFETY: an upcast to AnyObject.
    Some(unsafe { Retained::cast_unchecked(image) })
}
