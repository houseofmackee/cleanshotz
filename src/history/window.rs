//! The History window: a searchable grid of past captures and recordings.
//! Captures can be rated (0 to 5 stars; the digit keys), tagged and given
//! a note; `#tag` in the search keeps only captures with that tag.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::path::Path;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, ProtocolObject};
use objc2::{
    AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel,
};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSBackingStoreType, NSColor, NSCompositingOperation, NSEvent,
    NSEventModifierFlags, NSGraphicsContext, NSImage, NSImageInterpolation, NSMenu, NSMenuItem,
    NSPopUpButton, NSResponder, NSScrollView, NSSearchField, NSSegmentedControl, NSTextField,
    NSView, NSWindow, NSWindowDelegate, NSWindowStyleMask, NSWorkspace,
};
use objc2_foundation::{
    NSArray, NSData, NSNotification, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSURL,
    ns_string,
};

use super::store::{Entry, Kind, Show};
use crate::capture::geometry::{Point, Rect};
use crate::config::ImageFormat;
use crate::ui::draw::{self, Weight};

const CELL: NSSize = NSSize {
    width: 216.0,
    height: 200.0,
};
const THUMB: NSSize = NSSize {
    width: 196.0,
    height: 132.0,
};
const PADDING: f64 = 12.0;
const TOOLBAR: f64 = 48.0;
const LIMIT: usize = 1000;

mod tag {
    pub const OPEN: isize = 1;
    pub const EDIT: isize = 2;
    pub const COPY: isize = 3;
    pub const SHOW_IN_FINDER: isize = 4;
    pub const PIN: isize = 5;
    pub const DELETE: isize = 6;
    pub const UPLOAD: isize = 7;
    pub const COPY_LINK: isize = 8;
    pub const PRINT: isize = 9;
    pub const TAGS: isize = 10;
    /// `RATE + n` sets an n-star rating (0 removes it).
    pub const RATE: isize = 20;
}

struct Item {
    entry: Entry,
    exists: bool,
}

pub struct Ivars {
    window: OnceCell<Retained<NSWindow>>,
    grid: OnceCell<Retained<HistoryGrid>>,
    scroll: OnceCell<Retained<NSScrollView>>,
    search: OnceCell<Retained<NSSearchField>>,
    count: OnceCell<Retained<NSTextField>>,
    /// Lists the tags in use; choosing one searches for it.
    tags: OnceCell<Retained<NSPopUpButton>>,
    tag_names: RefCell<Vec<String>>,
    items: RefCell<Vec<Item>>,
    thumbnails: RefCell<HashMap<i64, Retained<NSImage>>>,
    selected: Cell<Option<usize>>,
    show: Cell<Show>,
    /// One accessibility element per item, for VoiceOver.
    elements: RefCell<Vec<Retained<objc2_app_kit::NSAccessibilityElement>>>,
}

thread_local! {
    static WINDOW: RefCell<Option<Retained<HistoryWindow>>> = const { RefCell::new(None) };
}

/// Shows the History window, creating it the first time.
pub fn open(mtm: MainThreadMarker) {
    let existing = WINDOW.with(|w| w.borrow().clone());
    let controller = existing.unwrap_or_else(|| {
        let c = HistoryWindow::new(mtm);
        WINDOW.with(|w| *w.borrow_mut() = Some(c.clone()));
        c
    });
    controller.reload();
    if let Some(window) = controller.ivars().window.get() {
        crate::ui::bring_forward(mtm, window);
    }
}

