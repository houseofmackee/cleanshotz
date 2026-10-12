//! Draws Studio frames: a custom AVFoundation video compositor that renders
//! each frame with Core Image (on the GPU). The preview and the export use
//! the same compositor, so they match.
//!
//! The background, its shadow and the rounded spot for the video are
//! drawn once by the image editor's renderer (so they look exactly like
//! image backgrounds). Each frame then puts the video into that spot with
//! rounded corners and draws the cursor and click effects on top.

use std::path::Path;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, ProtocolObject};
use objc2::{AllocAnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_av_foundation::{
    AVAsynchronousVideoCompositionRequest, AVVideoCompositing,
    AVVideoCompositionInstructionProtocol, AVVideoCompositionRenderContext,
};
use objc2_core_foundation::{CFRetained, CGAffineTransform, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGColorSpace, CGContext, CGImage, kCGColorSpaceSRGB};
use objc2_core_image::{CIColor, CIContext, CIImage, CIVector};
use objc2_core_media::{CMPersistentTrackID, CMTimeRange};
use objc2_core_video::CVPixelBuffer;
use objc2_foundation::{
    NSArray, NSDictionary, NSError, NSNumber, NSObjectProtocol, NSString, NSValue,
};

use super::events::Events;
use super::project::Project;
use super::scene::{ClickDraw, CursorDraw, Scene};
use crate::capture::geometry::{Point, Rect};
use crate::capture::image;

/// A cursor image ready to draw.
struct Sprite {
    image: Retained<CIImage>,
    /// Pixels per point of the image.
    density: f64,
    hotspot: Point,
}

/// Everything needed to draw any frame of one recording with one set of
/// edits. Immutable once built, so the compositor can use it from
/// AVFoundation's threads.
pub struct Renderer {
    scene: Scene,
    output: (usize, usize),
    background: Option<Retained<CIImage>>,
    cursors: Vec<Option<Sprite>>,
    click: Retained<CIImage>,
    ring: Retained<CIImage>,
    /// One pill per key label, at [`PILL_FONT`] pixels.
    pills: Vec<Option<Retained<CIImage>>>,
    context: Retained<CIContext>,
    srgb: Option<CFRetained<CGColorSpace>>,
    /// The watermark's mark and how to place it.
    watermark: Option<(Retained<CIImage>, crate::watermark::Style)>,
}

// SAFETY: Core Image images and contexts are immutable and documented as
// safe to use from several threads; the rest is plain data.
unsafe impl Send for Renderer {}
unsafe impl Sync for Renderer {}

/// The click effect's sprite size in pixels.
const CLICK_SPRITE: f64 = 256.0;

impl Renderer {
    /// Builds the renderer for a recording in `package` whose screen video
    /// is `video` pixels. Main thread (draws the background with AppKit).
    pub fn new(
        package: &Path,
        events: &Events,
        project: &Project,
        video: (f64, f64),
        duration: f64,
    ) -> Result<Self, String> {
        let scene = Scene::new(events, project, video, duration);
        let pills = scene.key_labels.clone();
        let layout = scene.layout;
        let output = project.output.fit(layout.canvas.0, layout.canvas.1);
        let background = match &project.background {
            Some(bg) => Some(background_image(bg, video, scene.scale)?),
            None => None,
        };
        let cursors = events
            .cursor_images
            .iter()
            .map(|c| {
                let cg = image::load(&package.join(&c.file))?;
                let (pw, _) = image::pixel_size(&cg);
                // SAFETY: a valid image.
                let image = unsafe { CIImage::imageWithCGImage(&cg) };
                Some(Sprite {
                    image,
                    density: pw as f64 / c.width.max(1.0),
                    hotspot: c.hotspot,
                })
            })
            .collect();
        // SAFETY: a context with default options (GPU).
        let context = unsafe { CIContext::contextWithOptions(None) };
        let watermark = project.watermark.as_ref().and_then(|style| {
            // The copy in the package when the logo file is gone.
            let mark = crate::watermark::mark(style)
                .ok()
                .or_else(|| image::load(&package.join(super::package::WATERMARK)));
            if mark.is_none() {
                tracing::warn!("the watermark could not be drawn");
            }
            // SAFETY: a valid image.
            mark.map(|cg| (unsafe { CIImage::imageWithCGImage(&cg) }, style.clone()))
        });
        Ok(Self {
            scene,
            output,
            background,
            cursors,
            click: click_sprite(true).ok_or("could not draw the click effect")?,
            ring: click_sprite(false).ok_or("could not draw the click effect")?,
            pills: pills
                .iter()
                .map(|label| {
                    let cg = pill_image(label)?;
                    // SAFETY: a valid image.
                    Some(unsafe { CIImage::imageWithCGImage(&cg) })
                })
                .collect(),
            context,
            // SAFETY: reading a CoreGraphics constant.
            srgb: CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB })),
            watermark,
        })
    }

    /// The view at `t`.
    pub fn view(&self, t: f64) -> super::zoom::View {
        self.scene.view(t)
    }

    /// The rendered frame size in pixels.
    pub fn output(&self) -> (usize, usize) {
        self.output
    }

    /// Converts a top-left canvas rect to Core Image's bottom-left one.
    fn ci_rect(&self, r: Rect) -> CGRect {
        let h = self.scene.layout.canvas.1;
        CGRect::new(
            CGPoint::new(r.x, h - r.y - r.height),
            CGSize::new(r.width, r.height),
        )
    }

    /// The frame at `t` seconds, from the screen video's frame `source`.
    pub fn compose(&self, source: &CIImage, camera: Option<&CIImage>, t: f64) -> Retained<CIImage> {
        let layout = self.scene.layout;
        let frame = self.scene.frame(t);
        // SAFETY: Core Image calls on immutable images.
        unsafe {
            let spot = self.ci_rect(layout.video);
            let video = source.imageByApplyingTransform(self.video_transform(frame.view, spot));
            let video = if self.scene.motion_blur {
                self.blur_motion(&video, &frame)
            } else {
                video
            };
            let video = video.imageByCroppingToRect(spot);
            let video = if layout.radius > 0.0 {
                rounded(&video, spot, layout.radius)
            } else {
                video
            };
            let mut image = match &self.background {
                Some(bg) => video.imageByCompositingOverImage(bg),
                None => video,
            };
            for click in &frame.clicks {
                image = self.draw_click(click).imageByCompositingOverImage(&image);
            }
            if let Some(cursor) = &frame.cursor
                && let Some(sprite) = self.draw_cursor(cursor)
            {
                image = sprite.imageByCompositingOverImage(&image);
            }
            if let Some((label, opacity)) = frame.key
                && let Some(pill) = self.draw_key(label, opacity)
            {
                image = pill.imageByCompositingOverImage(&image);
            }
            if self.scene.camera_style.visible
                && let Some(camera) = camera
            {
                image = self.draw_camera(camera).imageByCompositingOverImage(&image);
            }
            if let Some((mark, style)) = &self.watermark {
                image = self
                    .draw_watermark(mark, style)
                    .imageByCompositingOverImage(&image);
            }
            let canvas = CGRect::new(
                CGPoint::new(0.0, 0.0),
                CGSize::new(layout.canvas.0, layout.canvas.1),
            );
            let image = image.imageByCroppingToRect(canvas);
            let (ow, oh) = (self.output.0 as f64, self.output.1 as f64);
            if (ow - layout.canvas.0).abs() < 0.5 && (oh - layout.canvas.1).abs() < 0.5 {
                image
            } else {
                image
                    .imageByApplyingTransform_highQualityDownsample(
                        scale(ow / layout.canvas.0, oh / layout.canvas.1),
                        true,
                    )
                    .imageByCroppingToRect(CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(ow, oh)))
            }
        }
    }

    /// Maps the screen video (Core Image coordinates, pixels) so the part
    /// in `view` fills `spot`.
    fn video_transform(&self, view: super::zoom::View, spot: CGRect) -> CGAffineTransform {
        let (vw, vh) = (spot.size.width, spot.size.height);
        let s = self.scene.scale;
        let z = view.level;
        // The visible part's left and bottom edges, in video pixels with
        // Core Image's bottom-left origin.
        let left = view.center.x * s - vw / z / 2.0;
        let bottom = vh - (view.center.y * s + vh / z / 2.0);
        CGAffineTransform {
            a: z,
            b: 0.0,
            c: 0.0,
            d: z,
            tx: spot.origin.x - left * z,
            ty: spot.origin.y - bottom * z,
        }
    }

    /// Blurs the video along the view's movement since the last frame
    /// (panning) and toward its center (zooming).
    fn blur_motion(&self, video: &CIImage, frame: &super::scene::Frame) -> Retained<CIImage> {
        let spot = self.ci_rect(self.scene.layout.video);
        let now = self.video_transform(frame.view, spot);
        let before = self.video_transform(frame.previous, spot);
        // How far the middle of the picture moved, in canvas pixels.
        let (mx, my) = (
            spot.origin.x + spot.size.width / 2.0,
            spot.origin.y + spot.size.height / 2.0,
        );
        let (ax, ay) = ((mx - now.tx) / now.a, (my - now.ty) / now.d);
        let moved = (
            before.a * ax + before.tx - mx,
            before.d * ay + before.ty - my,
        );
        let pan = moved.0.hypot(moved.1);
        let zoom = (frame.view.level - frame.previous.level).abs() * 60.0;
        // SAFETY: built-in filters with valid parameters.
        unsafe {
            let mut image = video.retain();
            if pan > 1.5 {
                image = image.imageByApplyingFilter_withInputParameters(
                    &NSString::from_str("CIMotionBlur"),
                    &params(&[
                        (
                            "inputRadius",
                            NSNumber::new_f64((pan * 0.8).min(30.0)).into(),
                        ),
                        (
                            "inputAngle",
                            NSNumber::new_f64(moved.1.atan2(moved.0)).into(),
                        ),
                    ]),
                );
            }
            if zoom > 0.05 {
                let center = CIVector::vectorWithX_Y(mx, my);
                image = image.imageByApplyingFilter_withInputParameters(
                    &NSString::from_str("CIZoomBlur"),
                    &params(&[
                        ("inputCenter", center.into()),
                        (
                            "inputAmount",
                            NSNumber::new_f64((zoom * 6.0).min(20.0)).into(),
                        ),
                    ]),
                );
            }
            image
        }
    }

    fn draw_cursor(&self, cursor: &CursorDraw) -> Option<Retained<CIImage>> {
        let sprite = self.cursors.get(cursor.image as usize)?.as_ref()?;
        let h = self.scene.layout.canvas.1;
        let s = cursor.scale / sprite.density;
        // SAFETY: Core Image calls on immutable images.
        unsafe {
            let extent = sprite.image.extent();
            let (w, ih) = (extent.size.width * s, extent.size.height * s);
            let left = cursor.tip.x - sprite.hotspot.x * cursor.scale;
            let top = cursor.tip.y - sprite.hotspot.y * cursor.scale;
            let placed = sprite.image.imageByApplyingTransform(CGAffineTransform {
                a: s,
                b: 0.0,
                c: 0.0,
                d: s,
                tx: left,
                ty: h - top - ih,
            });
            let _ = w;
            Some(fade(&placed, cursor.opacity))
        }
    }

    fn draw_click(&self, click: &ClickDraw) -> Retained<CIImage> {
        let h = self.scene.layout.canvas.1;
        let ring = self.scene.style.clicks == super::project::ClickEffect::Ring;
        // A ripple grows from the click; a ring closes in on it.
        let (sprite, diameter) = if ring {
            (
                &self.ring,
                (56.0 - 36.0 * ease_out(click.progress)) * click.scale,
            )
        } else {
            (
                &self.click,
                (14.0 + 40.0 * ease_out(click.progress)) * click.scale,
            )
        };
        let s = diameter / CLICK_SPRITE;
        // SAFETY: Core Image calls on immutable images.
        unsafe {
            let placed = sprite.imageByApplyingTransform(CGAffineTransform {
                a: s,
                b: 0.0,
                c: 0.0,
                d: s,
                tx: click.at.x - diameter / 2.0,
                ty: h - click.at.y - diameter / 2.0,
            });
            fade(&placed, (1.0 - click.progress).powf(1.5))
        }
    }

    /// A pressed key's pill near the bottom (or top) of the frame.
    fn draw_key(&self, label: usize, opacity: f64) -> Option<Retained<CIImage>> {
        let pill = self.pills.get(label)?.as_ref()?;
        let (w, h) = self.scene.layout.canvas;
        // SAFETY: Core Image calls on immutable images.
        unsafe {
            let extent = pill.extent();
            let height = (h * 0.065 * self.scene.keys.size.clamp(0.6, 2.0)).max(24.0);
            let s = height / extent.size.height;
            let width = extent.size.width * s;
            let margin = h * 0.06;
            let y = if self.scene.keys.top {
                h - margin - height
            } else {
                margin
            };
            let placed = pill.imageByApplyingTransform(CGAffineTransform {
                a: s,
                b: 0.0,
                c: 0.0,
                d: s,
                tx: ((w - width) / 2.0).round(),
                ty: y.round(),
            });
            Some(fade(&placed, opacity))
        }
    }

    /// The watermark at its place in the frame, with its opacity.
    fn draw_watermark(&self, mark: &CIImage, style: &crate::watermark::Style) -> Retained<CIImage> {
        let (w, h) = self.scene.layout.canvas;
        // SAFETY: Core Image calls on immutable images.
        unsafe {
            let extent = mark.extent();
            let rect = crate::watermark::place(
                Rect::new(0.0, 0.0, w, h),
                (extent.size.width, extent.size.height),
                style,
                None,
            );
            let r = self.ci_rect(rect);
            let s = r.size.width / extent.size.width.max(1.0);
            let placed = mark.imageByApplyingTransform(CGAffineTransform {
                a: s,
                b: 0.0,
                c: 0.0,
                d: s,
                tx: r.origin.x - extent.origin.x * s,
                ty: r.origin.y - extent.origin.y * s,
            });
            fade(&placed, style.opacity)
        }
    }

    /// The camera, filling its square in the chosen shape, with a white
    /// edge and a soft shadow.
    fn draw_camera(&self, camera: &CIImage) -> Retained<CIImage> {
        let rect = self.ci_rect(self.scene.camera_rect());
        let side = rect.size.width;
        let radius = match self.scene.camera_style.shape {
            crate::config::CameraShape::Circle => side / 2.0,
            crate::config::CameraShape::Rounded => side * 0.18,
        };
        // SAFETY: Core Image calls on immutable images.
        unsafe {
            let extent = camera.extent();
            let s = side / extent.size.width.min(extent.size.height).max(1.0);
            let (cw, ch) = (extent.size.width * s, extent.size.height * s);
            let filled = camera
                .imageByApplyingTransform(CGAffineTransform {
                    a: s,
                    b: 0.0,
                    c: 0.0,
                    d: s,
                    tx: rect.origin.x - (cw - side) / 2.0 - extent.origin.x * s,
                    ty: rect.origin.y - (ch - side) / 2.0 - extent.origin.y * s,
                })
                .imageByCroppingToRect(rect);
            let face = rounded(&filled, rect, radius);
            let edge = 3.0_f64.max(side * 0.015);
            let ring_rect = CGRect::new(
                CGPoint::new(rect.origin.x - edge, rect.origin.y - edge),
                CGSize::new(side + 2.0 * edge, side + 2.0 * edge),
            );
            let mut image = face;
            if let Some(ring) = shape(ring_rect, radius + edge, 1.0, 1.0, 1.0, 0.95) {
                image = image.imageByCompositingOverImage(&ring);
                let shadow_rect = CGRect::new(
                    CGPoint::new(ring_rect.origin.x, ring_rect.origin.y - side * 0.03),
                    ring_rect.size,
                );
                if let Some(shadow) = shape(shadow_rect, radius + edge, 0.0, 0.0, 0.0, 0.35) {
                    let soft = shadow.imageByApplyingGaussianBlurWithSigma(side * 0.04);
                    image = image.imageByCompositingOverImage(&soft);
                }
            }
            image
        }
    }

    /// Renders the frame at `t` from `source` (and the camera's frame, if
    /// there is one) into `target`.
    pub fn render(
        &self,
        source: &CVPixelBuffer,
        camera: Option<&CVPixelBuffer>,
        target: &CVPixelBuffer,
        t: f64,
    ) {
        // SAFETY: valid pixel buffers; Core Image renders on the GPU.
        unsafe {
            let input = CIImage::imageWithCVPixelBuffer(source);
            let camera = camera.map(|c| CIImage::imageWithCVPixelBuffer(c));
            let frame = self.compose(&input, camera.as_deref(), t);
            self.context.render_toCVPixelBuffer_bounds_colorSpace(
                &frame,
                target,
                CGRect::new(
                    CGPoint::new(0.0, 0.0),
                    CGSize::new(self.output.0 as f64, self.output.1 as f64),
                ),
                self.srgb.as_deref(),
            );
        }
    }
}

