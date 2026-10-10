//! Scrolling capture: one tall image of content that scrolls.

mod session;
pub mod stitch;

pub use session::start;

/// Moves a value to the main thread (stitched images are CGImages, which
/// are immutable and thread-safe).
struct AssertSend<T>(T);
// SAFETY: see the type's documentation.
unsafe impl<T> Send for AssertSend<T> {}
impl<T> AssertSend<T> {
    fn into_inner(self) -> T {
        self.0
    }
}
