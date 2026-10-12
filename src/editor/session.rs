//! Editor interaction logic, independent of AppKit: what a press, drag and
//! release do with each tool. Points are image points.

use super::model::{
    Annotation, ArrowStyle, Background, Color, Document, Handle, History, Shape, TextStyle, Turn,
};
use super::picture::Picture;
use super::tools::Tool;
use crate::capture::geometry::{Point, Rect};

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    Create { id: u64, anchor: Point },
    Move { id: u64, last: Point, moved: bool },
    Handle { id: u64, handle: Handle },
    CropNew { anchor: Point },
    CropMove { last: Point },
    CropHandle { handle: Handle },
}

/// What the view should do after an event.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    Redraw,
    /// Start editing text: a new text at `at`, or the existing annotation.
    EditText {
        at: Point,
        existing: Option<u64>,
    },
}

pub struct Session {
    pub doc: Document,
    pub history: History,
    pub tool: Tool,
    pub color: Color,
    pub width: f64,
    pub arrow_style: ArrowStyle,
    pub text_style: TextStyle,
    pub text_size: f64,
    pub filled: bool,
    /// New rectangles, ellipses, lines and arrows look hand-drawn.
    pub sketchy: bool,
    /// The highlighter snaps to lines of text.
    pub highlight_text: bool,
    /// Lines of recognized text (image points), for the text highlighter.
    pub text_lines: Vec<Rect>,
    /// A click with the Measure tool, to measure automatically; the view
    /// takes it with [`Session::take_pending_measure`].
    pending_measure: Option<Point>,
    pub selection: Option<u64>,
    /// The crop being edited while the crop tool is active.
    pub crop_draft: Option<Rect>,
    /// Changed since the last save or copy.
    pub dirty: bool,
    /// The captured image's size in points, before any turn.
    original_size: (f64, f64),
    drag: Option<Drag>,
}

impl Session {
    /// `image_width` and `image_height`: the original (unturned) capture.
    pub fn new(doc: Document, image_width: f64, image_height: f64) -> Self {
        Self {
            doc,
            history: History::default(),
            tool: Tool::Arrow,
            color: Color::RED,
            width: 4.0,
            arrow_style: ArrowStyle::Tapered,
            text_style: TextStyle::Plain,
            text_size: 24.0,
            filled: false,
            sketchy: false,
            highlight_text: false,
            text_lines: Vec::new(),
            pending_measure: None,
            selection: None,
            crop_draft: None,
            dirty: false,
            original_size: (image_width, image_height),
            drag: None,
        }
    }

    /// Everything that can be shown: the captured image plus image layers
    /// placed beyond it.
    pub fn extent(&self) -> Rect {
        let (w, h) = self.image_size();
        self.doc.extent(w, h)
    }

    /// Adds `picture` as an image layer centered on `center` (or on the
    /// captured image), shrunk to fit if it is larger than the capture,
    /// and selects it.
    pub fn insert_picture(&mut self, picture: Picture, center: Option<Point>) -> u64 {
        let (w, h) = picture.size();
        let (iw, ih) = self.image_size();
        let fit = (iw * 0.8 / w).min(ih * 0.8 / h).min(1.0);
        let (w, h) = (w * fit, h * fit);
        let c = center.unwrap_or(Point::new(iw / 2.0, ih / 2.0));
        let rect = Rect::new((c.x - w / 2.0).round(), (c.y - h / 2.0).round(), w, h);
        self.add_picture(picture, rect)
    }

    /// Adds `picture` at full size next to everything shown so far, to
    /// the right or below, aligned to the top or left edge: combining
    /// captures into one image.
    pub fn append_picture(&mut self, picture: Picture, below: bool) -> u64 {
        let (w, h) = picture.size();
        let extent = self.extent();
        let rect = if below {
            Rect::new(extent.x, extent.max_y(), w, h)
        } else {
            Rect::new(extent.max_x(), extent.y, w, h)
        };
        self.add_picture(picture, rect)
    }

    fn add_picture(&mut self, picture: Picture, rect: Rect) -> u64 {
        if self.tool == Tool::Crop {
            self.set_tool(Tool::Select);
        }
        self.change();
        let id = self.doc.add(
            Shape::Image {
                rect,
                picture,
                opacity: 1.0,
            },
            self.color,
            self.width,
        );
        self.selection = Some(id);
        id
    }

    /// Adds the watermark (replacing one already there), selected, as one
    /// undo step. `frame` is the visible picture, for placing it.
    pub fn add_watermark(
        &mut self,
        style: crate::watermark::Style,
        picture: Picture,
        frame: Rect,
    ) -> u64 {
        self.change();
        if let Some(old) = self.doc.watermark() {
            self.doc.remove(old);
        }
        let id = self.doc.add(
            Shape::Watermark {
                style,
                picture,
                free: None,
                rect: Rect::default(),
                frame: Rect::default(),
            },
            self.color,
            self.width,
        );
        self.doc.place_watermarks(frame);
        self.selection = Some(id);
        id
    }

    /// Removes the watermark, as one undo step.
    pub fn remove_watermark(&mut self) {
        if let Some(id) = self.doc.watermark() {
            self.change();
            self.doc.remove(id);
            if self.selection == Some(id) {
                self.selection = None;
            }
        }
    }

    /// The watermark's position: `Some` anchored there, `None` once
    /// dragged.
    pub fn watermark_position(&self) -> Option<Option<crate::watermark::Position>> {
        let id = self.doc.watermark()?;
        match &self.doc.get(id)?.shape {
            Shape::Watermark { style, free, .. } => Some(free.is_none().then_some(style.position)),
            _ => None,
        }
    }