fn ease_out(x: f64) -> f64 {
    1.0 - (1.0 - x.clamp(0.0, 1.0)).powi(3)
}

fn scale(x: f64, y: f64) -> CGAffineTransform {
    CGAffineTransform {
        a: x,
        b: 0.0,
        c: 0.0,
        d: y,
        tx: 0.0,
        ty: 0.0,
    }
}

fn params(pairs: &[(&str, Retained<AnyObject>)]) -> Retained<NSDictionary<NSString, AnyObject>> {
    let keys: Vec<Retained<NSString>> = pairs.iter().map(|(k, _)| NSString::from_str(k)).collect();
    let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
    let values: Vec<Retained<AnyObject>> = pairs.iter().map(|(_, v)| v.clone()).collect();
    NSDictionary::from_retained_objects(&key_refs, &values)
}

/// `image` with its alpha multiplied by `opacity`.
fn fade(image: &CIImage, opacity: f64) -> Retained<CIImage> {
    if opacity >= 1.0 {
        return image.retain();
    }
    // SAFETY: a built-in filter with valid parameters.
    unsafe {
        let alpha = CIVector::vectorWithX_Y_Z_W(0.0, 0.0, 0.0, opacity.max(0.0));
        image.imageByApplyingFilter_withInputParameters(
            &NSString::from_str("CIColorMatrix"),
            &params(&[("inputAVector", alpha.into())]),
        )
    }
}

