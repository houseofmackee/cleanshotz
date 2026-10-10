//! The Quick Access Overlay: a stack of floating thumbnails in a screen
//! corner, one per recent capture.
//!
//! Each card is a non-activating panel, so using it never takes focus away
//! from the app you are working in. Hovering a card shows its actions:
//! Copy, Save (or Show in Finder once saved), Pin and Close. A card can also
//! be dragged into another app as a file, or double-clicked to open it.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::ptr::NonNull;
use std::rc::Rc;

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{
    AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSDragOperation, NSDraggingContext, NSDraggingItem,
    NSDraggingSession, NSDraggingSource, NSEvent, NSGraphicsContext, NSImage, NSImageInterpolation,
    NSPanel, NSResponder, NSScreen, NSStatusWindowLevel, NSTrackingArea, NSTrackingAreaOptions,
    NSView, NSWindowCollectionBehavior, NSWindowStyleMask, NSWorkspace,
};
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::CGImage;
use objc2_foundation::{
    NSArray, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSTimer, NSURL,
};

use crate::capture::geometry::{Point, Rect};
use crate::capture::output::{self, Delivered};
use crate::capture::{image, screen_under_mouse};
use crate::config::{CaptureSettings, ImageFormat, QuickAccessSettings, Side};
use crate::pin;
use crate::ui::draw::{self, Weight};

const MARGIN: f64 = 16.0;
const SPACING: f64 = 12.0;
const MAX_CARD: (f64, f64) = (240.0, 160.0);
const MIN_CARD: (f64, f64) = (150.0, 96.0);
const RADIUS: f64 = 10.0;
const RECENTLY_CLOSED_LIMIT: usize = 10;

/// A capture shown (or recently shown) in Quick Access.
pub struct Item {
    image: RefCell<CFRetained<CGImage>>,
    pub scale: f64,
    pub source: Option<Rect>,
    file: RefCell<PathBuf>,
    saved: Cell<bool>,
    png: RefCell<Option<Vec<u8>>>,
    /// For recordings: the duration in seconds. `image` is the thumbnail.
    video: Option<f64>,
}

impl Item {
    pub fn new(
        image: CFRetained<CGImage>,
        scale: f64,
        source: Option<Rect>,
        file: PathBuf,
        saved: bool,
    ) -> Self {
        Self {
            image: RefCell::new(image),
            scale,
            source,
            file: RefCell::new(file),
            saved: Cell::new(saved),
            png: RefCell::new(None),
            video: None,
        }
    }

    pub fn is_video(&self) -> bool {
        self.video.is_some()
    }

    pub fn image(&self) -> CFRetained<CGImage> {
        self.image.borrow().clone()
    }

    pub fn file(&self) -> PathBuf {
        self.file.borrow().clone()
    }

    pub fn is_saved(&self) -> bool {
        self.saved.get()
    }

    /// PNG bytes, encoded once on first use.
    pub fn png(&self) -> Option<Vec<u8>> {
        let mut png = self.png.borrow_mut();
        if png.is_none() {
            *png = image::encode(&self.image.borrow(), ImageFormat::Png, 1.0);
        }
        png.clone()
    }

    /// Size in points.
    pub fn point_size(&self) -> NSSize {
        let (w, h) = image::pixel_size(&self.image.borrow());
        NSSize::new(w as f64 / self.scale, h as f64 / self.scale)
    }

