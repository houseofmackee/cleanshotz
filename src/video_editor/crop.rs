//! The crop overlay: a view over the player that shows the kept area and,
//! while cropping, lets you draw, move and resize it. Outside crop mode it
//! lets every click through to the player.

use std::cell::Cell;

use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSAccessibility, NSColor, NSEvent, NSView};
use objc2_foundation::{NSPoint, NSRect};

use super::registry;
use super::window::VideoEditor;
use crate::capture::geometry::{Point, Rect};
use crate::ui::draw;

const ESCAPE: u16 = 53;
const RETURN: u16 = 36;
const KEYPAD_ENTER: u16 = 76;
/// How close (in points) a press must be to a corner to grab it.
const GRAB: f64 = 10.0;

#[derive(Clone, Copy, Debug)]
enum Drag {
    /// Drawing a new crop from this corner (video pixels).
    New(Point),
    /// Moving the crop; the last pointer position (video pixels).
    Move(Point),
    /// Resizing; the corner that stays put (video pixels).
    Corner(Point),
}

pub struct CropIvars {
    editor: u64,
    editing: Cell<bool>,
    drag: Cell<Option<Drag>>,
}

define_class!(
    // SAFETY: NSView can be subclassed; no Drop implementation.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = CropIvars]
    pub struct CropView;

    impl CropView {
        /// Clicks pass through to the player unless cropping.
        #[unsafe(method(hitTest:))]
        fn hit_test(&self, point: NSPoint) -> *mut NSView {
            if !self.ivars().editing.get() {
                return std::ptr::null_mut();
            }
            // SAFETY: forwarding to NSView.
            unsafe { msg_send![super(self), hitTest: point] }
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            self.ivars().editing.get()
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            self.draw();
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            self.press(self.video_point(event));
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            self.drag_to(self.video_point(event));
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            self.ivars().drag.set(None);
            if let Some(editor) = self.editor() {
                // Too small or the whole frame: no crop.
                let crop = editor
                    .crop()
                    .and_then(|c| super::plan::clamp_crop(c, editor.source_size()));
                editor.set_crop(crop);
            }
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let Some(editor) = self.editor() else {
                return;
            };
            match event.keyCode() {
                ESCAPE => {
                    editor.set_crop(None);
                    editor.end_cropping();
                }
                RETURN | KEYPAD_ENTER => editor.end_cropping(),
                // SAFETY: forwarding to NSView.
                _ => unsafe { msg_send![super(self), keyDown: event] },
            }
        }
    }
);

