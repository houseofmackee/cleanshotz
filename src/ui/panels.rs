//! Floating panels shared by capture modes: the area border and placing
//! control bars next to an area.

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSBox, NSBoxType, NSColor, NSPanel, NSStatusWindowLevel, NSTitlePosition,
    NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize};

pub fn floating_panel(mtm: MainThreadMarker, frame: NSRect) -> Retained<NSPanel> {
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        frame,
        NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
        NSBackingStoreType::Buffered,
        false,
    );
    // SAFETY: callers keep a strong reference.
    unsafe { panel.setReleasedWhenClosed(false) };
    // At status level: captures and recordings leave our UI at this level out.
    panel.setLevel(NSStatusWindowLevel);
    panel.setOpaque(false);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    panel.setHidesOnDeactivate(false);
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::Stationary,
    );
    panel
}

/// A red outline just outside `area` (Cocoa screen coordinates).
pub fn border_panel(mtm: MainThreadMarker, area: NSRect) -> Retained<NSPanel> {
    let inset = 4.0;
    let frame = NSRect::new(
        NSPoint::new(area.origin.x - inset, area.origin.y - inset),
        NSSize::new(
            area.size.width + 2.0 * inset,
            area.size.height + 2.0 * inset,
        ),
    );
    let panel = floating_panel(mtm, frame);
    panel.setIgnoresMouseEvents(true);
    panel.setHasShadow(false);
    let outline = NSBox::new(mtm);
    outline.setBoxType(NSBoxType::Custom);
    outline.setTitlePosition(NSTitlePosition::NoTitle);
    outline.setBorderWidth(2.0);
    outline.setBorderColor(&NSColor::colorWithSRGBRed_green_blue_alpha(
        1.0, 0.25, 0.2, 0.9,
    ));
    outline.setFillColor(&NSColor::clearColor());
    outline.setCornerRadius(4.0);
    panel.setContentView(Some(&outline));
    panel.orderFrontRegardless();
    panel
}

/// Where the control bar goes: below the area, else above, else inside.
pub fn control_origin(area: NSRect, visible: NSRect, bar: NSSize) -> NSPoint {
    let gap = 12.0;
    let x = (area.origin.x + (area.size.width - bar.width) / 2.0).clamp(
        visible.origin.x + 8.0,
        visible.origin.x + visible.size.width - bar.width - 8.0,
    );
    let below = area.origin.y - gap - bar.height;
    let above = area.origin.y + area.size.height + gap;
    let y = if below >= visible.origin.y + 8.0 {
        below
    } else if above + bar.height <= visible.origin.y + visible.size.height - 8.0 {
        above
    } else {
        area.origin.y.max(visible.origin.y) + 20.0
    };
    NSPoint::new(x.round(), y.round())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BAR: NSSize = NSSize {
        width: 220.0,
        height: 44.0,
    };

    #[test]
    fn control_bar_goes_below_then_above_then_inside() {
        let visible = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1000.0, 800.0));
        let mid = NSRect::new(NSPoint::new(100.0, 300.0), NSSize::new(400.0, 200.0));
        assert_eq!(control_origin(mid, visible, BAR).y, 300.0 - 12.0 - 44.0);
        let low = NSRect::new(NSPoint::new(100.0, 10.0), NSSize::new(400.0, 200.0));
        assert_eq!(control_origin(low, visible, BAR).y, 222.0);
        let full = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1000.0, 800.0));
        assert_eq!(control_origin(full, visible, BAR).y, 20.0);
        // Clamped horizontally.
        let left = NSRect::new(NSPoint::new(-50.0, 300.0), NSSize::new(60.0, 60.0));
        assert_eq!(control_origin(left, visible, BAR).x, 8.0);
    }
}