    /// Anchors the watermark at `position`, as one undo step.
    pub fn anchor_watermark(&mut self, position: crate::watermark::Position) {
        let Some(id) = self.doc.watermark() else {
            return;
        };
        if self.watermark_position() == Some(Some(position)) {
            return;
        }
        self.change();
        if let Some(Shape::Watermark {
            style,
            picture,
            free,
            rect,
            frame,
        }) = self.doc.get_mut(id).map(|a| &mut a.shape)
        {
            style.position = position;
            *free = None;
            *rect = crate::watermark::place(*frame, picture.size(), style, None);
        }
    }

    /// The captured image's size in points, as turned.
    pub fn image_size(&self) -> (f64, f64) {
        self.doc
            .orientation
            .size(self.original_size.0, self.original_size.1)
    }

    /// Sets the Resize factor (`None`: original size), as one undo step.
    pub fn set_output_scale(&mut self, scale: Option<f64>) {
        if self.doc.output_scale != scale {
            self.change();
            self.doc.output_scale = scale;
        }
    }

    /// Rotates or mirrors the image and everything on it, as one undo step.
    pub fn turn(&mut self, turn: Turn) {
        self.change();
        let (w, h) = self.image_size();
        self.doc.turn(turn, w, h, |p| p.turned(turn));
        if self.tool == Tool::Crop {
            self.crop_draft = Some(self.doc.crop.unwrap_or(self.extent()));
        }
    }

    fn change(&mut self) {
        self.history.record(&self.doc);
        self.dirty = true;
    }

    pub fn set_tool(&mut self, tool: Tool) {
        if self.tool == Tool::Crop && tool != Tool::Crop {
            self.apply_crop();
        }
        if tool == Tool::Crop {
            self.crop_draft = Some(self.doc.crop.unwrap_or(self.extent()));
            self.selection = None;
        }
        self.tool = tool;
    }

    /// Commits the crop draft to the document.
    pub fn apply_crop(&mut self) {
        let Some(draft) = self.crop_draft.take() else {
            return;
        };
        let crop = (draft != self.extent()).then_some(draft);
        if crop != self.doc.crop {
            self.change();
            self.doc.crop = crop;
        }
    }

    pub fn reset_crop(&mut self) {
        if self.tool == Tool::Crop {
            self.crop_draft = Some(self.extent());
        }
    }

    pub fn set_background(&mut self, background: Option<Background>) {
        if background != self.doc.background {
            self.change();
            self.doc.background = background;
        }
    }

    /// Applies to the selection too, as in CleanShot.
    pub fn set_color(&mut self, color: Color) {
        self.color = color;
        self.update_selected(|a| a.color = color);
    }

    pub fn set_width(&mut self, width: f64) {
        self.width = width;
        self.update_selected(|a| a.width = width);
    }

    pub fn set_filled(&mut self, filled: bool) {
        self.filled = filled;
        self.update_selected(|a| {
            if let Shape::Rectangle { filled: f, .. } | Shape::Ellipse { filled: f, .. } =
                &mut a.shape
            {
                *f = filled;
            }
        });
    }

    pub fn set_arrow_style(&mut self, style: ArrowStyle) {
        self.arrow_style = style;
        self.update_selected(|a| {
            if let Shape::Arrow { style: s, .. } = &mut a.shape {
                *s = style;
            }
        });
    }

    fn update_selected(&mut self, f: impl FnOnce(&mut Annotation)) {
        let Some(id) = self.selection else {
            return;
        };
        let Some(before) = self.doc.get(id).cloned() else {
            return;
        };
        let mut after = before.clone();
        f(&mut after);
        if after != before {
            self.change();
            if let Some(a) = self.doc.get_mut(id) {
                *a = after;
            }
        }
    }

    /// Covers each rect (image points) with an opaque box, as one undo
    /// step. Opaque rather than pixelated: pixelated text can sometimes be
    /// reconstructed. Returns how many were added.
    pub fn add_redactions(&mut self, rects: &[Rect]) -> usize {
        let rects = redaction_boxes(rects, self.extent());
        if rects.is_empty() {
            return 0;
        }
        self.change();
        for rect in &rects {
            self.doc.add(
                Shape::Rectangle {
                    rect: *rect,
                    filled: true,
                },
                REDACTION,
                self.width,
            );
        }
        self.selection = None;
        rects.len()
    }

    pub fn delete_selection(&mut self) -> bool {
        let Some(id) = self.selection.take() else {
            return false;
        };
        self.change();
        self.doc.remove(id);
        true
    }

    pub fn undo(&mut self) -> bool {
        self.drag = None;
        let done = self.history.undo(&mut self.doc);
        self.after_history(done)
    }

    pub fn redo(&mut self) -> bool {
        self.drag = None;
        let done = self.history.redo(&mut self.doc);
        self.after_history(done)
    }

    fn after_history(&mut self, done: bool) -> bool {
        if done {
            self.dirty = true;
            if self.selection.is_some_and(|id| self.doc.get(id).is_none()) {
                self.selection = None;
            }
            if self.tool == Tool::Crop {
                self.crop_draft = Some(self.doc.crop.unwrap_or(self.extent()));
            }
        }
        done
    }