impl CropView {
    pub fn new(mtm: MainThreadMarker, editor: u64, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(CropIvars {
            editor,
            editing: Cell::new(false),
            drag: Cell::new(None),
        });
        // SAFETY: NSView's designated initializer.
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    fn editor(&self) -> Option<Retained<VideoEditor>> {
        registry::get(self.ivars().editor)
    }

    pub fn set_editing(&self, editing: bool) {
        self.ivars().editing.set(editing);
        if editing && let Some(window) = self.window() {
            window.makeFirstResponder(Some(self));
        }
        // Only an element while cropping; otherwise VoiceOver reaches the
        // player underneath.
        self.setAccessibilityElement(editing);
        if editing {
            let text = &t!(
                "Drag over the video to choose the area to keep. Return when done, Escape to remove the crop."
            );
            crate::ui::access::describe(
                self,
                // SAFETY: reading an AppKit constant.
                unsafe { objc2_app_kit::NSAccessibilityGroupRole },
                "Crop area",
                Some(text),
            );
            crate::ui::access::announce(self.mtm(), text);
        }
        self.setNeedsDisplay(true);
    }

    /// The pointer in video pixels (top-left origin), clamped to the frame.
    fn video_point(&self, event: &NSEvent) -> Point {
        let p = self.convertPoint_fromView(event.locationInWindow(), None);
        let Some(editor) = self.editor() else {
            return Point::default();
        };
        let (w, h) = editor.source_size();
        let vb = editor.video_rect();
        if vb.size.width <= 0.0 || vb.size.height <= 0.0 {
            return Point::default();
        }
        let x = (p.x - vb.origin.x) / vb.size.width * w;
        let y = (vb.origin.y + vb.size.height - p.y) / vb.size.height * h;
        Point::new(x.clamp(0.0, w), y.clamp(0.0, h))
    }

    /// Video pixels to view points.
    fn view_rect(&self, editor: &VideoEditor, r: Rect) -> Rect {
        let (w, _) = editor.source_size();
        let vb = editor.video_rect();
        let s = vb.size.width / w.max(1.0);
        Rect::new(
            vb.origin.x + r.x * s,
            vb.origin.y + vb.size.height - (r.y + r.height) * s,
            r.width * s,
            r.height * s,
        )
    }

    fn press(&self, p: Point) {
        let Some(editor) = self.editor() else {
            return;
        };
        let (w, _) = editor.source_size();
        let grab = GRAB * w / editor.video_rect().size.width.max(1.0);
        let drag = match editor.crop() {
            Some(c) => {
                let corners = [
                    (Point::new(c.x, c.y), Point::new(c.max_x(), c.max_y())),
                    (Point::new(c.max_x(), c.y), Point::new(c.x, c.max_y())),
                    (Point::new(c.x, c.max_y()), Point::new(c.max_x(), c.y)),
                    (Point::new(c.max_x(), c.max_y()), Point::new(c.x, c.y)),
                ];
                match corners
                    .iter()
                    .find(|(corner, _)| (corner.x - p.x).hypot(corner.y - p.y) <= grab)
                {
                    Some((_, fixed)) => Drag::Corner(*fixed),
                    None if c.contains(p) => Drag::Move(p),
                    None => Drag::New(p),
                }
            }
            None => Drag::New(p),
        };
        self.ivars().drag.set(Some(drag));
    }

    fn drag_to(&self, p: Point) {
        let Some(editor) = self.editor() else {
            return;
        };
        let (w, h) = editor.source_size();
        let frame = Rect::new(0.0, 0.0, w, h);
        let crop = match self.ivars().drag.get() {
            Some(Drag::New(anchor) | Drag::Corner(anchor)) => {
                Some(Rect::from_corners(anchor, p).intersection(&frame))
            }
            Some(Drag::Move(last)) => {
                self.ivars().drag.set(Some(Drag::Move(p)));
                editor.crop().map(|c| {
                    crate::capture::geometry::move_within(c, p.x - last.x, p.y - last.y, frame)
                })
            }
            None => return,
        };
        editor.set_crop(crop);
        self.setNeedsDisplay(true);
    }

    fn draw(&self) {
        let Some(editor) = self.editor() else {
            return;
        };
        let editing = self.ivars().editing.get();
        let vb = editor.video_rect();
        let video = Rect::new(vb.origin.x, vb.origin.y, vb.size.width, vb.size.height);
        let Some(crop) = editor.crop() else {
            if editing {
                draw::fill_rect(video, &NSColor::colorWithWhite_alpha(0.0, 0.25));
                draw::draw_label(
                    "Drag to choose the area to keep · Return when done · Escape to clear",
                    Point::new(video.x + 12.0, video.y + 12.0),
                    12.0,
                );
            }
            return;
        };
        let c = self.view_rect(&editor, crop);
        let dim = NSColor::colorWithWhite_alpha(0.0, if editing { 0.55 } else { 0.35 });
        // The four bands around the crop, inside the video.
        for r in [
            Rect::new(video.x, video.y, video.width, c.y - video.y),
            Rect::new(video.x, c.max_y(), video.width, video.max_y() - c.max_y()),
            Rect::new(video.x, c.y, c.x - video.x, c.height),
            Rect::new(c.max_x(), c.y, video.max_x() - c.max_x(), c.height),
        ] {
            if r.width > 0.0 && r.height > 0.0 {
                draw::fill_rect(r, &dim);
            }
        }
        NSColor::whiteColor().setStroke();
        draw::stroke_rect(draw::inset(c, -0.5), 1.0);
        if editing {
            let accent = NSColor::controlAccentColor();
            for corner in [
                Point::new(c.x, c.y),
                Point::new(c.max_x(), c.y),
                Point::new(c.x, c.max_y()),
                Point::new(c.max_x(), c.max_y()),
            ] {
                let r = Rect::new(corner.x - 4.5, corner.y - 4.5, 9.0, 9.0);
                draw::fill_rounded(r, 2.0, &NSColor::whiteColor());
                draw::stroke_rounded(r, 2.0, 1.5, &accent);
            }
            let label = format!("{} × {}", crop.width.round(), crop.height.round());
            draw::draw_label(&label, Point::new(c.x, (c.y - 24.0).max(video.y)), 12.0);
        }
    }
}