pub fn reload_if_open(_mtm: MainThreadMarker) {
    if let Some(c) = WINDOW.with(|w| w.borrow().clone())
        && c.ivars().window.get().is_some_and(|w| w.isVisible())
    {
        c.reload();
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    struct HistoryWindow;

    unsafe impl NSObjectProtocol for HistoryWindow {}

    unsafe impl NSWindowDelegate for HistoryWindow {
        #[unsafe(method(windowDidResize:))]
        fn window_did_resize(&self, _notification: &NSNotification) {
            self.relayout();
        }
    }

    impl HistoryWindow {
        #[unsafe(method(searchChanged:))]
        fn search_changed(&self, _sender: &AnyObject) {
            self.reload();
        }

        #[unsafe(method(showChanged:))]
        fn show_changed(&self, sender: &NSSegmentedControl) {
            self.ivars().show.set(match sender.selectedSegment() {
                1 => Show::Screenshots,
                2 => Show::Recordings,
                3 => Show::Starred,
                _ => Show::All,
            });
            self.reload();
        }

        #[unsafe(method(tagChosen:))]
        fn tag_chosen(&self, sender: &NSPopUpButton) {
            let index = sender.indexOfSelectedItem();
            let tag = usize::try_from(index - 1)
                .ok()
                .and_then(|i| self.ivars().tag_names.borrow().get(i).cloned());
            if let (Some(tag), Some(search)) = (tag, self.ivars().search.get()) {
                search.setStringValue(&NSString::from_str(&format!("#{tag}")));
                self.reload();
            }
        }

        #[unsafe(method(menuAction:))]
        fn menu_action(&self, sender: &NSMenuItem) {
            if let Some(index) = self.ivars().selected.get() {
                self.perform(sender.tag(), index);
            }
        }
    }
);

impl HistoryWindow {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            window: OnceCell::new(),
            grid: OnceCell::new(),
            scroll: OnceCell::new(),
            search: OnceCell::new(),
            count: OnceCell::new(),
            tags: OnceCell::new(),
            tag_names: RefCell::new(Vec::new()),
            items: RefCell::new(Vec::new()),
            thumbnails: RefCell::new(HashMap::new()),
            selected: Cell::new(None),
            show: Cell::new(Show::All),
            elements: RefCell::new(Vec::new()),
        });
        // SAFETY: NSObject's designated initializer.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        this.build(mtm);
        this
    }

    fn build(&self, mtm: MainThreadMarker) {
        let (width, height) = (CELL.width * 4.0 + 2.0 * PADDING + 16.0, 640.0);
        // SAFETY: designated NSWindow initializer.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::ZERO, NSSize::new(width, height)),
                NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Closable
                    | NSWindowStyleMask::Miniaturizable
                    | NSWindowStyleMask::Resizable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: the WINDOW thread-local keeps the controller and window.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(ns_string!("Capture History"));
        window.setContentMinSize(NSSize::new(CELL.width + 2.0 * PADDING + 16.0, 300.0));
        window.setDelegate(Some(ProtocolObject::from_ref(self)));
        let content = window.contentView().expect("windows have a content view");
        let target: &AnyObject = self.as_ref();
        let top = NSAutoresizingMaskOptions::ViewMinYMargin;

        let search = NSSearchField::new(mtm);
        search.setPlaceholderString(Some(&NSString::from_str(&t!(
            "Search text, file names, notes and #tags"
        ))));
        // SAFETY: the action exists on HistoryWindow, which outlives the field.
        unsafe {
            search.setTarget(Some(target));
            search.setAction(Some(sel!(searchChanged:)));
        }
        search.setFrame(NSRect::new(
            NSPoint::new(PADDING, height - 38.0),
            NSSize::new(300.0, 26.0),
        ));
        search.setAutoresizingMask(top);
        content.addSubview(&search);

        let show = NSSegmentedControl::new(mtm);
        show.setSegmentCount(4);
        for (i, label) in ["All", "Screenshots", "Recordings", "Starred"]
            .iter()
            .enumerate()
        {
            show.setLabel_forSegment(&NSString::from_str(&crate::i18n::tr(label)), i as isize);
        }
        show.setSelectedSegment(0);
        // SAFETY: the action exists on HistoryWindow.
        unsafe {
            show.setTarget(Some(target));
            show.setAction(Some(sel!(showChanged:)));
        }
        show.setFrame(NSRect::new(
            NSPoint::new(PADDING + 312.0, height - 38.0),
            NSSize::new(350.0, 26.0),
        ));
        show.setAutoresizingMask(top);
        content.addSubview(&show);

        let tags = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(mtm),
            NSRect::new(
                NSPoint::new(PADDING + 672.0, height - 38.0),
                NSSize::new(100.0, 26.0),
            ),
            true,
        );
        // SAFETY: the action exists on HistoryWindow.
        unsafe {
            tags.setTarget(Some(target));
            tags.setAction(Some(sel!(tagChosen:)));
        }
        tags.setAutoresizingMask(top);
        content.addSubview(&tags);

        let count = NSTextField::labelWithString(ns_string!(""), mtm);
        count.setTextColor(Some(&NSColor::secondaryLabelColor()));
        count.setFrame(NSRect::new(
            NSPoint::new(width - 118.0, height - 34.0),
            NSSize::new(104.0, 18.0),
        ));
        count.setAlignment(objc2_app_kit::NSTextAlignment::Right);
        count.setAutoresizingMask(top | NSAutoresizingMaskOptions::ViewMinXMargin);
        content.addSubview(&count);

        let scroll = NSScrollView::new(mtm);
        scroll.setFrame(NSRect::new(
            NSPoint::ZERO,
            NSSize::new(width, height - TOOLBAR),
        ));
        scroll.setHasVerticalScroller(true);
        scroll.setAutohidesScrollers(true);
        scroll.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        let grid = HistoryGrid::new(
            mtm,
            NSRect::new(NSPoint::ZERO, NSSize::new(width, height - TOOLBAR)),
        );
        scroll.setDocumentView(Some(&grid));
        content.addSubview(&scroll);

        let _ = self.ivars().search.set(search);
        let _ = self.ivars().count.set(count);
        let _ = self.ivars().tags.set(tags);
        let _ = self.ivars().scroll.set(scroll);
        let _ = self.ivars().grid.set(grid.clone());
        let _ = self.ivars().window.set(window.clone());
        window.center();
        window.makeFirstResponder(Some(&grid));
    }

    fn reload(&self) {
        let ivars = self.ivars();
        let query = ivars
            .search
            .get()
            .map(|s| s.stringValue().to_string())
            .unwrap_or_default();
        let store = super::open_store();
        let entries = store
            .as_ref()
            .and_then(|store| store.list(&query, ivars.show.get(), LIMIT).ok())
            .unwrap_or_default();
        let tags = store
            .as_ref()
            .and_then(|store| store.all_tags().ok())
            .unwrap_or_default();
        self.show_tags(tags);
        let items: Vec<Item> = entries
            .into_iter()
            .map(|entry| Item {
                exists: entry.path.exists(),
                entry,
            })
            .collect();
        if let Some(count) = ivars.count.get() {
            let text = match items.len() {
                1 => t!("1 capture"),
                n => t!("{n} captures", n),
            };
            count.setStringValue(&NSString::from_str(&text));
        }
        *ivars.items.borrow_mut() = items;
        ivars.selected.set(None);
        self.describe_items();
        self.relayout();
    }

    /// Builds the accessibility elements VoiceOver lists for the grid: one
    /// per capture, labeled, with the context menu's commands as actions.
    fn describe_items(&self) {
        use objc2_app_kit::{NSAccessibility, NSAccessibilityCustomAction, NSAccessibilityElement};
        let Some(grid) = self.ivars().grid.get() else {
            return;
        };
        let items = self.ivars().items.borrow();
        let elements: Vec<Retained<NSAccessibilityElement>> = items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let label = item_label(item);
                // SAFETY: reading an AppKit constant; the grid is the parent
                // and outlives its elements (they are replaced on reload).
                let element = unsafe {
                    NSAccessibilityElement::accessibilityElementWithRole_frame_label_parent(
                        objc2_app_kit::NSAccessibilityImageRole,
                        NSRect::ZERO,
                        Some(&NSString::from_str(&label)),
                        Some(grid.as_ref()),
                    )
                }
                .downcast::<NSAccessibilityElement>()
                .expect("an accessibility element");
                let recording = item.entry.kind.is_recording();
                let mut commands = vec![
                    (if recording { "Play" } else { "Open" }, tag::OPEN),
                    (if recording { "Edit Video" } else { "Annotate" }, tag::EDIT),
                    ("Copy", tag::COPY),
                    ("Show in Finder", tag::SHOW_IN_FINDER),
                ];
                if !recording {
                    commands.push(("Pin to Screen", tag::PIN));
                    commands.push(("Print…", tag::PRINT));
                }
                if crate::upload::enabled() {
                    commands.push(("Upload", tag::UPLOAD));
                }
                if item.entry.link.is_some() {
                    commands.push(("Copy Link", tag::COPY_LINK));
                }
                commands.push(("Tags and Note…", tag::TAGS));
                commands.push(if item.entry.rating > 0 {
                    ("Remove Rating", tag::RATE)
                } else {
                    ("Star", tag::RATE + 1)
                });
                commands.push(("Delete", tag::DELETE));
                let actions: Vec<Retained<NSAccessibilityCustomAction>> = commands
                    .into_iter()
                    .filter(|(_, t)| item.exists || !needs_file(*t))
                    .map(|(name, t)| {
                        let block = block2::RcBlock::new(move || {
                            if let Some(c) = controller() {
                                c.perform(t, index);
                            }
                            objc2::runtime::Bool::YES
                        });
                        NSAccessibilityCustomAction::initWithName_handler(
                            NSAccessibilityCustomAction::alloc(),
                            &NSString::from_str(&crate::i18n::tr(name)),
                            Some(&block),
                        )
                    })
                    .collect();
                element
                    .setAccessibilityCustomActions(Some(&NSArray::from_retained_slice(&actions)));
                element
            })
            .collect();
        let children: Vec<Retained<AnyObject>> = elements
            .iter()
            .map(|e| Retained::into_super(Retained::into_super(e.clone())))
            .collect();
        crate::ui::access::describe(
            grid,
            // SAFETY: reading an AppKit constant.
            unsafe { objc2_app_kit::NSAccessibilityListRole },
            "Captures",
            Some(&t!(
                "Each capture has actions to open, copy, tag, show in Finder and more. Press 1 to 5 to rate the selected capture, 0 to remove its rating."
            )),
        );
        // SAFETY: an array of accessibility elements.
        unsafe { grid.setAccessibilityChildren(Some(&NSArray::from_retained_slice(&children))) };
        *self.ivars().elements.borrow_mut() = elements;
    }

    fn columns(&self) -> usize {
        let width = self
            .ivars()
            .scroll
            .get()
            .map_or(800.0, |s| s.contentSize().width);
        (((width - 2.0 * PADDING) / CELL.width).floor() as usize).max(1)
    }

    /// Sizes the grid to its content and redraws.
    fn relayout(&self) {
        let ivars = self.ivars();
        let (Some(scroll), Some(grid)) = (ivars.scroll.get(), ivars.grid.get()) else {
            return;
        };
        let visible = scroll.contentSize();
        let rows = ivars.items.borrow().len().div_ceil(self.columns());
        let height = (rows as f64 * CELL.height + 2.0 * PADDING).max(visible.height);
        grid.setFrameSize(NSSize::new(visible.width, height));
        grid.setNeedsDisplay(true);
        // Columns may have changed: move the accessibility elements along.
        for (index, element) in ivars.elements.borrow().iter().enumerate() {
            let r = self.cell_rect(index);
            element.setAccessibilityFrameInParentSpace(NSRect::new(
                NSPoint::new(r.x, r.y),
                NSSize::new(r.width, r.height),
            ));
        }
    }

    fn cell_rect(&self, index: usize) -> Rect {
        let columns = self.columns();
        Rect::new(
            PADDING + (index % columns) as f64 * CELL.width,
            PADDING + (index / columns) as f64 * CELL.height,
            CELL.width,
            CELL.height,
        )
    }

    fn index_at(&self, p: Point) -> Option<usize> {
        let count = self.ivars().items.borrow().len();
        (0..count).find(|&i| draw::inset(self.cell_rect(i), 4.0).contains(p))
    }

    fn thumbnail(&self, entry: &Entry) -> Option<Retained<NSImage>> {
        let mut cache = self.ivars().thumbnails.borrow_mut();
        if let Some(image) = cache.get(&entry.id) {
            return Some(image.clone());
        }
        let image = NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(&entry.thumbnail))?;
        cache.insert(entry.id, image.clone());
        Some(image)
    }

    fn draw_grid(&self, dirty: NSRect) {
        let items = self.ivars().items.borrow();
        if items.is_empty() {
            let message = if self
                .ivars()
                .search
                .get()
                .is_some_and(|s| s.stringValue().length() > 0)
            {
                "No captures match your search"
            } else {
                "Captures and recordings will appear here"
            };
            let size = draw::text_size(message, 15.0, Weight::Medium);
            let bounds = self.ivars().grid.get().map_or(dirty, |g| g.bounds());
            draw::draw_text(
                message,
                Point::new((bounds.size.width - size.width) / 2.0, 80.0),
                15.0,
                Weight::Medium,
                &NSColor::secondaryLabelColor(),
            );
            return;
        }
        let dirty = Rect::new(
            dirty.origin.x,
            dirty.origin.y,
            dirty.size.width,
            dirty.size.height,
        );
        let selected = self.ivars().selected.get();
        let context = NSGraphicsContext::currentContext();
        for (i, item) in items.iter().enumerate() {
            let cell = self.cell_rect(i);
            if cell.intersection(&dirty).width <= 0.0 {
                continue;
            }
            if selected == Some(i) {
                draw::fill_rounded(
                    draw::inset(cell, 4.0),
                    10.0,
                    &NSColor::selectedContentBackgroundColor().colorWithAlphaComponent(0.35),
                );
            }
            let thumb_box = Rect::new(cell.x + 10.0, cell.y + 10.0, THUMB.width, THUMB.height);
            draw::fill_rounded(thumb_box, 6.0, &NSColor::colorWithWhite_alpha(0.5, 0.12));
            if let Some(image) = self.thumbnail(&item.entry) {
                let size = image.size();
                let scale = (THUMB.width / size.width)
                    .min(THUMB.height / size.height)
                    .min(1.0);
                let (w, h) = (size.width * scale, size.height * scale);
                let at = Rect::new(
                    thumb_box.x + (THUMB.width - w) / 2.0,
                    thumb_box.y + (THUMB.height - h) / 2.0,
                    w,
                    h,
                );
                if let Some(context) = &context {
                    context.setImageInterpolation(NSImageInterpolation::High);
                }
                // SAFETY: drawing inside drawRect:.
                unsafe {
                    image.drawInRect_fromRect_operation_fraction_respectFlipped_hints(
                        draw::ns_rect(at),
                        NSRect::ZERO,
                        NSCompositingOperation::SourceOver,
                        if item.exists { 1.0 } else { 0.35 },
                        true,
                        None,
                    );
                }
            }
            let badge = match (item.entry.kind, item.entry.duration) {
                (Kind::Gif, _) => Some("GIF".to_string()),
                (Kind::Video, Some(d)) => {
                    Some(format!("▶ {}", crate::quick_access::format_duration(d)))
                }
                _ => None,
            };
            if let Some(badge) = badge {
                let size = draw::text_size(&badge, 10.0, Weight::Semibold);
                let r = Rect::new(
                    thumb_box.max_x() - size.width - 14.0,
                    thumb_box.max_y() - size.height - 10.0,
                    size.width + 8.0,
                    size.height + 4.0,
                );
                draw::fill_rounded(r, 4.0, &NSColor::colorWithWhite_alpha(0.0, 0.7));
                draw::draw_text(
                    &badge,
                    Point::new(r.x + 4.0, r.y + 2.0),
                    10.0,
                    Weight::Semibold,
                    &NSColor::whiteColor(),
                );
            }
            let when = crate::i18n::date(item.entry.created, true);
            let label = if item.exists {
                when
            } else {
                t!("{when} · file missing", when)
            };
            draw::draw_text(
                &label,
                Point::new(cell.x + 12.0, thumb_box.max_y() + 8.0),
                11.0,
                Weight::Medium,
                &NSColor::secondaryLabelColor(),
            );
            let size = format!("{} × {}", item.entry.width, item.entry.height);
            draw::draw_text(
                &size,
                Point::new(cell.x + 12.0, thumb_box.max_y() + 24.0),
                10.0,
                Weight::Medium,
                &NSColor::tertiaryLabelColor(),
            );
            if item.entry.rating > 0 {
                let stars = "★".repeat(item.entry.rating as usize);
                let width = draw::text_size(&stars, 11.0, Weight::Medium).width;
                draw::draw_text(
                    &stars,
                    Point::new(thumb_box.max_x() - width, thumb_box.max_y() + 22.0),
                    11.0,
                    Weight::Medium,
                    &NSColor::systemYellowColor(),
                );
            }
            let details = details_line(&item.entry);
            if !details.is_empty() {
                let text = fit(&details, THUMB.width, 10.0);
                draw::draw_text(
                    &text,
                    Point::new(cell.x + 12.0, thumb_box.max_y() + 39.0),
                    10.0,
                    Weight::Medium,
                    &NSColor::controlAccentColor(),
                );
            }
        }
    }

    fn select(&self, index: Option<usize>) {
        self.ivars().selected.set(index);
        if let Some(grid) = self.ivars().grid.get() {
            if let Some(i) = index {
                grid.scrollRectToVisible(draw::ns_rect(self.cell_rect(i)));
            }
            grid.setNeedsDisplay(true);
        }
    }

    fn context_menu(&self, mtm: MainThreadMarker, index: usize) -> Retained<NSMenu> {
        let items = self.ivars().items.borrow();
        let item = &items[index];
        let recording = item.entry.kind.is_recording();
        let menu = NSMenu::new(mtm);
        menu.setAutoenablesItems(false);
        let target: &AnyObject = self.as_ref();
        let add = |title: &str, tag: isize, enabled: bool| {
            // SAFETY: `menuAction:` exists on HistoryWindow, which outlives the menu.
            let menu_item = unsafe {
                let i = NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &NSString::from_str(title),
                    Some(sel!(menuAction:)),
                    ns_string!(""),
                );
                i.setTarget(Some(target));
                i
            };
            menu_item.setTag(tag);
            menu_item.setEnabled(enabled);
            menu.addItem(&menu_item);
        };
        let exists = item.exists;
        add(if recording { "Play" } else { "Open" }, tag::OPEN, exists);
        add(
            if recording {
                "Edit Video…"
            } else {
                "Annotate…"
            },
            tag::EDIT,
            exists && item.entry.kind != Kind::Gif,
        );
        add("Copy", tag::COPY, exists);
        add("Show in Finder", tag::SHOW_IN_FINDER, exists);
        if !recording {
            add("Pin to Screen", tag::PIN, exists);
            add("Print…", tag::PRINT, exists);
        }
        if crate::upload::enabled() {
            add("Upload", tag::UPLOAD, exists);
        }
        if item.entry.link.is_some() {
            add("Copy Link", tag::COPY_LINK, true);
        }
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        add("Tags and Note…", tag::TAGS, true);
        let rating = NSMenu::new(mtm);
        for stars in 0..=5u8 {
            let title = if stars == 0 {
                t!("No Rating")
            } else {
                "★".repeat(stars as usize)
            };
            // SAFETY: `menuAction:` exists on HistoryWindow, which outlives
            // the menu.
            let menu_item = unsafe {
                let i = NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &NSString::from_str(&title),
                    Some(sel!(menuAction:)),
                    &NSString::from_str(&stars.to_string()),
                );
                i.setTarget(Some(target));
                i
            };
            menu_item.setKeyEquivalentModifierMask(NSEventModifierFlags::empty());
            menu_item.setTag(tag::RATE + stars as isize);
            menu_item.setState(if stars == item.entry.rating {
                objc2_app_kit::NSControlStateValueOn
            } else {
                objc2_app_kit::NSControlStateValueOff
            });
            rating.addItem(&menu_item);
        }
        let rating_item = NSMenuItem::new(mtm);
        rating_item.setTitle(&NSString::from_str(&t!("Rating")));
        rating_item.setSubmenu(Some(&rating));
        menu.addItem(&rating_item);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        add("Delete", tag::DELETE, true);
        menu
    }

    fn perform(&self, tag: isize, index: usize) {
        let mtm = self.mtm();
        let Some((entry, exists)) = self
            .ivars()
            .items
            .borrow()
            .get(index)
            .map(|i| (i.entry.clone(), i.exists))
        else {
            return;
        };
        if !exists && needs_file(tag) {
            crate::ui::toast::show(mtm, "The file is no longer there");
            return;
        }
        let url = || NSURL::fileURLWithPath(&NSString::from_str(&entry.path.to_string_lossy()));
        match tag {
            tag::OPEN if entry.kind.is_recording() => {
                NSWorkspace::sharedWorkspace().openURL(&url());
            }
            tag::OPEN | tag::EDIT => crate::editor::open_file(mtm, &entry.path),
            tag::COPY => copy(&entry),
            tag::UPLOAD => crate::upload::start(mtm, entry.path.clone()),
            tag::COPY_LINK => {
                if let Some(link) = &entry.link {
                    crate::ocr::copy_string(link);
                    crate::ui::toast::show(mtm, "Link copied");
                }
            }
            tag::SHOW_IN_FINDER => {
                NSWorkspace::sharedWorkspace()
                    .activateFileViewerSelectingURLs(&NSArray::from_retained_slice(&[url()]));
            }
            tag::PRINT => {
                if let Some(image) = crate::capture::image::load(&entry.path) {
                    let title = entry
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    crate::ui::print::print_image(mtm, &image, &title);
                }
            }
            tag::PIN => {
                if let Some(image) = crate::capture::image::load(&entry.path) {
                    let scale = crate::capture::image::scale_for_path(&entry.path);
                    let item = crate::quick_access::Item::new(
                        image,
                        scale,
                        None,
                        entry.path.clone(),
                        true,
                    );
                    crate::pin::pin(mtm, &item);
                }
            }
            tag::TAGS => self.edit_tags(entry, index),
            t if (tag::RATE..=tag::RATE + 5).contains(&t) => {
                let rating = (t - tag::RATE) as u8;
                if let Some(store) = super::open_store()
                    && let Err(e) = store.set_rating(entry.id, rating)
                {
                    tracing::warn!(error = %e, "could not rate the capture");
                }
                self.reload_keeping(index);
                crate::ui::access::announce(
                    mtm,
                    &if rating == 0 {
                        t!("Rating removed")
                    } else {
                        t!("Rated {rating} of 5", rating)
                    },
                );
            }
            tag::DELETE => {
                if let Some(store) = super::open_store()
                    && let Ok(Some(path)) = store.delete(entry.id)
                    && super::owns_file(&path)
                {
                    let _ = std::fs::remove_file(&path);
                }
                self.ivars().thumbnails.borrow_mut().remove(&entry.id);
                self.reload();
                let count = self.ivars().items.borrow().len();
                if count > 0 {
                    self.select(Some(index.min(count - 1)));
                }
            }
            _ => {}
        }
    }
}

