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
    context: Retained<CIContext>,
    srgb: Option<CFRetained<CGColorSpace>>,
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
    ) -> Result<Self, String> {
        let scene = Scene::new(events, project, video);
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
        Ok(Self {
            scene,
            output,
            background,
            cursors,
            click: click_sprite(true).ok_or("could not draw the click effect")?,
            ring: click_sprite(false).ok_or("could not draw the click effect")?,
            context,
            // SAFETY: reading a CoreGraphics constant.
            srgb: CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB })),
        })
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
    pub fn compose(&self, source: &CIImage, t: f64) -> Retained<CIImage> {
        let layout = self.scene.layout;
        let frame = self.scene.frame(t);
        // SAFETY: Core Image calls on immutable images.
        unsafe {
            let spot = self.ci_rect(layout.video);
            let video = source.imageByApplyingTransform(translate(spot.origin.x, spot.origin.y));
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

    /// Renders the frame at `t` from `source` into `target`.
    pub fn render(&self, source: &CVPixelBuffer, target: &CVPixelBuffer, t: f64) {
        // SAFETY: valid pixel buffers; Core Image renders on the GPU.
        unsafe {
            let input = CIImage::imageWithCVPixelBuffer(source);
            let frame = self.compose(&input, t);
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

fn translate(x: f64, y: f64) -> CGAffineTransform {
    CGAffineTransform {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        tx: x,
        ty: y,
    }
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
            let id: Retained<NSValue> = Retained::into_super(NSNumber::new_i32(self.ivars().track));
            Some(NSArray::from_retained_slice(&[id]))
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
    ) -> Retained<Self> {
        let this = Self::alloc().set_ivars(InstructionIvars {
            renderer,
            range,
            track,
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
        ivars.renderer.render(&source, &target, t);
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