    /// Sets the text of a new (`existing` = None) or existing annotation.
    /// Empty text removes it.
    pub fn commit_text(
        &mut self,
        existing: Option<u64>,
        origin: Point,
        text: &str,
        size: (f64, f64),
    ) {
        let bounds = Rect::new(origin.x, origin.y, size.0, size.1);
        match existing {
            Some(id) => {
                let Some(before) = self.doc.get(id).cloned() else {
                    return;
                };
                if text.is_empty() {
                    self.change();
                    self.doc.remove(id);
                    self.selection = None;
                    return;
                }
                let mut after = before.clone();
                if let Shape::Text {
                    text: t, bounds: b, ..
                } = &mut after.shape
                {
                    *t = text.to_string();
                    *b = bounds;
                }
                if after != before {
                    self.change();
                    if let Some(a) = self.doc.get_mut(id) {
                        *a = after;
                    }
                }
                self.selection = Some(id);
            }
            None if !text.is_empty() => {
                self.change();
                let shape = Shape::Text {
                    origin,
                    text: text.to_string(),
                    size: self.text_size,
                    style: self.text_style,
                    bounds,
                };
                self.selection = Some(self.doc.add(shape, self.color, self.width));
            }
            None => {}
        }
    }

    fn handle_at(&self, p: Point, tolerance: f64) -> Option<(u64, Handle)> {
        let id = self.selection?;
        let a = self.doc.get(id)?;
        a.handles()
            .into_iter()
            .find(|(_, h)| (h.x - p.x).hypot(h.y - p.y) <= tolerance * 1.5)
            .map(|(handle, _)| (id, handle))
    }

    pub fn mouse_down(&mut self, p: Point, tolerance: f64) -> Effect {
        if self.tool == Tool::Crop {
            return self.crop_down(p, tolerance);
        }
        if self.tool == Tool::Background {
            return Effect::Redraw;
        }
        // With any tool, the selected annotation can be resized or moved.
        if let Some((id, handle)) = self.handle_at(p, tolerance) {
            self.change();
            self.drag = Some(Drag::Handle { id, handle });
            return Effect::Redraw;
        }
        let hit = self.doc.hit(p, tolerance);
        if let Some(id) = hit
            && (self.tool == Tool::Select || Some(id) == self.selection)
        {
            if self.tool == Tool::Text
                && matches!(self.doc.get(id).map(|a| &a.shape), Some(Shape::Text { .. }))
            {
                return self.edit_existing_text(id);
            }
            self.selection = Some(id);
            self.drag = Some(Drag::Move {
                id,
                last: p,
                moved: false,
            });
            return Effect::Redraw;
        }

        let rect = Rect::new(p.x, p.y, 0.0, 0.0);
        let shape = match self.tool {
            Tool::Select => {
                self.selection = None;
                return Effect::Redraw;
            }
            Tool::Text => {
                if let Some(id) = hit
                    && matches!(self.doc.get(id).map(|a| &a.shape), Some(Shape::Text { .. }))
                {
                    return self.edit_existing_text(id);
                }
                self.selection = None;
                return Effect::EditText {
                    at: p,
                    existing: None,
                };
            }
            Tool::Counter => {
                self.change();
                let number = self.doc.next_counter();
                let id = self
                    .doc
                    .add(Shape::Counter { center: p, number }, self.color, self.width);
                self.selection = Some(id);
                return Effect::Redraw;
            }
            Tool::Arrow => Shape::Arrow {
                start: p,
                end: p,
                style: self.arrow_style,
                bend: None,
            },
            Tool::Measure => Shape::Measure { start: p, end: p },
            Tool::Magnify => Shape::Magnify {
                source: rect,
                callout: rect,
            },
            Tool::Line => Shape::Line { start: p, end: p },
            Tool::Rectangle => Shape::Rectangle {
                rect,
                filled: self.filled,
            },
            Tool::Ellipse => Shape::Ellipse {
                rect,
                filled: self.filled,
            },
            Tool::Pen => Shape::Pen { points: vec![p] },
            Tool::Highlighter => Shape::Highlighter { points: vec![p] },
            Tool::Pixelate => Shape::Pixelate { rect },
            Tool::Blur => Shape::Blur { rect },
            Tool::Spotlight => Shape::Spotlight { rect },
            Tool::Crop | Tool::Background => unreachable!("handled above"),
        };
        self.change();
        let sketchy = self.sketchy
            && matches!(
                shape,
                Shape::Arrow { .. }
                    | Shape::Line { .. }
                    | Shape::Rectangle { .. }
                    | Shape::Ellipse { .. }
            );
        let id = self.doc.add(shape, self.color, self.width);
        if let Some(a) = self.doc.get_mut(id) {
            a.sketchy = sketchy;
        }
        self.selection = None;
        self.drag = Some(Drag::Create { id, anchor: p });
        Effect::Redraw
    }

    /// A click (no drag) with the Measure tool, for the view to measure
    /// from automatically.
    pub fn take_pending_measure(&mut self) -> Option<Point> {
        self.pending_measure.take()
    }

    /// Adds a measurement from `start` to `end`, selected, as one undo step.
    pub fn add_measure(&mut self, start: Point, end: Point) {
        self.change();
        let id = self
            .doc
            .add(Shape::Measure { start, end }, self.color, self.width);
        self.selection = Some(id);
    }

    /// Sets the hand-drawn look for new shapes and the selected one.
    pub fn set_sketchy(&mut self, sketchy: bool) {
        self.sketchy = sketchy;
        if let Some(id) = self.selection
            && self.doc.get(id).is_some_and(|a| {
                a.sketchy != sketchy
                    && matches!(
                        a.shape,
                        Shape::Arrow { .. }
                            | Shape::Line { .. }
                            | Shape::Rectangle { .. }
                            | Shape::Ellipse { .. }
                    )
            })
        {
            self.change();
            if let Some(a) = self.doc.get_mut(id) {
                a.sketchy = sketchy;
            }
        }
    }