impl HistoryWindow {
    /// Fills the Tags menu; its first item is the button's title.
    fn show_tags(&self, tags: Vec<(String, usize)>) {
        let Some(button) = self.ivars().tags.get() else {
            return;
        };
        button.removeAllItems();
        button.addItemWithTitle(&NSString::from_str(&t!("Tags")));
        for (tag, count) in &tags {
            button.addItemWithTitle(&NSString::from_str(&format!("#{tag} ({count})")));
        }
        button.setEnabled(!tags.is_empty());
        *self.ivars().tag_names.borrow_mut() = tags.into_iter().map(|(t, _)| t).collect();
    }

    /// Reloads (after an edit) and selects the same capture again, if it
    /// is still listed.
    fn reload_keeping(&self, index: usize) {
        let id = self.ivars().items.borrow().get(index).map(|i| i.entry.id);
        self.reload();
        let position = self
            .ivars()
            .items
            .borrow()
            .iter()
            .position(|i| Some(i.entry.id) == id);
        self.select(position);
    }

    /// Asks for a capture's tags and note in a sheet.
    fn edit_tags(&self, entry: Entry, index: usize) {
        use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn};
        let mtm = self.mtm();
        let Some(window) = self.ivars().window.get() else {
            return;
        };
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str(&t!("Tags and Note")));
        alert.setInformativeText(&NSString::from_str(&t!(
            "Separate tags with commas or spaces. Search for #tag to find them."
        )));
        alert.addButtonWithTitle(&NSString::from_str(&t!("Save")));
        alert.addButtonWithTitle(&NSString::from_str(&t!("Cancel")));
        let accessory = NSView::initWithFrame(
            NSView::alloc(mtm),
            NSRect::new(NSPoint::ZERO, NSSize::new(300.0, 112.0)),
        );
        let label = |text: &str, y: f64| {
            let l = NSTextField::labelWithString(&NSString::from_str(text), mtm);
            l.setFrame(NSRect::new(NSPoint::new(0.0, y), NSSize::new(300.0, 16.0)));
            accessory.addSubview(&l);
        };
        let field = |value: &str, y: f64, height: f64, name: &str| {
            let f = NSTextField::new(mtm);
            f.setStringValue(&NSString::from_str(value));
            f.setFrame(NSRect::new(
                NSPoint::new(0.0, y),
                NSSize::new(300.0, height),
            ));
            use objc2_app_kit::NSAccessibility;
            f.setAccessibilityLabel(Some(&NSString::from_str(name)));
            accessory.addSubview(&f);
            f
        };
        let tags_name = t!("Tags");
        let note_name = t!("Note");
        label(&tags_name, 94.0);
        let tags = field(&entry.tags.join(", "), 68.0, 24.0, &tags_name);
        label(&note_name, 48.0);
        let note = field(&entry.note, 0.0, 46.0, &note_name);
        tags.setPlaceholderString(Some(&NSString::from_str(&t!("bug, design"))));
        alert.setAccessoryView(Some(&accessory));
        alert.window().setInitialFirstResponder(Some(&tags));
        let handler = block2::RcBlock::new(move |response: objc2_app_kit::NSModalResponse| {
            if response != NSAlertFirstButtonReturn {
                return;
            }
            let new_tags = super::store::parse_tags(&tags.stringValue().to_string());
            if let Some(store) = super::open_store()
                && let Err(e) =
                    store.set_tags_and_note(entry.id, &new_tags, &note.stringValue().to_string())
            {
                tracing::warn!(error = %e, "could not save the tags");
            }
            if let Some(c) = controller() {
                c.reload_keeping(index);
            }
        });
        alert.beginSheetModalForWindow_completionHandler(window, Some(&handler));
    }
}