/// `image` clipped to the rounded rect `spot` (Core Image coordinates).
fn rounded(image: &CIImage, spot: CGRect, radius: f64) -> Retained<CIImage> {
    // SAFETY: built-in filters with valid parameters.
    unsafe {
        let Some(generator) = objc2_core_image::CIFilter::filterWithName(&NSString::from_str(
            "CIRoundedRectangleGenerator",
        )) else {
            return image.imageByCroppingToRect(spot);
        };
        let extent = CIVector::vectorWithX_Y_Z_W(
            spot.origin.x,
            spot.origin.y,
            spot.size.width,
            spot.size.height,
        );
        let white = CIColor::colorWithRed_green_blue_alpha(1.0, 1.0, 1.0, 1.0);
        let set = |key: &str, value: &AnyObject| {
            let _: () = msg_send![&generator, setValue: value, forKey: &*NSString::from_str(key)];
        };
        set("inputExtent", &extent);
        set("inputRadius", &NSNumber::new_f64(radius));
        set("inputColor", &white);
        let Some(mask) = generator.outputImage() else {
            return image.imageByCroppingToRect(spot);
        };
        image.imageByApplyingFilter_withInputParameters(
            &NSString::from_str("CIBlendWithMask"),
            &params(&[
                ("inputBackgroundImage", CIImage::emptyImage().into()),
                ("inputMaskImage", mask.into()),
            ]),
        )
    }
}

