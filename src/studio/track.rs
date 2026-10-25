//! The zoom track under the Studio preview: the recording's zooms as
//! blocks along its length, with the playhead. Drag a block to move it or
//! its edges to resize it, double-click an empty spot to add one, press
//! Delete to remove the selected one; clicking elsewhere moves the
//! playhead there.

use std::cell::Cell;

use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSColor, NSEvent, NSView};
use objc2_foundation::NSRect;

use super::zoom::Zoom;
use crate::capture::geometry::{Point, Rect};
use crate::ui::draw::{self, Weight};

/// Pointer within this many points of a block's edge resizes it.
const EDGE: f64 = 6.0;
/// The shortest zoom, in seconds.
const MIN_LENGTH: f64 = 0.5;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    /// Moving zoom `index`; `grab` is the time under the pointer minus its
    /// start.
    Move {
        index: usize,
        grab: f64,
    },
    Start {
        index: usize,
    },
    End {
        index: usize,
    },
}

pub struct TrackIvars {
    editor: u64,
    drag: Cell<Option<Drag>>,
}

define_class!(
    // SAFETY: NSView can be subclassed; no Drop implementation.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = TrackIvars]
    pub struct ZoomTrack;

    impl ZoomTrack {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
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

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            if let Some(window) = self.window() {
                window.makeFirstResponder(Some(self));
            }
            let p = self.point(event);
            let Some(editor) = super::registry::get(self.ivars().editor) else {
                return;
            };
            let zooms = editor.zooms();
            let t = self.time_at(p.x, editor.duration());
            let hit = zooms.iter().position(|z| {
                let r = self.block(z, editor.duration());
                p.x >= r.x - EDGE && p.x <= r.max_x() + EDGE
            });
            match hit {
                Some(index) => {
                    let r = self.block(&zooms[index], editor.duration());
                    let drag = if (p.x - r.x).abs() <= EDGE {
                        Drag::Start { index }
                    } else if (p.x - r.max_x()).abs() <= EDGE {
                        Drag::End { index }
                    } else {
                        Drag::Move {
                            index,
                            grab: t - zooms[index].start,
                        }
                    };
                    self.ivars().drag.set(Some(drag));
                    editor.select_zoom(Some(index));
                }
                None if event.clickCount() >= 2 => editor.add_zoom(t),
                None => {
                    editor.select_zoom(None);
                    editor.seek(t);
                }
            }
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let Some(drag) = self.ivars().drag.get() else {
                return;
            };
            let Some(editor) = super::registry::get(self.ivars().editor) else {
                return;
            };
            let duration = editor.duration();
            let t = self.time_at(self.point(event).x, duration);
            let mut zooms = editor.zooms();
            match drag {
                Drag::Move { index, grab } => {
                    if let Some(z) = zooms.get_mut(index) {
                        let length = z.end - z.start;
                        z.start = (t - grab).clamp(0.0, (duration - length).max(0.0));
                        z.end = z.start + length;
                        z.auto = false;
                    }
                }
                Drag::Start { index } => {
                    if let Some(z) = zooms.get_mut(index) {
                        z.start = t.clamp(0.0, z.end - MIN_LENGTH);
                        z.auto = false;
                    }
                }
                Drag::End { index } => {
                    if let Some(z) = zooms.get_mut(index) {
                        z.end = t.clamp(z.start + MIN_LENGTH, duration);
                        z.auto = false;
                    }
                }
            }
            // Shown at once; the preview catches up when the drag ends.
            editor.set_zooms_quietly(zooms);
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            if self.ivars().drag.take().is_some()
                && let Some(editor) = super::registry::get(self.ivars().editor)
            {
                editor.commit_zooms();
            }
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            const DELETE: u16 = 51;
            const FORWARD_DELETE: u16 = 117;
            if matches!(event.keyCode(), DELETE | FORWARD_DELETE)
                && let Some(editor) = super::registry::get(self.ivars().editor)
            {
                editor.delete_zoom();
                return;
            }
            // SAFETY: forwarding to NSView.
            unsafe {
                let _: () = msg_send![super(self), keyDown: event];
            }
        }
    }
);

impl ZoomTrack {
    pub fn new(mtm: MainThreadMarker, editor: u64, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(TrackIvars {
            editor,
            drag: Cell::new(None),
        });
        // SAFETY: NSView's designated initializer.
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        crate::ui::access::describe(
            &this,
            // SAFETY: reading an AppKit constant.
            unsafe { objc2_app_kit::NSAccessibilityGroupRole },
            "Zooms",
            Some(&t!(
                "Drag a zoom to move it or its edges to resize it. Double-click to add one; Delete removes the selected one."
            )),
        );
        this
    }

    fn point(&self, event: &NSEvent) -> Point {
        let p = self.convertPoint_fromView(event.locationInWindow(), None);
        Point::new(p.x, p.y)
    }

    /// The bar the zooms are drawn in.
    fn lane(&self) -> Rect {
        let b = self.bounds();
        Rect::new(
            12.0,
            10.0,
            (b.size.width - 24.0).max(1.0),
            b.size.height - 20.0,
        )
    }

    fn time_at(&self, x: f64, duration: f64) -> f64 {
        let lane = self.lane();
        ((x - lane.x) / lane.width * duration).clamp(0.0, duration.max(0.0))
    }

    fn x_at(&self, t: f64, duration: f64) -> f64 {
        let lane = self.lane();
        lane.x + t / duration.max(f64::MIN_POSITIVE) * lane.width
    }

    fn block(&self, zoom: &Zoom, duration: f64) -> Rect {
        let lane = self.lane();
        let (a, b) = (
            self.x_at(zoom.start, duration),
            self.x_at(zoom.end, duration),
        );
        Rect::new(a, lane.y + 4.0, (b - a).max(4.0), lane.height - 8.0)
    }

    fn draw(&self) {
        let Some(editor) = super::registry::get(self.ivars().editor) else {
            return;
        };
        let duration = editor.duration();
        let lane = self.lane();
        draw::fill_rounded(lane, 6.0, &NSColor::colorWithWhite_alpha(0.5, 0.15));
        // What is cut (filler words) is left out of the result.
        let cut = NSColor::systemRedColor().colorWithAlphaComponent(0.3);
        for span in editor.cuts() {
            let (a, b) = (
                self.x_at(span.start, duration),
                self.x_at(span.end, duration),
            );
            draw::fill_rect(Rect::new(a, lane.y, (b - a).max(1.0), lane.height), &cut);
        }
        let selected = editor.selected_zoom();
        let accent = NSColor::controlAccentColor();
        for (i, zoom) in editor.zooms().iter().enumerate() {
            let r = self.block(zoom, duration);
            let color = if Some(i) == selected {
                accent.clone()
            } else {
                accent.colorWithAlphaComponent(0.55)
            };
            draw::fill_rounded(r, 5.0, &color);
            let label = format!("{:.1}×", zoom.level);
            if r.width > 36.0 {
                draw::draw_text_centered(&label, r, 11.0, Weight::Semibold, &NSColor::whiteColor());
            }
        }
        let x = self.x_at(editor.current_time(), duration).round() + 0.5;
        draw::fill_rect(
            Rect::new(x - 1.0, lane.y - 4.0, 2.0, lane.height + 8.0),
            &NSColor::systemRedColor(),
        );
    }
}
