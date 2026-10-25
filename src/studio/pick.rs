//! Choosing where a zoom stays by clicking the Studio preview: a clear
//! view over the player while choosing, with a crosshair and a hint. A
//! click inside the video picks the point; Escape (or a click outside the
//! video) stops without changing anything.

use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSAccessibility, NSColor, NSCursor, NSEvent, NSView};
use objc2_foundation::{NSPoint, NSRect};

use crate::capture::geometry::{Point, Rect};
use crate::ui::draw::{self, Weight};

pub struct Ivars {
    editor: u64,
    /// The video's place in this view (the player's video bounds).
    video: std::cell::Cell<NSRect>,
}

define_class!(
    // SAFETY: NSView can be subclassed; no Drop implementation.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    pub struct PickOverlay;

    impl PickOverlay {
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(resetCursorRects))]
        fn reset_cursor_rects(&self) {
            self.addCursorRect_cursor(self.ivars().video.get(), &NSCursor::crosshairCursor());
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let p = self.convertPoint_fromView(event.locationInWindow(), None);
            let video = self.ivars().video.get();
            let editor = super::registry::get(self.ivars().editor);
            match fraction(p, video) {
                Some(at) => {
                    if let Some(editor) = editor {
                        editor.point_picked(at);
                    }
                }
                None => {
                    if let Some(editor) = editor {
                        editor.stop_picking();
                    }
                }
            }
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            // Escape.
            if event.keyCode() == 53 {
                if let Some(editor) = super::registry::get(self.ivars().editor) {
                    editor.stop_picking();
                }
            } else {
                // SAFETY: passing the event on, as NSView does.
                unsafe { msg_send![super(self), keyDown: event] }
            }
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            self.draw();
        }
    }
);

/// Where `p` is in `video`, as fractions of its width and height from the
/// top left; `None` outside it.
fn fraction(p: NSPoint, video: NSRect) -> Option<(f64, f64)> {
    if video.size.width <= 0.0 || video.size.height <= 0.0 {
        return None;
    }
    let x = (p.x - video.origin.x) / video.size.width;
    // The view is not flipped: y grows upward.
    let y = 1.0 - (p.y - video.origin.y) / video.size.height;
    ((0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y)).then_some((x, y))
}

impl PickOverlay {
    /// An overlay `frame` large for Studio editor `editor`, whose video is
    /// at `video` within it.
    pub fn new(mtm: MainThreadMarker, editor: u64, frame: NSRect, video: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            editor,
            video: std::cell::Cell::new(video),
        });
        // SAFETY: NSView's designated initializer.
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        this.setAccessibilityLabel(Some(&objc2_foundation::NSString::from_str(&t!(
            "Click where the zoom should stay"
        ))));
        this
    }

    /// The video moved (the window was resized).
    pub fn set_video(&self, video: NSRect) {
        self.ivars().video.set(video);
        if let Some(window) = self.window() {
            window.invalidateCursorRectsForView(self);
        }
        self.setNeedsDisplay(true);
    }

    fn draw(&self) {
        let bounds = self.bounds();
        let video = self.ivars().video.get();
        // Dim around the video, so it is clear where to click.
        let dim = NSColor::colorWithWhite_alpha(0.0, 0.35);
        let (x0, y0) = (video.origin.x, video.origin.y);
        let (x1, y1) = (x0 + video.size.width, y0 + video.size.height);
        let (w, h) = (bounds.size.width, bounds.size.height);
        for r in [
            Rect::new(0.0, 0.0, w, y0),
            Rect::new(0.0, y1, w, h - y1),
            Rect::new(0.0, y0, x0, y1 - y0),
            Rect::new(x1, y0, w - x1, y1 - y0),
        ] {
            if r.width > 0.0 && r.height > 0.0 {
                draw::fill_rect(r, &dim);
            }
        }
        // The hint, at the top of the video.
        let hint = t!("Click where the zoom should stay. Esc cancels.");
        let size = draw::text_size(&hint, 13.0, Weight::Semibold);
        let pill = Rect::new(
            x0 + (video.size.width - size.width - 24.0) / 2.0,
            y1 - size.height - 22.0,
            size.width + 24.0,
            size.height + 10.0,
        );
        draw::fill_rounded(
            pill,
            pill.height / 2.0,
            &NSColor::colorWithWhite_alpha(0.0, 0.7),
        );
        draw::draw_text(
            &hint,
            Point::new(pill.x + 12.0, pill.y + 5.0),
            13.0,
            Weight::Semibold,
            &NSColor::whiteColor(),
        );
    }
}

#[cfg(test)]
mod tests {
    use objc2_foundation::{NSPoint, NSRect, NSSize};

    #[test]
    fn measures_clicks_from_the_videos_top_left() {
        let video = NSRect::new(NSPoint::new(100.0, 50.0), NSSize::new(400.0, 200.0));
        assert_eq!(
            super::fraction(NSPoint::new(100.0, 250.0), video),
            Some((0.0, 0.0))
        );
        assert_eq!(
            super::fraction(NSPoint::new(300.0, 100.0), video),
            Some((0.5, 0.75))
        );
        assert_eq!(super::fraction(NSPoint::new(99.0, 100.0), video), None);
    }
}