/// A rounded rect (Core Image coordinates) in one color.
fn shape(rect: CGRect, radius: f64, r: f64, g: f64, b: f64, a: f64) -> Option<Retained<CIImage>> {
    // SAFETY: a built-in generator with valid parameters.
    unsafe {
        let generator = objc2_core_image::CIFilter::filterWithName(&NSString::from_str(
            "CIRoundedRectangleGenerator",
        ))?;
        let set = |key: &str, value: &AnyObject| {
            let _: () = msg_send![&generator, setValue: value, forKey: &*NSString::from_str(key)];
        };
        set(
            "inputExtent",
            &CIVector::vectorWithX_Y_Z_W(
                rect.origin.x,
                rect.origin.y,
                rect.size.width,
                rect.size.height,
            ),
        );
        set("inputRadius", &NSNumber::new_f64(radius));
        set(
            "inputColor",
            &CIColor::colorWithRed_green_blue_alpha(r, g, b, a),
        );
        generator.outputImage()
    }
}

/// Key pills are drawn with text this many pixels high, then scaled.
const PILL_FONT: f64 = 56.0;

/// A dark rounded pill with `label` in white, like the keystroke display.
fn pill_image(label: &str) -> Option<CFRetained<CGImage>> {
    use objc2_app_kit::{NSColor, NSGraphicsContext};

    let text = crate::ui::draw::text_size(label, PILL_FONT, crate::ui::draw::Weight::Semibold);
    let (pad_x, pad_y) = (PILL_FONT * 0.6, PILL_FONT * 0.35);
    let (w, h) = (
        (text.width + 2.0 * pad_x).ceil().max(PILL_FONT * 1.6),
        (text.height + 2.0 * pad_y).ceil(),
    );
    let cg = image::bitmap_context(w as usize, h as usize, None)?;
    // Top-left origin, like the rest of the app's drawing.
    CGContext::translate_ctm(Some(&cg), 0.0, h);
    CGContext::scale_ctm(Some(&cg), 1.0, -1.0);
    let context = NSGraphicsContext::graphicsContextWithCGContext_flipped(&cg, true);
    let previous = NSGraphicsContext::currentContext();
    NSGraphicsContext::setCurrentContext(Some(&context));
    let pill = Rect::new(0.0, 0.0, w, h);
    crate::ui::draw::fill_rounded(pill, h / 2.0, &NSColor::colorWithWhite_alpha(0.1, 0.88));
    crate::ui::draw::draw_text(
        label,
        Point::new(
            ((w - text.width) / 2.0).round(),
            ((h - text.height) / 2.0).round(),
        ),
        PILL_FONT,
        crate::ui::draw::Weight::Semibold,
        &NSColor::whiteColor(),
    );
    context.flushGraphics();
    NSGraphicsContext::setCurrentContext(previous.as_deref());
    objc2_core_graphics::CGBitmapContextCreateImage(Some(&cg))
}