    pub fn ns_image(&self) -> Retained<NSImage> {
        NSImage::initWithCGImage_size(NSImage::alloc(), &self.image.borrow(), self.point_size())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Button {
    Close,
    Annotate,
    Pin,
    Copy,
    Save,
}

struct Card {
    id: u64,
    item: Rc<Item>,
    panel: Retained<NSPanel>,
    view: Retained<CardView>,
    thumbnail: Retained<NSImage>,
    size: NSSize,
    timer: Option<Retained<NSTimer>>,
}

#[derive(Default)]
struct Shelf {
    settings: QuickAccessSettings,
    capture: CaptureSettings,
    /// Oldest first; the newest card sits at the bottom of the stack.
    cards: Vec<Card>,
    closed: Vec<Rc<Item>>,
    next_id: u64,
    screen: Option<Retained<NSScreen>>,
}

thread_local! {
    static SHELF: RefCell<Shelf> = RefCell::new(Shelf::default());
    /// Panels of closed cards, released on the next change. AppKit may
    /// still be delivering an event to a panel when its card closes.
    static RETIRED: RefCell<Vec<Retained<NSPanel>>> = const { RefCell::new(Vec::new()) };
}

pub fn configure(settings: QuickAccessSettings, capture: CaptureSettings) {
    SHELF.with(|s| {
        let mut shelf = s.borrow_mut();
        shelf.settings = settings;
        shelf.capture = capture;
    });
}

/// Shows a new capture. Called on the main thread once saving and copying
/// have finished.
pub fn add(mtm: MainThreadMarker, delivered: Delivered) {
    let item = Rc::new(Item {
        image: RefCell::new(delivered.image),
        scale: delivered.scale,
        source: delivered.source,
        file: RefCell::new(delivered.file),
        saved: Cell::new(delivered.saved),
        png: RefCell::new(delivered.png),
        video: None,
    });
    if !SHELF.with(|s| s.borrow().settings.enabled) {
        return;
    }
    show(mtm, item);
}

/// Shows a finished recording. `thumbnail` is a frame of it.
pub fn add_video(
    mtm: MainThreadMarker,
    file: PathBuf,
    thumbnail: CFRetained<CGImage>,
    scale: f64,
    duration: f64,
    saved: bool,
) {
    let item = Rc::new(Item {
        image: RefCell::new(thumbnail),
        scale,
        source: None,
        file: RefCell::new(file),
        saved: Cell::new(saved),
        png: RefCell::new(None),
        video: Some(duration),
    });
    if !SHELF.with(|s| s.borrow().settings.enabled) {
        return;
    }
    show(mtm, item);
}

/// Puts a file on the clipboard (pasting it in Finder, Mail or chat apps
/// attaches the file).
pub fn copy_file(path: &std::path::Path) {
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    let pasteboard = objc2_app_kit::NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    let object: Retained<ProtocolObject<dyn objc2_app_kit::NSPasteboardWriting>> =
        ProtocolObject::from_retained(url);
    pasteboard.writeObjects(&NSArray::from_retained_slice(&[object]));
}

/// `m:ss`, or `h:mm:ss` from an hour on.
pub fn format_duration(seconds: f64) -> String {
    let total = seconds.round().max(0.0) as u64;
    let (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

pub fn has_recently_closed() -> bool {
    SHELF.with(|s| !s.borrow().closed.is_empty())
}

/// Shows the most recently closed capture again.
pub fn restore_recently_closed(mtm: MainThreadMarker) {
    if let Some(item) = SHELF.with(|s| s.borrow_mut().closed.pop()) {
        show(mtm, item);
    }
}

fn show(mtm: MainThreadMarker, item: Rc<Item>) {
    RETIRED.with(|r| r.borrow_mut().clear());
    let size = card_size(item.point_size());
    let id = SHELF.with(|s| {
        let mut shelf = s.borrow_mut();
        shelf.next_id += 1;
        if shelf.cards.is_empty() {
            shelf.screen = screen_under_mouse(mtm);
        }
        shelf.next_id
    });

    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        NSRect::new(NSPoint::ZERO, size),
        NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
        NSBackingStoreType::Buffered,
        false,
    );
    // SAFETY: the shelf keeps a strong reference to the panel.
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setLevel(NSStatusWindowLevel);
    panel.setOpaque(false);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    panel.setHasShadow(true);
    panel.setHidesOnDeactivate(false);
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
    let view = CardView::new(mtm, id, NSRect::new(NSPoint::ZERO, size));
    panel.setContentView(Some(&view));
    // SAFETY: the view owns the tracking area.
    let tracking = unsafe {
        NSTrackingArea::initWithRect_options_owner_userInfo(
            NSTrackingArea::alloc(),
            NSRect::ZERO,
            NSTrackingAreaOptions::MouseEnteredAndExited
                | NSTrackingAreaOptions::ActiveAlways
                | NSTrackingAreaOptions::InVisibleRect,
            Some(&view),
            None,
        )
    };
    view.addTrackingArea(&tracking);

    let thumbnail = item.ns_image();
    let timer = auto_close_timer(id);
    SHELF.with(|s| {
        s.borrow_mut().cards.push(Card {
            id,
            item,
            panel: panel.clone(),
            view,
            thumbnail,
            size,
            timer,
        })
    });
    layout();
    panel.orderFrontRegardless();
}

fn auto_close_timer(id: u64) -> Option<Retained<NSTimer>> {
    let seconds = SHELF.with(|s| s.borrow().settings.auto_close_seconds);
    if seconds == 0 {
        return None;
    }
    let block = RcBlock::new(move |_timer: NonNull<NSTimer>| {
        // Wait while the pointer is over the card.
        let hovered = SHELF.with(|s| {
            s.borrow()
                .cards
                .iter()
                .find(|c| c.id == id)
                .is_some_and(|c| c.view.ivars().hover.get())
        });
        if !hovered {
            defer(move |_| close(id, true));
        }
    });
    // SAFETY: the block has the expected signature; the run loop owns the
    // timer until it is invalidated when the card closes.
    Some(unsafe {
        NSTimer::scheduledTimerWithTimeInterval_repeats_block(f64::from(seconds), true, &block)
    })
}

/// Stacks the cards bottom-up in the configured corner.
fn layout() {
    SHELF.with(|s| {
        let mut shelf = s.borrow_mut();
        let Some(screen) = shelf.screen.clone() else {
            return;
        };
        let visible = screen.visibleFrame();
        // Drop the oldest cards that no longer fit.
        loop {
            let total: f64 = shelf.cards.iter().map(|c| c.size.height + SPACING).sum();
            if total + MARGIN <= visible.size.height || shelf.cards.len() <= 1 {
                break;
            }
            let oldest = shelf.cards.remove(0);
            retire(&mut shelf, oldest, true);
        }
        let side = shelf.settings.position;
        let mut y = visible.origin.y + MARGIN;
        for card in shelf.cards.iter().rev() {
            let x = match side {
                Side::Left => visible.origin.x + MARGIN,
                Side::Right => visible.origin.x + visible.size.width - MARGIN - card.size.width,
            };
            card.panel
                .setFrame_display(NSRect::new(NSPoint::new(x, y), card.size), true);
            y += card.size.height + SPACING;
        }
    });
}

fn close(id: u64, remember: bool) {
    SHELF.with(|s| {
        let mut shelf = s.borrow_mut();
        if let Some(index) = shelf.cards.iter().position(|c| c.id == id) {
            let card = shelf.cards.remove(index);
            retire(&mut shelf, card, remember);
        }
    });
    layout();
}

fn retire(shelf: &mut Shelf, card: Card, remember: bool) {
    if let Some(timer) = &card.timer {
        timer.invalidate();
    }
    card.panel.orderOut(None);
    if remember {
        shelf.closed.push(card.item);
        let excess = shelf.closed.len().saturating_sub(RECENTLY_CLOSED_LIMIT);
        shelf.closed.drain(..excess);
    }
    RETIRED.with(|r| r.borrow_mut().push(card.panel));
}

/// Runs `f` after the current event has been handled, so actions never
/// close a panel from inside its own event handler.
fn defer(f: impl FnOnce(MainThreadMarker) + Send + 'static) {
    DispatchQueue::main().exec_async(move || {
        f(MainThreadMarker::new().expect("the main queue runs on the main thread"));
    });
}

fn item_for(id: u64) -> Option<Rc<Item>> {
    SHELF.with(|s| {
        s.borrow()
            .cards
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.item.clone())
    })
}

fn perform(mtm: MainThreadMarker, id: u64, button: Button) {
    let Some(item) = item_for(id) else {
        return;
    };
    match button {
        Button::Close => close(id, true),
        Button::Copy if item.is_video() => {
            copy_file(&item.file());
            close(id, true);
        }
        Button::Copy => {
            match item.png() {
                Some(png) => output::copy_png(&png),
                None => tracing::error!("could not encode capture for the clipboard"),
            }
            close(id, true);
        }
        Button::Save if item.is_saved() => {
            let url = NSURL::fileURLWithPath(&NSString::from_str(&item.file().to_string_lossy()));
            NSWorkspace::sharedWorkspace()
                .activateFileViewerSelectingURLs(&NSArray::from_retained_slice(&[url]));
        }
        Button::Save if item.is_video() => {
            let settings = SHELF.with(|s| s.borrow().capture.clone());
            match save_video_copy(&item.file(), &settings) {
                Ok(path) => {
                    tracing::info!(path = %path.display(), "recording saved from Quick Access");
                    *item.file.borrow_mut() = path;
                    item.saved.set(true);
                    close(id, true);
                }
                Err(e) => tracing::error!(error = %e, "could not save recording"),
            }
        }
        Button::Save => {
            let settings = SHELF.with(|s| s.borrow().capture.clone());
            // Keep the capture-time name the temporary file already has.
            let file = item.file();
            let stem = file.file_stem().map(|s| s.to_string_lossy().into_owned());
            match output::save_to_directory(&item.image(), item.scale, &settings, stem.as_deref()) {
                Ok((path, _)) => {
                    tracing::info!(path = %path.display(), "capture saved from Quick Access");
                    *item.file.borrow_mut() = path;
                    item.saved.set(true);
                    close(id, true);
                }
                Err(e) => tracing::error!(error = %e, "could not save capture"),
            }
        }
        Button::Pin => {
            pin::pin(mtm, &item);
            close(id, true);
        }
        Button::Annotate => crate::editor::open_capture(mtm, &item),
    }
}

/// Double-click: annotate an image, play a recording.
fn annotate(mtm: MainThreadMarker, id: u64) {
    let Some(item) = item_for(id) else {
        return;
    };
    if item.is_video() {
        let url = NSURL::fileURLWithPath(&NSString::from_str(&item.file().to_string_lossy()));
        NSWorkspace::sharedWorkspace().openURL(&url);
    } else {
        crate::editor::open_capture(mtm, &item);
    }
}

/// Copies a temporary recording into the save folder, keeping its name.
fn save_video_copy(file: &std::path::Path, settings: &CaptureSettings) -> Result<PathBuf, String> {
    let dir = crate::capture::naming::expand_home(&settings.save_directory);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let stem = file
        .file_stem()
        .map_or_else(|| "Recording".into(), |s| s.to_string_lossy().into_owned());
    let ext = file
        .extension()
        .map_or_else(|| "mp4".into(), |e| e.to_string_lossy().into_owned());
    let path = crate::capture::naming::unique_path(&dir, &stem, &ext);
    std::fs::copy(file, &path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// The capture settings Quick Access was configured with.
pub fn capture_settings() -> CaptureSettings {
    SHELF.with(|s| s.borrow().capture.clone())
}

/// After `file` was edited and saved: shows `image` in the cards for it.
pub fn refresh(file: &std::path::Path, image: CFRetained<CGImage>) {
    SHELF.with(|s| {
        let mut shelf = s.borrow_mut();
        for card in shelf.cards.iter_mut().filter(|c| c.item.file() == file) {
            *card.item.image.borrow_mut() = image.clone();
            *card.item.png.borrow_mut() = None;
            card.item.saved.set(true);
            card.thumbnail = card.item.ns_image();
            card.size = card_size(card.item.point_size());
            card.view.setNeedsDisplay(true);
        }
    });
    layout();
}

/// Card size in points for an image of `image` points: the image scaled
/// to fit the maximum card, but never smaller than the minimum card (so the
/// buttons fit). The image is letterboxed when the card is wider or taller.
fn card_size(image: NSSize) -> NSSize {
    let (max_w, max_h) = MAX_CARD;
    let scale = (max_w / image.width).min(max_h / image.height).min(1.0);
    let (min_w, min_h) = MIN_CARD;
    NSSize::new(
        (image.width * scale).max(min_w).round(),
        (image.height * scale).max(min_h).round(),
    )
}

/// The image's rect inside a card of `bounds`, aspect-fit and centered.
fn fit(image: NSSize, bounds: Rect) -> Rect {
    let scale = (bounds.width / image.width).min(bounds.height / image.height);
    let (w, h) = (image.width * scale, image.height * scale);
    Rect::new(
        bounds.x + (bounds.width - w) / 2.0,
        bounds.y + (bounds.height - h) / 2.0,
        w,
        h,
    )
}

fn button_rects(bounds: Rect, saved: bool, video: bool) -> Vec<(Button, Rect, &'static str)> {
    let corner = 24.0;
    let pill_w = (bounds.width - 32.0).min(120.0);
    let pill_h = 28.0;
    let cx = bounds.x + (bounds.width - pill_w) / 2.0;
    let cy = bounds.y + bounds.height / 2.0;
    let buttons = vec![
        (Button::Close, Rect::new(8.0, 8.0, corner, corner), "xmark"),
        (
            Button::Pin,
            Rect::new(bounds.max_x() - 8.0 - corner, 8.0, corner, corner),
            "pin.fill",
        ),
        (
            Button::Annotate,
            Rect::new(bounds.max_x() - 14.0 - 2.0 * corner, 8.0, corner, corner),
            "pencil",
        ),
        (
            Button::Copy,
            Rect::new(cx, cy - pill_h - 3.0, pill_w, pill_h),
            "Copy",
        ),
        (
            Button::Save,
            Rect::new(cx, cy + 3.0, pill_w, pill_h),
            if saved { "Show in Finder" } else { "Save" },
        ),
    ];
    // Recordings cannot be annotated or pinned.
    buttons
        .into_iter()
        .filter(|(b, _, _)| !video || !matches!(b, Button::Annotate | Button::Pin))
        .collect()
}

pub struct CardIvars {
    id: u64,
    hover: Cell<bool>,
    pressed: Cell<Option<Button>>,
    down_at: Cell<Option<Point>>,
    dragging: Cell<bool>,
}

define_class!(
    // SAFETY: NSView can be subclassed; no Drop implementation.
    #[unsafe(super(NSView, NSResponder, objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = CardIvars]
    struct CardView;

    unsafe impl NSObjectProtocol for CardView {}

    unsafe impl NSDraggingSource for CardView {
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn source_operation_mask(
            &self,
            _session: &NSDraggingSession,
            _context: NSDraggingContext,
        ) -> NSDragOperation {
            NSDragOperation::Copy
        }

        #[unsafe(method(draggingSession:endedAtPoint:operation:))]
        fn dragging_ended(
            &self,
            _session: &NSDraggingSession,
            _point: NSPoint,
            operation: NSDragOperation,
        ) {
            self.ivars().dragging.set(false);
            let close_after = SHELF.with(|s| s.borrow().settings.close_after_drag);
            if operation != NSDragOperation::None && close_after {
                let id = self.ivars().id;
                defer(move |_| close(id, true));
            }
        }
    }

    impl CardView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            self.draw();
        }

        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _event: &NSEvent) {
            self.ivars().hover.set(true);
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            self.ivars().hover.set(false);
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let p = self.point(event);
            let ivars = self.ivars();
            ivars.down_at.set(Some(p));
            ivars.pressed.set(self.button_at(p));
            ivars.dragging.set(false);
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let ivars = self.ivars();
            let (Some(start), None, false) =
                (ivars.down_at.get(), ivars.pressed.get(), ivars.dragging.get())
            else {
                return;
            };
            let p = self.point(event);
            if (p.x - start.x).hypot(p.y - start.y) > 4.0 {
                ivars.dragging.set(true);
                self.begin_file_drag(event);
            }
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            let ivars = self.ivars();
            let pressed = ivars.pressed.take();
            ivars.down_at.set(None);
            self.setNeedsDisplay(true);
            if ivars.dragging.get() {
                return;
            }
            let id = ivars.id;
            match pressed {
                Some(button) if self.button_at(self.point(event)) == Some(button) => {
                    defer(move |mtm| perform(mtm, id, button));
                }
                None if event.clickCount() == 2 => defer(move |mtm| annotate(mtm, id)),
                _ => {}
            }
        }
    }
);

impl CardView {
    fn new(mtm: MainThreadMarker, id: u64, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(CardIvars {
            id,
            hover: Cell::new(false),
            pressed: Cell::new(None),
            down_at: Cell::new(None),
            dragging: Cell::new(false),
        });
        // SAFETY: NSView's designated initializer.
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    fn point(&self, event: &NSEvent) -> Point {
        let p = self.convertPoint_fromView(event.locationInWindow(), None);
        Point::new(p.x, p.y)
    }

    fn bounds_rect(&self) -> Rect {
        let b = self.bounds();
        Rect::new(0.0, 0.0, b.size.width, b.size.height)
    }

    fn button_at(&self, p: Point) -> Option<Button> {
        let item = item_for(self.ivars().id);
        let saved = item.as_ref().is_some_and(|i| i.is_saved());
        let video = item.as_ref().is_some_and(|i| i.is_video());
        button_rects(self.bounds_rect(), saved, video)
            .into_iter()
            .find(|(_, r, _)| r.contains(p))
            .map(|(b, _, _)| b)
    }

    fn begin_file_drag(&self, event: &NSEvent) {
        let id = self.ivars().id;
        let Some((file, thumbnail)) = SHELF.with(|s| {
            s.borrow()
                .cards
                .iter()
                .find(|c| c.id == id)
                .map(|c| (c.item.file(), c.thumbnail.clone()))
        }) else {
            return;
        };
        tracing::debug!(file = %file.display(), "starting file drag");
        let url = NSURL::fileURLWithPath(&NSString::from_str(&file.to_string_lossy()));
        let item = NSDraggingItem::initWithPasteboardWriter(
            NSDraggingItem::alloc(),
            ProtocolObject::from_ref(&*url),
        );
        let contents: &AnyObject = thumbnail.as_ref();
        // SAFETY: an NSImage is valid dragging contents.
        unsafe { item.setDraggingFrame_contents(self.bounds(), Some(contents)) };
        self.beginDraggingSessionWithItems_event_source(
            &NSArray::from_retained_slice(&[item]),
            event,
            ProtocolObject::from_ref(self),
        );
    }

    fn draw(&self) {
        let ivars = self.ivars();
        let Some((item, thumbnail)) = SHELF.with(|s| {
            s.borrow()
                .cards
                .iter()
                .find(|c| c.id == ivars.id)
                .map(|c| (c.item.clone(), c.thumbnail.clone()))
        }) else {
            return;
        };
        let bounds = self.bounds_rect();
        let Some(context) = NSGraphicsContext::currentContext() else {
            return;
        };
        context.saveGraphicsState();
        objc2_app_kit::NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
            draw::ns_rect(bounds),
            RADIUS,
            RADIUS,
        )
        .addClip();
        draw::fill_rect(bounds, &NSColor::colorWithWhite_alpha(0.12, 1.0));
        context.setImageInterpolation(NSImageInterpolation::High);
        // SAFETY: drawing inside drawRect:.
        unsafe {
            thumbnail.drawInRect_fromRect_operation_fraction_respectFlipped_hints(
                draw::ns_rect(fit(item.point_size(), bounds)),
                NSRect::ZERO,
                objc2_app_kit::NSCompositingOperation::SourceOver,
                1.0,
                true,
                None,
            );
        }

        if let Some(duration) = item.video {
            let label = format!("▶ {}", format_duration(duration));
            let size = draw::text_size(&label, 11.0, Weight::Semibold);
            let badge = Rect::new(
                bounds.max_x() - size.width - 20.0,
                bounds.max_y() - size.height - 14.0,
                size.width + 12.0,
                size.height + 6.0,
            );
            draw::fill_rounded(badge, 5.0, &NSColor::colorWithWhite_alpha(0.0, 0.7));
            draw::draw_text(
                &label,
                Point::new(badge.x + 6.0, badge.y + 3.0),
                11.0,
                Weight::Semibold,
                &NSColor::whiteColor(),
            );
        }

        if ivars.hover.get() {
            draw::fill_rect(bounds, &NSColor::colorWithWhite_alpha(0.0, 0.45));
            let pressed = ivars.pressed.get();
            for (button, rect, label) in button_rects(bounds, item.is_saved(), item.is_video()) {
                let down = pressed == Some(button);
                match button {
                    Button::Close | Button::Pin | Button::Annotate => {
                        let alpha = if down { 0.85 } else { 0.6 };
                        draw::fill_rounded(
                            rect,
                            rect.width / 2.0,
                            &NSColor::colorWithWhite_alpha(0.0, alpha),
                        );
                        draw::draw_symbol(label, rect, 11.0, &NSColor::whiteColor());
                    }
                    Button::Copy | Button::Save => {
                        let alpha = if down { 0.35 } else { 0.2 };
                        draw::fill_rounded(rect, 7.0, &NSColor::colorWithWhite_alpha(1.0, alpha));
                        draw::draw_text_centered(
                            label,
                            rect,
                            13.0,
                            Weight::Semibold,
                            &NSColor::whiteColor(),
                        );
                    }
                }
            }
        }
        context.restoreGraphicsState();
        draw::stroke_rounded(
            draw::inset(bounds, 0.5),
            RADIUS,
            1.0,
            &NSColor::colorWithWhite_alpha(1.0, 0.18),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cards_fit_within_limits() {
        let wide = card_size(NSSize::new(1600.0, 900.0));
        assert_eq!((wide.width, wide.height), (240.0, 135.0));
        let tall = card_size(NSSize::new(300.0, 1200.0));
        assert_eq!((tall.width, tall.height), (150.0, 160.0));
        let small = card_size(NSSize::new(50.0, 20.0));
        assert_eq!((small.width, small.height), (150.0, 96.0));
    }

    #[test]
    fn formats_durations() {
        assert_eq!(format_duration(4.4), "0:04");
        assert_eq!(format_duration(75.0), "1:15");
        assert_eq!(format_duration(3725.0), "1:02:05");
    }

    #[test]
    fn fit_letterboxes() {
        let r = fit(NSSize::new(100.0, 100.0), Rect::new(0.0, 0.0, 200.0, 100.0));
        assert_eq!(r, Rect::new(50.0, 0.0, 100.0, 100.0));
    }
}