    /// The opacity of the selected image layer, if one is selected.
    pub fn selected_opacity(&self) -> Option<f64> {
        match self
            .selection
            .and_then(|id| self.doc.get(id))
            .map(|a| &a.shape)
        {
            Some(Shape::Image { opacity, .. }) => Some(*opacity),
            Some(Shape::Watermark { style, .. }) => Some(style.opacity),
            _ => None,
        }
    }

    /// Sets the selected image layer's opacity, as one undo step.
    pub fn set_opacity(&mut self, value: f64) {
        let value = value.clamp(0.05, 1.0);
        if self
            .selected_opacity()
            .is_some_and(|o| (o - value).abs() > f64::EPSILON)
        {
            self.change();
            match self
                .selection
                .and_then(|id| self.doc.get_mut(id))
                .map(|a| &mut a.shape)
            {
                Some(Shape::Image { opacity, .. }) => *opacity = value,
                Some(Shape::Watermark { style, .. }) => style.opacity = value,
                _ => {}
            }
        }
    }

    fn edit_existing_text(&mut self, id: u64) -> Effect {
        self.selection = Some(id);
        let at = match self.doc.get(id).map(|a| &a.shape) {
            Some(Shape::Text { origin, .. }) => *origin,
            _ => Point::default(),
        };
        Effect::EditText {
            at,
            existing: Some(id),
        }
    }

    /// `constrain`: Shift held (squares, circles, 45-degree lines).
    pub fn mouse_dragged(&mut self, p: Point, constrain: bool) {
        let Some(drag) = self.drag else {
            return;
        };
        match drag {
            Drag::Create { id, anchor } => {
                let extent = self.extent();
                let Some(a) = self.doc.get_mut(id) else {
                    return;
                };
                match &mut a.shape {
                    Shape::Arrow { end, .. } | Shape::Line { end, .. } => {
                        *end = if constrain { snap_angle(anchor, p) } else { p };
                    }
                    Shape::Measure { end, .. } => {
                        *end = if constrain { snap_axis(anchor, p) } else { p };
                    }
                    Shape::Magnify { source, callout } => {
                        let r = (p.x - anchor.x).hypot(p.y - anchor.y).max(1.0);
                        *source = Rect::new(anchor.x - r, anchor.y - r, 2.0 * r, 2.0 * r);
                        *callout = callout_beside(*source, extent);
                    }
                    Shape::Rectangle { rect, .. }
                    | Shape::Ellipse { rect, .. }
                    | Shape::Pixelate { rect }
                    | Shape::Blur { rect }
                    | Shape::Spotlight { rect } => {
                        let corner = if constrain {
                            square_corner(anchor, p)
                        } else {
                            p
                        };
                        *rect = Rect::from_corners(anchor, corner);
                    }
                    Shape::Pen { points } | Shape::Highlighter { points } => {
                        let far_enough = points
                            .last()
                            .is_none_or(|l| (l.x - p.x).hypot(l.y - p.y) >= 1.0);
                        if far_enough {
                            points.push(p);
                        }
                    }
                    _ => {}
                }
            }
            Drag::Move { id, last, moved } => {
                if !moved {
                    self.change();
                }
                if let Some(a) = self.doc.get_mut(id) {
                    a.translate(p.x - last.x, p.y - last.y);
                }
                self.drag = Some(Drag::Move {
                    id,
                    last: p,
                    moved: true,
                });
            }
            Drag::Handle { id, handle } => {
                if let Some(a) = self.doc.get_mut(id) {
                    a.drag_handle(handle, p);
                }
            }
            Drag::CropNew { anchor } => {
                let corner = if constrain {
                    square_corner(anchor, p)
                } else {
                    p
                };
                self.crop_draft =
                    Some(Rect::from_corners(anchor, corner).intersection(&self.extent()));
            }
            Drag::CropMove { last } => {
                if let Some(draft) = self.crop_draft {
                    self.crop_draft = Some(crate::capture::geometry::move_within(
                        draft,
                        p.x - last.x,
                        p.y - last.y,
                        self.extent(),
                    ));
                }
                self.drag = Some(Drag::CropMove { last: p });
            }
            Drag::CropHandle { handle } => {
                if let Some(draft) = self.crop_draft {
                    let mut a = Annotation {
                        id: 0,
                        shape: Shape::Rectangle {
                            rect: draft,
                            filled: false,
                        },
                        color: Color::BLACK,
                        width: 0.0,
                        sketchy: false,
                    };
                    a.drag_handle(handle, p);
                    self.crop_draft = Some(a.bounds().intersection(&self.extent()));
                }
            }
        }
    }