/// The background canvas with its shadow and a dark rounded spot where the
/// video goes, drawn by the image editor.
fn background_image(
    background: &crate::editor::model::Background,
    video: (f64, f64),
    scale: f64,
) -> Result<Retained<CIImage>, String> {
    let (w, h) = (video.0.round() as usize, video.1.round() as usize);
    let placeholder = image::from_pixels(w, h, &vec![0xFF10_1010; w * h])
        .ok_or("could not draw the background")?;
    let base = crate::editor::render::BaseImage::new(placeholder, scale);
    let doc = crate::editor::model::Document {
        background: Some(background.clone()),
        ..Default::default()
    };
    let cg = crate::editor::render::export(&doc, &base).ok_or("could not draw the background")?;
    // SAFETY: a valid image.
    Ok(unsafe { CIImage::imageWithCGImage(&cg) })
}

/// A soft blue dot with a white rim (`filled`), or just a blue ring with
/// a white edge, for clicks.
fn click_sprite(filled: bool) -> Option<Retained<CIImage>> {
    let side = CLICK_SPRITE as usize;
    let context = image::bitmap_context(side, side, None)?;
    let inset = 6.0;
    let rect = CGRect::new(
        CGPoint::new(inset, inset),
        CGSize::new(CLICK_SPRITE - 2.0 * inset, CLICK_SPRITE - 2.0 * inset),
    );
    if filled {
        CGContext::set_rgb_fill_color(Some(&context), 0.25, 0.55, 1.0, 0.45);
        CGContext::fill_ellipse_in_rect(Some(&context), rect);
        CGContext::set_rgb_stroke_color(Some(&context), 1.0, 1.0, 1.0, 0.95);
        CGContext::set_line_width(Some(&context), 8.0);
        CGContext::stroke_ellipse_in_rect(Some(&context), rect);
    } else {
        CGContext::set_rgb_stroke_color(Some(&context), 1.0, 1.0, 1.0, 0.95);
        CGContext::set_line_width(Some(&context), 22.0);
        CGContext::stroke_ellipse_in_rect(Some(&context), rect);
        CGContext::set_rgb_stroke_color(Some(&context), 0.25, 0.55, 1.0, 1.0);
        CGContext::set_line_width(Some(&context), 14.0);
        CGContext::stroke_ellipse_in_rect(Some(&context), rect);
    }
    let cg: CFRetained<CGImage> = objc2_core_graphics::CGBitmapContextCreateImage(Some(&context))?;
    // SAFETY: a valid image.
    Some(unsafe { CIImage::imageWithCGImage(&cg) })
}