/// Whether a command needs the capture's file to exist.
fn needs_file(tag: isize) -> bool {
    !matches!(tag, tag::DELETE | tag::COPY_LINK | tag::TAGS)
        && !(tag::RATE..=tag::RATE + 5).contains(&tag)
}

/// The tags and note under a capture: `#bug #ui · Login fails`.
fn details_line(entry: &Entry) -> String {
    let tags = entry
        .tags
        .iter()
        .map(|t| format!("#{t}"))
        .collect::<Vec<_>>()
        .join(" ");
    let note = entry.note.lines().next().unwrap_or("").trim();
    match (tags.is_empty(), note.is_empty()) {
        (false, false) => format!("{tags} · {note}"),
        (false, true) => tags,
        (true, false) => note.to_string(),
        (true, true) => String::new(),
    }
}

/// `text`, cut with an ellipsis to fit `width` points.
fn fit(text: &str, width: f64, size: f64) -> String {
    if draw::text_size(text, size, Weight::Medium).width <= width {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut n = chars.len();
    while n > 0 {
        n -= 1;
        let cut: String = chars[..n].iter().collect::<String>() + "…";
        if draw::text_size(&cut, size, Weight::Medium).width <= width {
            return cut;
        }
    }
    String::new()
}

/// What VoiceOver says for a history item: kind, size or length, date,
/// rating, tags and note.
fn item_label(item: &Item) -> String {
    let e = &item.entry;
    let when = crate::i18n::date(e.created, false);
    let length = || e.duration.map(crate::quick_access::format_duration);
    let what = match (e.kind, length()) {
        (Kind::Image, _) => t!(
            "Screenshot, {width} by {height} pixels",
            width = e.width,
            height = e.height
        ),
        (Kind::Gif, Some(length)) => t!("GIF, {length}", length),
        (Kind::Gif, None) => t!("GIF"),
        (Kind::Video, Some(length)) => t!("Recording, {length}", length),
        (Kind::Video, None) => t!("Recording"),
    };
    let mut label = if item.exists {
        t!("{what}, {when}", what, when)
    } else {
        t!("{what}, {when}, file missing", what, when)
    };
    if e.rating > 0 {
        let rating = e.rating;
        label.push_str(&format!(", {}", t!("{rating} stars", rating)));
    }
    let details = details_line(e);
    if !details.is_empty() {
        label.push_str(&format!(", {details}"));
    }
    label
}

fn copy(entry: &Entry) {
    if entry.kind.is_recording() {
        crate::quick_access::copy_file(&entry.path);
        return;
    }
    let png = if has_extension(&entry.path, "png") {
        std::fs::read(&entry.path).ok()
    } else {
        crate::capture::image::load(&entry.path)
            .and_then(|image| crate::capture::image::encode(&image, ImageFormat::Png, 1.0))
    };
    if let Some(png) = png {
        crate::capture::output::copy_png(&png, &crate::capture::output::name_of(&entry.path));
    }
}

fn has_extension(path: &Path, ext: &str) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

define_class!(
    // SAFETY: NSView can be subclassed; no Drop implementation.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    struct HistoryGrid;

    impl HistoryGrid {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, dirty: NSRect) {
            if let Some(c) = controller() {
                c.draw_grid(dirty);
            }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let Some(c) = controller() else {
                return;
            };
            if let Some(window) = self.window() {
                window.makeFirstResponder(Some(self));
            }
            let index = c.index_at(self.point(event));
            c.select(index);
            if event.clickCount() == 2
                && let Some(i) = index
            {
                c.perform(tag::OPEN, i);
            }
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            let Some(c) = controller() else {
                return;
            };
            let Some(index) = c.index_at(self.point(event)) else {
                return;
            };
            c.select(Some(index));
            let menu = c.context_menu(self.mtm(), index);
            NSMenu::popUpContextMenu_withEvent_forView(&menu, event, self);
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            const RETURN: u16 = 36;
            const DELETE: u16 = 51;
            const FORWARD_DELETE: u16 = 117;
            const LEFT: u16 = 123;
            const RIGHT: u16 = 124;
            const DOWN: u16 = 125;
            const UP: u16 = 126;
            let Some(c) = controller() else {
                return;
            };
            let count = c.ivars().items.borrow().len();
            let selected = c.ivars().selected.get();
            let command = event.modifierFlags().contains(NSEventModifierFlags::Command);
            let columns = c.columns();
            let moved = |delta: isize| {
                let current = selected.map_or(-1, |s| s as isize);
                Some((current + delta).clamp(0, count as isize - 1) as usize)
            };
            match (command, event.keyCode()) {
                (_, DELETE | FORWARD_DELETE) => {
                    if let Some(i) = selected {
                        c.perform(tag::DELETE, i);
                    }
                }
                (false, RETURN) => {
                    if let Some(i) = selected {
                        c.perform(tag::OPEN, i);
                    }
                }
                (false, LEFT) if count > 0 => c.select(moved(-1)),
                (false, RIGHT) if count > 0 => c.select(moved(1)),
                (false, UP) if count > 0 => c.select(moved(-(columns as isize))),
                (false, DOWN) if count > 0 => c.select(moved(columns as isize)),
                (false, _)
                    if let Some(i) = selected
                        && let Some(stars) = digit(event) =>
                {
                    c.perform(tag::RATE + stars, i);
                }
                _ => {
                    // SAFETY: forwarding the event to the superclass.
                    unsafe {
                        let _: () = msg_send![super(self), keyDown: event];
                    }
                }
            }
        }

        #[unsafe(method(copy:))]
        fn copy_action(&self, _sender: Option<&AnyObject>) {
            if let Some(c) = controller()
                && let Some(i) = c.ivars().selected.get()
            {
                c.perform(tag::COPY, i);
            }
        }
    }
);

impl HistoryGrid {
    fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSView's designated initializer.
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    fn point(&self, event: &NSEvent) -> Point {
        let p = self.convertPoint_fromView(event.locationInWindow(), None);
        Point::new(p.x, p.y)
    }
}

/// 0 to 5 typed on the keyboard, for rating.
fn digit(event: &NSEvent) -> Option<isize> {
    let typed = event.charactersIgnoringModifiers()?.to_string();
    let mut chars = typed.chars();
    match (chars.next()?.to_digit(10), chars.next()) {
        (Some(d), None) if d <= 5 => Some(d as isize),
        _ => None,
    }
}

fn controller() -> Option<Retained<HistoryWindow>> {
    WINDOW.with(|w| w.borrow().clone())
}