    pub fn mouse_up(&mut self) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        match drag {
            Drag::Create { id, anchor } => {
                if self.highlight_text {
                    self.snap_highlight(id);
                }
                let keep = self.doc.get(id).is_some_and(|a| match &a.shape {
                    Shape::Arrow { start, end, .. }
                    | Shape::Line { start, end }
                    | Shape::Measure { start, end } => {
                        (end.x - start.x).hypot(end.y - start.y) >= 4.0
                    }
                    Shape::Magnify { source, .. } => source.width >= 8.0,
                    Shape::TextHighlight { .. } => true,
                    Shape::Pen { .. } | Shape::Highlighter { .. } => true,
                    _ => a.bounds().width >= 3.0 && a.bounds().height >= 3.0,
                });
                if keep {
                    self.selection = Some(id);
                } else {
                    // A click without a drag draws nothing (with the
                    // Measure tool, it measures automatically).
                    if matches!(
                        self.doc.get(id).map(|a| &a.shape),
                        Some(Shape::Measure { .. })
                    ) {
                        self.pending_measure = Some(anchor);
                    }
                    self.doc.remove(id);
                    self.history.discard_last();
                }
            }
            Drag::Move { moved: false, .. } => {}
            // A crop too small to use goes back to the previous one.
            Drag::CropNew { .. } | Drag::CropHandle { .. }
                if self
                    .crop_draft
                    .is_none_or(|d| d.width < 4.0 || d.height < 4.0) =>
            {
                self.crop_draft = Some(self.doc.crop.unwrap_or(self.extent()));
            }
            _ => {}
        }
    }

    /// Turns a highlighter stroke into markers over the lines of text it
    /// crosses, if it crosses any.
    fn snap_highlight(&mut self, id: u64) {
        let Some(a) = self.doc.get_mut(id) else {
            return;
        };
        if let Shape::Highlighter { points } = &a.shape {
            let rects = snap_to_text(points, &self.text_lines);
            if !rects.is_empty() {
                a.shape = Shape::TextHighlight { rects };
            }
        }
    }

    fn crop_down(&mut self, p: Point, tolerance: f64) -> Effect {
        let draft = self.crop_draft.unwrap_or(self.extent());
        let corners = [
            (Handle::TopLeft, Point::new(draft.x, draft.y)),
            (Handle::TopRight, Point::new(draft.max_x(), draft.y)),
            (Handle::BottomLeft, Point::new(draft.x, draft.max_y())),
            (
                Handle::BottomRight,
                Point::new(draft.max_x(), draft.max_y()),
            ),
        ];
        self.drag = Some(
            match corners
                .iter()
                .find(|(_, c)| (c.x - p.x).hypot(c.y - p.y) <= tolerance * 2.0)
            {
                Some((handle, _)) => Drag::CropHandle { handle: *handle },
                None if draft.contains(p) && draft != self.extent() => Drag::CropMove { last: p },
                None => Drag::CropNew { anchor: p },
            },
        );
        Effect::Redraw
    }
}

/// `p` snapped so the line from `anchor` is a multiple of 45 degrees.
fn snap_angle(anchor: Point, p: Point) -> Point {
    let (dx, dy) = (p.x - anchor.x, p.y - anchor.y);
    let length = dx.hypot(dy);
    let step = std::f64::consts::FRAC_PI_4;
    let angle = (dy.atan2(dx) / step).round() * step;
    Point::new(
        anchor.x + length * angle.cos(),
        anchor.y + length * angle.sin(),
    )
}

/// `p` moved onto the horizontal or vertical line through `anchor`,
/// whichever is closer.
fn snap_axis(anchor: Point, p: Point) -> Point {
    if (p.x - anchor.x).abs() >= (p.y - anchor.y).abs() {
        Point::new(p.x, anchor.y)
    } else {
        Point::new(anchor.x, p.y)
    }
}

/// Where a new magnify callout goes: twice the spot's size, beside it on
/// the right (or on the left when there is no room), within `extent`.
pub fn callout_beside(source: Rect, extent: Rect) -> Rect {
    let size = (source.width * 2.0).max(48.0);
    let gap = 24.0;
    let y = (source.y + source.height / 2.0 - size / 2.0)
        .min(extent.max_y() - size)
        .max(extent.y);
    let right = source.max_x() + gap;
    let x = if right + size <= extent.max_x() || source.x - gap - size < extent.x {
        right
    } else {
        source.x - gap - size
    };
    Rect::new(x, y, size, size)
}

/// Markers over the parts of `lines` (text line boxes) that the stroke
/// through `points` crosses: from the first to the last point inside each
/// line, at the line's height.
pub fn snap_to_text(points: &[Point], lines: &[Rect]) -> Vec<Rect> {
    // Fast strokes have few points: fill in every 2 points along them.
    let mut dense: Vec<Point> = points.first().copied().into_iter().collect();
    for w in points.windows(2) {
        let (a, b) = (w[0], w[1]);
        let steps = ((b.x - a.x).hypot(b.y - a.y) / 2.0).ceil().max(1.0) as usize;
        dense.extend((1..=steps).map(|i| {
            let t = i as f64 / steps as f64;
            Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
        }));
    }
    let points = &dense;
    lines
        .iter()
        .filter_map(|line| {
            let slack = line.height * 0.25;
            let inside: Vec<f64> = points
                .iter()
                .filter(|p| {
                    p.y >= line.y - slack
                        && p.y <= line.max_y() + slack
                        && p.x >= line.x - slack
                        && p.x <= line.max_x() + slack
                })
                .map(|p| p.x.clamp(line.x, line.max_x()))
                .collect();
            let lo = inside.iter().copied().fold(f64::INFINITY, f64::min);
            let hi = inside.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            (hi - lo >= 4.0).then(|| Rect::new(lo - 2.0, line.y, hi - lo + 4.0, line.height))
        })
        .collect()
}

fn square_corner(anchor: Point, p: Point) -> Point {
    let side = (p.x - anchor.x).abs().max((p.y - anchor.y).abs());
    Point::new(
        anchor.x + side.copysign(p.x - anchor.x),
        anchor.y + side.copysign(p.y - anchor.y),
    )
}

/// The boxes that cover text found at `rects` (image points), padded so
/// no edge of a tall glyph shows and kept inside `image`.
pub fn redaction_boxes(rects: &[Rect], image: Rect) -> Vec<Rect> {
    rects
        .iter()
        .map(|r| {
            let pad = (r.height * 0.15).max(2.0);
            Rect::new(
                r.x - pad,
                r.y - pad,
                r.width + 2.0 * pad,
                r.height + 2.0 * pad,
            )
            .intersection(&image)
        })
        .filter(|r| r.width > 0.0 && r.height > 0.0)
        .collect()
}