pub struct InstructionIvars {
    renderer: Arc<Renderer>,
    range: CMTimeRange,
    track: CMPersistentTrackID,
    /// The camera's track, when the recording has one.
    camera: Option<CMPersistentTrackID>,
}

define_class!(
    /// The one instruction of a Studio video composition: which track to
    /// read and the renderer to draw with.
    // SAFETY: NSObject has no subclassing requirements; no Drop.
    #[unsafe(super(NSObject))]
    #[ivars = InstructionIvars]
    pub struct StudioInstruction;

    unsafe impl NSObjectProtocol for StudioInstruction {}

    unsafe impl AVVideoCompositionInstructionProtocol for StudioInstruction {
        #[unsafe(method(timeRange))]
        fn time_range(&self) -> CMTimeRange {
            self.ivars().range
        }

        #[unsafe(method(enablePostProcessing))]
        fn enable_post_processing(&self) -> bool {
            false
        }

        #[unsafe(method(containsTweening))]
        fn contains_tweening(&self) -> bool {
            true
        }

        #[unsafe(method_id(requiredSourceTrackIDs))]
        fn required_source_track_ids(&self) -> Option<Retained<NSArray<NSValue>>> {
            let ids: Vec<Retained<NSValue>> = std::iter::once(self.ivars().track)
                .chain(self.ivars().camera)
                .map(|id| Retained::into_super(NSNumber::new_i32(id)))
                .collect();
            Some(NSArray::from_retained_slice(&ids))
        }

        #[unsafe(method(passthroughTrackID))]
        fn passthrough_track_id(&self) -> CMPersistentTrackID {
            0
        }
    }
);

impl StudioInstruction {
    pub fn new(
        renderer: Arc<Renderer>,
        range: CMTimeRange,
        track: CMPersistentTrackID,
        camera: Option<CMPersistentTrackID>,
    ) -> Retained<Self> {
        let this = Self::alloc().set_ivars(InstructionIvars {
            renderer,
            range,
            track,
            camera,
        });
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

define_class!(
    /// Renders each frame of a Studio composition. AVFoundation creates
    /// it from the class; everything it needs comes with the instruction.
    // SAFETY: NSObject has no subclassing requirements; no Drop.
    #[unsafe(super(NSObject))]
    pub struct StudioCompositor;

    unsafe impl NSObjectProtocol for StudioCompositor {}

    unsafe impl AVVideoCompositing for StudioCompositor {
        #[unsafe(method_id(sourcePixelBufferAttributes))]
        fn source_pixel_buffer_attributes(
            &self,
        ) -> Option<Retained<NSDictionary<NSString, AnyObject>>> {
            Some(pixel_buffer_attributes())
        }

        #[unsafe(method_id(requiredPixelBufferAttributesForRenderContext))]
        fn required_pixel_buffer_attributes(&self) -> Retained<NSDictionary<NSString, AnyObject>> {
            pixel_buffer_attributes()
        }

        #[unsafe(method(renderContextChanged:))]
        fn render_context_changed(&self, _context: &AVVideoCompositionRenderContext) {}

        #[unsafe(method(startVideoCompositionRequest:))]
        fn start_request(&self, request: &AVAsynchronousVideoCompositionRequest) {
            if let Err(e) = compose_request(request) {
                tracing::warn!(error = %e, "studio frame not rendered");
                let error = NSError::new(-1, &NSString::from_str("CleanShotZStudio"));
                // SAFETY: finishing a request that was not finished yet.
                unsafe { request.finishWithError(&error) };
            }
        }
    }
);

impl StudioCompositor {
    pub fn class_object() -> &'static objc2::runtime::AnyClass {
        Self::class()
    }
}

fn compose_request(request: &AVAsynchronousVideoCompositionRequest) -> Result<(), String> {
    // SAFETY: AVFoundation request methods, called on its queue.
    unsafe {
        let instruction = request.videoCompositionInstruction();
        let object: &AnyObject = instruction.as_ref();
        let instruction = object
            .downcast_ref::<StudioInstruction>()
            .ok_or("not a Studio instruction")?;
        let ivars = instruction.ivars();
        let source = request
            .sourceFrameByTrackID(ivars.track)
            .ok_or("no source frame")?;
        let target = request
            .renderContext()
            .newPixelBuffer()
            .ok_or("no output buffer")?;
        let t = request.compositionTime().seconds();
        // Outside the camera recording's time there is no camera frame.
        let camera = ivars.camera.and_then(|id| request.sourceFrameByTrackID(id));
        ivars
            .renderer
            .render(&source, camera.as_deref(), &target, t);
        request.finishWithComposedVideoFrame(&target);
        Ok(())
    }
}

/// BGRA buffers that Core Image can render into directly (IOSurface).
fn pixel_buffer_attributes() -> Retained<NSDictionary<NSString, AnyObject>> {
    const BGRA: i32 = 0x4247_5241; // 'BGRA'
    let formats: Retained<NSArray<NSNumber>> =
        NSArray::from_retained_slice(&[NSNumber::new_i32(BGRA)]);
    let surface: Retained<NSDictionary<NSString, AnyObject>> = NSDictionary::new();
    params(&[
        ("PixelFormatType", formats.into()),
        ("IOSurfaceProperties", surface.into()),
    ])
}

/// The protocol object AVFoundation wants for the instruction list.
pub fn instruction_object(
    instruction: Retained<StudioInstruction>,
) -> Retained<ProtocolObject<dyn AVVideoCompositionInstructionProtocol>> {
    ProtocolObject::from_retained(instruction)
}