/// The color redaction boxes are filled with.
pub const REDACTION: Color = Color::rgb(0.1, 0.1, 0.1);

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        Session::new(Document::default(), 800.0, 600.0)
    }

    fn drag(s: &mut Session, from: (f64, f64), to: (f64, f64)) -> Effect {
        let effect = s.mouse_down(Point::new(from.0, from.1), 4.0);
        s.mouse_dragged(Point::new(to.0, to.1), false);
        s.mouse_up();
        effect
    }

    #[test]
    fn drawing_creates_and_selects() {
        let mut s = session();
        s.set_tool(Tool::Rectangle);
        drag(&mut s, (10.0, 10.0), (110.0, 60.0));
        assert_eq!(s.doc.annotations.len(), 1);
        let a = &s.doc.annotations[0];
        assert_eq!(a.bounds(), Rect::new(10.0, 10.0, 100.0, 50.0));
        assert_eq!(s.selection, Some(a.id));
        assert!(s.history.can_undo());
    }

    #[test]
    fn a_click_draws_nothing_and_leaves_no_undo_step() {
        let mut s = session();
        s.set_tool(Tool::Arrow);
        drag(&mut s, (10.0, 10.0), (11.0, 11.0));
        assert!(s.doc.annotations.is_empty());
        assert!(!s.history.can_undo());
    }

    #[test]
    fn shift_constrains() {
        let mut s = session();
        s.set_tool(Tool::Line);
        s.mouse_down(Point::new(0.0, 0.0), 4.0);
        s.mouse_dragged(Point::new(100.0, 8.0), true);
        s.mouse_up();
        match s.doc.annotations[0].shape {
            Shape::Line { end, .. } => assert!(end.y.abs() < 1e-9),
            _ => panic!(),
        }
    }

    fn watermark_style() -> crate::watermark::Style {
        crate::watermark::Style {
            size: 25.0,
            margin: 5.0,
            ..crate::watermark::Style::default()
        }
    }

    #[test]
    fn watermarks_anchor_drag_free_and_resize() {
        use crate::watermark::Position;
        let mut s = session();
        let frame = Rect::new(0.0, 0.0, 800.0, 600.0);
        // A 4:1 mark at 25% of 800 = 200 × 50, 5% of 600 = 30 from the edges.
        let id = s.add_watermark(watermark_style(), picture(40, 10, 1.0), frame);
        assert_eq!(s.selection, Some(id));
        assert_eq!(
            s.doc.get(id).unwrap().bounds(),
            Rect::new(570.0, 520.0, 200.0, 50.0)
        );
        assert_eq!(s.watermark_position(), Some(Some(Position::BottomRight)));

        // Dragging frees it; it stays where it was dropped.
        s.set_tool(Tool::Select);
        drag(&mut s, (600.0, 540.0), (400.0, 340.0));
        assert_eq!(
            s.doc.get(id).unwrap().bounds(),
            Rect::new(370.0, 320.0, 200.0, 50.0)
        );
        assert_eq!(s.watermark_position(), Some(None));
        // A crop keeps it at the same share of the picture.
        s.doc.place_watermarks(Rect::new(0.0, 0.0, 400.0, 300.0));
        assert_eq!(
            s.doc.get(id).unwrap().bounds(),
            Rect::new(185.0, 160.0, 100.0, 25.0)
        );
        s.doc.place_watermarks(frame);

        // Its corner handle sets the size, as a share of the width.
        drag(&mut s, (570.0, 370.0), (670.0, 370.0));
        match &s.doc.get(id).unwrap().shape {
            Shape::Watermark { style, .. } => assert!((style.size - 37.5).abs() < 1e-9),
            other => panic!("{other:?}"),
        }

        // Anchoring again, then undoing back to the free spot.
        s.anchor_watermark(Position::TopLeft);
        assert_eq!(
            s.doc.get(id).unwrap().bounds(),
            Rect::new(30.0, 30.0, 300.0, 75.0)
        );
        assert!(s.undo());
        assert_eq!(s.watermark_position(), Some(None));

        // Only one watermark; removing it is one undo step.
        s.add_watermark(watermark_style(), picture(40, 10, 1.0), frame);
        assert_eq!(
            s.doc
                .annotations
                .iter()
                .filter(|a| matches!(a.shape, Shape::Watermark { .. }))
                .count(),
            1
        );
        s.remove_watermark();
        assert_eq!(s.doc.watermark(), None);
        assert!(s.undo());
        assert!(s.doc.watermark().is_some());
    }

    #[test]
    fn watermarks_survive_saving() {
        let mut doc = Document::default();
        doc.add(
            Shape::Watermark {
                style: watermark_style(),
                picture: picture(40, 10, 1.0),
                free: Some(Point::new(0.25, 0.75)),
                rect: Rect::default(),
                frame: Rect::default(),
            },
            Color::RED,
            4.0,
        );
        let json = serde_json::to_string(&doc).unwrap();
        let mut back: Document = serde_json::from_str(&json).unwrap();
        back.place_watermarks(Rect::new(0.0, 0.0, 800.0, 600.0));
        match &back.annotations[0].shape {
            Shape::Watermark {
                style, free, rect, ..
            } => {
                assert_eq!(style.size, 25.0);
                assert_eq!(*free, Some(Point::new(0.25, 0.75)));
                assert_eq!(*rect, Rect::new(100.0, 425.0, 200.0, 50.0));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn select_and_move_then_undo() {
        let mut s = session();
        s.set_tool(Tool::Rectangle);
        drag(&mut s, (10.0, 10.0), (110.0, 60.0));
        s.set_tool(Tool::Select);
        s.selection = None;
        drag(&mut s, (10.0, 30.0), (30.0, 40.0));
        assert_eq!(
            s.doc.annotations[0].bounds(),
            Rect::new(30.0, 20.0, 100.0, 50.0)
        );
        assert!(s.undo());
        assert_eq!(
            s.doc.annotations[0].bounds(),
            Rect::new(10.0, 10.0, 100.0, 50.0)
        );
    }

    #[test]
    fn handles_resize_the_selection_with_any_tool() {
        let mut s = session();
        s.set_tool(Tool::Rectangle);
        drag(&mut s, (10.0, 10.0), (110.0, 60.0));
        // Still the rectangle tool: dragging the corner resizes instead of
        // drawing a new rectangle.
        drag(&mut s, (110.0, 60.0), (210.0, 160.0));
        assert_eq!(s.doc.annotations.len(), 1);
        assert_eq!(
            s.doc.annotations[0].bounds(),
            Rect::new(10.0, 10.0, 200.0, 150.0)
        );
    }

    #[test]
    fn color_changes_apply_to_the_selection() {
        let mut s = session();
        s.set_tool(Tool::Arrow);
        drag(&mut s, (0.0, 0.0), (50.0, 50.0));
        s.set_color(Color::BLACK);
        assert_eq!(s.doc.annotations[0].color, Color::BLACK);
        s.undo();
        assert_eq!(s.doc.annotations[0].color, Color::RED);
    }

    #[test]
    fn counters_increment() {
        let mut s = session();
        s.set_tool(Tool::Counter);
        s.mouse_down(Point::new(10.0, 10.0), 4.0);
        s.mouse_up();
        s.mouse_down(Point::new(100.0, 10.0), 4.0);
        s.mouse_up();
        let numbers: Vec<u32> = s
            .doc
            .annotations
            .iter()
            .filter_map(|a| match a.shape {
                Shape::Counter { number, .. } => Some(number),
                _ => None,
            })
            .collect();
        assert_eq!(numbers, vec![1, 2]);
    }

    #[test]
    fn text_tool_requests_editing() {
        let mut s = session();
        s.set_tool(Tool::Text);
        let effect = s.mouse_down(Point::new(5.0, 5.0), 4.0);
        assert_eq!(
            effect,
            Effect::EditText {
                at: Point::new(5.0, 5.0),
                existing: None
            }
        );
        s.commit_text(None, Point::new(5.0, 5.0), "Hi", (20.0, 30.0));
        let id = s.selection.unwrap();
        // Clicking the text again edits it.
        let effect = s.mouse_down(Point::new(10.0, 10.0), 4.0);
        assert_eq!(
            effect,
            Effect::EditText {
                at: Point::new(5.0, 5.0),
                existing: Some(id)
            }
        );
        s.commit_text(Some(id), Point::new(5.0, 5.0), "", (0.0, 0.0));
        assert!(s.doc.annotations.is_empty());
    }

    #[test]
    fn snaps_highlights_to_text_lines() {
        let lines = [
            Rect::new(10.0, 10.0, 200.0, 20.0),
            Rect::new(10.0, 40.0, 150.0, 20.0),
        ];
        // A stroke across the middle of the first line, from x 50 to 120.
        let stroke: Vec<Point> = (50..=120)
            .step_by(5)
            .map(|x| Point::new(f64::from(x), 22.0))
            .collect();
        assert_eq!(
            snap_to_text(&stroke, &lines),
            vec![Rect::new(48.0, 10.0, 74.0, 20.0)]
        );
        // A diagonal stroke over both lines marks both.
        let both = [
            Point::new(20.0, 15.0),
            Point::new(60.0, 30.0),
            Point::new(100.0, 50.0),
        ];
        assert_eq!(snap_to_text(&both, &lines).len(), 2);
        // Strokes away from text keep their freehand shape.
        assert!(snap_to_text(&[Point::new(300.0, 300.0)], &lines).is_empty());
    }

    #[test]
    fn places_magnify_callouts_beside_the_spot() {
        let extent = Rect::new(0.0, 0.0, 400.0, 300.0);
        let right = callout_beside(Rect::new(50.0, 50.0, 40.0, 40.0), extent);
        assert_eq!(right, Rect::new(114.0, 30.0, 80.0, 80.0));
        // No room on the right: on the left.
        let left = callout_beside(Rect::new(340.0, 200.0, 40.0, 40.0), extent);
        assert_eq!(left.max_x(), 316.0);
        assert!(left.max_y() <= 300.0);
    }

    #[test]
    fn a_click_with_the_measure_tool_asks_to_measure() {
        let mut s = session();
        s.set_tool(Tool::Measure);
        s.mouse_down(Point::new(30.0, 40.0), 4.0);
        s.mouse_up();
        assert!(s.doc.annotations.is_empty());
        assert!(!s.history.can_undo());
        assert_eq!(s.take_pending_measure(), Some(Point::new(30.0, 40.0)));
        assert_eq!(s.take_pending_measure(), None);
        s.add_measure(Point::new(10.0, 40.0), Point::new(90.0, 40.0));
        assert_eq!(s.doc.annotations.len(), 1);
        assert!(s.undo());
    }

    #[test]
    fn bends_arrows_by_their_middle_handle() {
        let mut s = session();
        s.mouse_down(Point::new(10.0, 10.0), 4.0);
        s.mouse_dragged(Point::new(110.0, 10.0), false);
        s.mouse_up();
        let id = s.selection.unwrap();
        // Drag the middle handle (60, 10) down by 40.
        s.mouse_down(Point::new(60.0, 10.0), 4.0);
        s.mouse_dragged(Point::new(60.0, 50.0), false);
        s.mouse_up();
        match &s.doc.get(id).unwrap().shape {
            Shape::Arrow { bend, .. } => assert_eq!(*bend, Some(Point::new(60.0, 90.0))),
            other => panic!("{other:?}"),
        }
        let a = s.doc.get(id).unwrap();
        assert!(
            a.hit(Point::new(60.0, 50.0), 2.0),
            "the curve is hit at its middle"
        );
        assert!(
            !a.hit(Point::new(60.0, 10.0), 2.0),
            "the straight line is gone"
        );
    }

    #[test]
    fn redactions_are_padded_clamped_and_one_undo_step() {
        let mut s = session();
        let n = s.add_redactions(&[
            Rect::new(100.0, 100.0, 50.0, 20.0),
            Rect::new(790.0, 590.0, 30.0, 30.0),
        ]);
        assert_eq!(n, 2);
        assert_eq!(
            s.doc.annotations[0].bounds(),
            Rect::new(97.0, 97.0, 56.0, 26.0)
        );
        // Clamped to the image.
        assert_eq!(s.doc.annotations[1].bounds().max_x(), 800.0);
        assert!(s.undo());
        assert!(s.doc.annotations.is_empty());
        assert_eq!(s.add_redactions(&[]), 0);
        assert!(!s.history.can_undo());
    }

    #[test]
    fn crop_draft_applies_when_leaving_the_tool() {
        let mut s = session();
        s.set_tool(Tool::Crop);
        drag(&mut s, (100.0, 100.0), (300.0, 250.0));
        assert_eq!(s.doc.crop, None);
        s.set_tool(Tool::Arrow);
        assert_eq!(s.doc.crop, Some(Rect::new(100.0, 100.0, 200.0, 150.0)));
        // Clamped to the image.
        s.set_tool(Tool::Crop);
        s.reset_crop();
        drag(&mut s, (700.0, 500.0), (900.0, 700.0));
        s.apply_crop();
        assert_eq!(s.doc.crop, Some(Rect::new(700.0, 500.0, 100.0, 100.0)));
        // Each applied crop is one undo step.
        assert!(s.undo());
        assert_eq!(s.doc.crop, Some(Rect::new(100.0, 100.0, 200.0, 150.0)));
        assert!(s.undo());
        assert_eq!(s.doc.crop, None);
    }

    fn picture(w: usize, h: usize, scale: f64) -> Picture {
        let pixels = vec![0xff33_6699u32; w * h];
        let cg = crate::capture::image::from_pixels(w, h, &pixels).unwrap();
        Picture::new(cg, scale, crate::editor::picture::Origin::Pixels)
    }

    #[test]
    fn pasted_images_are_centered_shrunk_and_undoable() {
        let mut s = session();
        // 2000 x 1000 pixels at 2x is 1000 x 500 points: shrunk to 80% of
        // the 800-point-wide image.
        let id = s.insert_picture(picture(2000, 1000, 2.0), None);
        let rect = s.doc.get(id).unwrap().bounds();
        assert_eq!(rect, Rect::new(80.0, 140.0, 640.0, 320.0));
        assert_eq!(s.selection, Some(id));
        assert_eq!(s.extent(), Rect::new(0.0, 0.0, 800.0, 600.0));
        assert!(s.undo());
        assert!(s.doc.annotations.is_empty());
    }

    #[test]
    fn combining_grows_the_canvas() {
        let mut s = session();
        s.append_picture(picture(400, 300, 1.0), false);
        assert_eq!(s.extent(), Rect::new(0.0, 0.0, 1200.0, 600.0));
        s.append_picture(picture(100, 100, 1.0), true);
        assert_eq!(s.extent(), Rect::new(0.0, 0.0, 1200.0, 700.0));
        let layout = s.doc.layout(800.0, 600.0);
        assert_eq!((layout.width, layout.height), (1200.0, 700.0));
    }

    #[test]
    fn image_layers_resize_keeping_their_shape() {
        let mut s = session();
        let id = s.insert_picture(picture(200, 100, 1.0), Some(Point::new(100.0, 50.0)));
        let a = s.doc.get_mut(id).unwrap();
        assert_eq!(a.bounds(), Rect::new(0.0, 0.0, 200.0, 100.0));
        // Dragging the bottom-right corner mostly sideways doubles both.
        a.drag_handle(Handle::BottomRight, Point::new(400.0, 120.0));
        assert_eq!(a.bounds(), Rect::new(0.0, 0.0, 400.0, 200.0));
        // Past the fixed corner: flips to the other side, same shape.
        a.drag_handle(Handle::BottomRight, Point::new(-100.0, 10.0));
        assert_eq!(a.bounds(), Rect::new(-100.0, 0.0, 100.0, 50.0));
    }

    #[test]
    fn pictures_survive_saving() {
        let mut s = session();
        s.insert_picture(picture(20, 10, 2.0), None);
        let json = serde_json::to_string(&s.doc).unwrap();
        let back: Document = serde_json::from_str(&json).unwrap();
        let Some(Shape::Image { rect, picture, .. }) = back.annotations.first().map(|a| &a.shape)
        else {
            panic!("no image layer");
        };
        assert_eq!(picture.size(), (10.0, 5.0));
        assert_eq!(*rect, s.doc.annotations[0].bounds());
    }
}
