//! The annotation editor.

pub mod model;
pub mod picture;
pub mod presets;
pub mod project;
pub mod render;
pub mod session;
pub mod tools;
mod window;

use std::path::{Path, PathBuf};

use objc2::AllocAnyThread;
use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSAlert, NSApplication, NSImage, NSModalResponseOK, NSOpenPanel, NSPasteboard,
};
use objc2_core_foundation::Type;
use objc2_foundation::NSString;

use self::model::Document;
use self::picture::{Origin, Picture};
use self::render::BaseImage;
use self::session::Session;
use crate::capture::image;
use crate::quick_access::Item;

/// Where the edited image came from; decides what Save does.
#[derive(Clone, Debug)]
pub enum Source {
    /// A capture: Save writes over its file.
    Capture { file: PathBuf },
    /// An opened image: Save writes a new file into the save folder.
    File { path: PathBuf },
    /// An opened project: Save updates the project.
    Project { path: PathBuf },
    /// A clipboard image: Save writes a new file into the save folder.
    Clipboard,
}

mod registry {
    use std::cell::{Cell, RefCell};

    use objc2::DefinedClass;
    use objc2::rc::Retained;

    use super::window::EditorController;

    thread_local! {
        static EDITORS: RefCell<Vec<Retained<EditorController>>> = const { RefCell::new(Vec::new()) };
        static NEXT_ID: Cell<u64> = const { Cell::new(0) };
    }

    pub fn next_id() -> u64 {
        NEXT_ID.with(|n| {
            n.set(n.get() + 1);
            n.get()
        })
    }

    pub fn add(editor: Retained<EditorController>) {
        EDITORS.with(|e| e.borrow_mut().push(editor));
    }

    pub fn all() -> Vec<Retained<EditorController>> {
        EDITORS.with(|e| e.borrow().clone())
    }

    pub fn get(id: u64) -> Option<Retained<EditorController>> {
        EDITORS.with(|e| e.borrow().iter().find(|c| c.ivars().id == id).cloned())
    }

    /// Releases a closed editor once the current event is done with it.
    pub fn remove(id: u64) {
        dispatch2::DispatchQueue::main().exec_async(move || {
            EDITORS.with(|e| e.borrow_mut().retain(|c| c.ivars().id != id));
        });
    }
}

/// Debug builds only: `CLEANSHOTZ_NO_CONFIRM=1` discards unsaved
/// annotations without asking, so automated test runs never leave a
/// confirmation dialog on screen.
pub fn skip_confirmations() -> bool {
    cfg!(debug_assertions) && std::env::var_os("CLEANSHOTZ_NO_CONFIRM").is_some()
}

/// Whether any open editor has changes that were not saved or copied.
pub fn has_unsaved_changes() -> bool {
    registry::all().iter().any(|e| e.is_dirty())
}

fn open(mtm: MainThreadMarker, base: BaseImage, doc: Document, source: Source, title: &str) {
    let session = Session::new(doc, base.width, base.height);
    let id = registry::next_id();
    let editor = window::EditorController::open(mtm, id, base, session, source, title);
    registry::add(editor);
}

fn title_for(path: &Path) -> String {
    path.file_name()
        .map_or_else(|| "CleanShotZ".into(), |n| n.to_string_lossy().into_owned())
}

/// Opens a Quick Access capture.
pub fn open_capture(mtm: MainThreadMarker, item: &Item) {
    let file = item.file();
    let base = BaseImage::new(item.image(), item.scale);
    open(
        mtm,
        base,
        Document::default(),
        Source::Capture { file: file.clone() },
        &title_for(&file),
    );
}

/// Opens an image file or a `.cszdoc` project (videos go to the video
/// editor).
pub fn open_file(mtm: MainThreadMarker, path: &Path) {
    if crate::video_editor::is_video(path) {
        crate::video_editor::open(mtm, path);
        return;
    }
    let is_project = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(project::EXTENSION));
    let result = if is_project {
        project::load(path).map(|(cg, scale, doc)| {
            (
                BaseImage::new(cg, scale),
                doc,
                Source::Project {
                    path: path.to_path_buf(),
                },
            )
        })
    } else {
        image::load(path)
            .map(|cg| {
                (
                    BaseImage::new(cg, image::scale_for_path(path)),
                    Document::default(),
                    Source::File {
                        path: path.to_path_buf(),
                    },
                )
            })
            .ok_or_else(|| {
                t!(
                    "{file} is not an image CleanShotZ can open.",
                    file = path.display()
                )
            })
    };
    match result {
        Ok((base, doc, source)) => open(mtm, base, doc, source, &title_for(path)),
        Err(e) => show_error(mtm, "Could not open the file", &e),
    }
}

/// Asks for an image or project to open.
pub fn open_with_panel(mtm: MainThreadMarker) {
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setCanChooseDirectories(false);
    NSApplication::sharedApplication(mtm).activate();
    if panel.runModal() != NSModalResponseOK {
        return;
    }
    for url in panel.URLs().iter() {
        if let Some(path) = url.path() {
            open_file(mtm, Path::new(&path.to_string()));
        }
    }
}

/// Opens the image on the clipboard.
pub fn open_clipboard(mtm: MainThreadMarker) {
    let pasteboard = NSPasteboard::generalPasteboard();
    let data = ["public.png", "public.tiff", "public.jpeg"]
        .iter()
        .find_map(|t| pasteboard.dataForType(&NSString::from_str(t)));
    let Some(data) = data else {
        show_error(
            mtm,
            "No image on the clipboard",
            "Copy an image first, then try again.",
        );
        return;
    };
    let Some(picture) = picture_from_data(&data) else {
        show_error(mtm, "Could not read the clipboard image", "");
        return;
    };
    open(
        mtm,
        BaseImage::new(picture.cg().retain(), picture.scale()),
        Document::default(),
        Source::Clipboard,
        "Clipboard Image",
    );
}

/// `image` on the saved background preset `preset`, or on the default
/// background, as the Background tool draws it. Main thread (it draws with
/// AppKit).
pub fn with_background(
    mtm: MainThreadMarker,
    image: &objc2_core_graphics::CGImage,
    scale: f64,
    preset: Option<&str>,
) -> Result<objc2_core_foundation::CFRetained<objc2_core_graphics::CGImage>, String> {
    let background = match preset {
        None => model::Background::default(),
        Some(name) => presets::find(name)
            .ok_or_else(|| format!("there is no saved background called {name:?}"))?
            .background(mtm)?,
    };
    let base = BaseImage::new(image.retain(), scale);
    let doc = Document {
        background: Some(background),
        ..Document::default()
    };
    render::export(&doc, &base).ok_or_else(|| "could not draw the background".to_string())
}

/// Where `image` shows sensitive text (emails, keys, card numbers and
/// similar), in points. Recognizes text, so call it off the main thread.
pub fn sensitive_rects(
    image: &objc2_core_graphics::CGImage,
    scale: f64,
) -> Result<Vec<crate::capture::geometry::Rect>, String> {
    let (w, h) = image::pixel_size(image);
    let (width, height) = (w as f64 / scale, h as f64 / scale);
    let lines = crate::ocr::recognize(image, &crate::ocr::settings(), |text| {
        crate::ocr::sensitive::find(text)
            .into_iter()
            .map(|(_, r)| r)
            .collect()
    })?;
    Ok(lines
        .iter()
        .flat_map(|l| l.matches.iter())
        .map(|m| {
            crate::capture::geometry::Rect::new(
                m.x * width,
                m.y * height,
                m.width * width,
                m.height * height,
            )
        })
        .collect())
}

/// `image` with opaque boxes over `rects` (points), as the editor's
/// Redact command draws them. Main thread.
pub fn with_redactions(
    image: &objc2_core_graphics::CGImage,
    scale: f64,
    rects: &[crate::capture::geometry::Rect],
) -> Option<objc2_core_foundation::CFRetained<objc2_core_graphics::CGImage>> {
    let base = BaseImage::new(image.retain(), scale);
    let bounds = crate::capture::geometry::Rect::new(0.0, 0.0, base.width, base.height);
    let mut doc = Document::default();
    for rect in session::redaction_boxes(rects, bounds) {
        doc.add(
            model::Shape::Rectangle { rect, filled: true },
            session::REDACTION,
            1.0,
        );
    }
    render::export(&doc, &base)
}

/// Decodes image data from a pasteboard. Images carry their resolution: a
/// Retina screenshot reports half its pixel width as its point width.
fn picture_from_data(data: &objc2_foundation::NSData) -> Option<Picture> {
    let cg = image::decode(&data.to_vec())?;
    let points = NSImage::initWithData(NSImage::alloc(), data).map(|i| i.size().width);
    let pixels = image::pixel_size(&cg).0 as f64;
    let scale = points
        .filter(|w| *w > 0.0)
        .map_or(1.0, |w| (pixels / w).round().clamp(1.0, 3.0));
    Some(Picture::new(cg, scale, Origin::Pixels))
}

/// The images on a pasteboard: image files (for example a Quick Access
/// card or a file dragged from Finder), or else image data.
pub fn pictures_from(pasteboard: &NSPasteboard) -> Vec<Picture> {
    let file_url = NSString::from_str("public.file-url");
    let files: Vec<Picture> = pasteboard
        .pasteboardItems()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.stringForType(&file_url))
                .filter_map(|url| objc2_foundation::NSURL::URLWithString(&url))
                .filter_map(|url| url.path())
                .filter_map(|path| Picture::load(Path::new(&path.to_string())))
                .collect()
        })
        .unwrap_or_default();
    if !files.is_empty() {
        return files;
    }
    ["public.png", "public.tiff", "public.jpeg"]
        .iter()
        .find_map(|t| pasteboard.dataForType(&NSString::from_str(t)))
        .and_then(|data| picture_from_data(&data))
        .into_iter()
        .collect()
}

fn show_error(mtm: MainThreadMarker, title: &str, message: &str) {
    tracing::error!(error = message, "{title}");
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(title));
    alert.setInformativeText(&NSString::from_str(message));
    NSApplication::sharedApplication(mtm).activate();
    alert.runModal();
}

/// Development aid (debug builds): renders one of every annotation onto
/// `input` and writes the result to `output`, to check the renderer.
#[cfg(debug_assertions)]
pub fn render_sample(input: &std::path::Path, output: &std::path::Path) -> Result<(), String> {
    use crate::capture::geometry::{Point, Rect};
    use crate::capture::{image, output as out};
    use crate::config::ImageFormat;
    use model::*;

    let cg = image::load(input).ok_or("could not load the input image")?;
    let base = render::BaseImage::new(cg, image::scale_for_path(input));
    let (w, h) = (base.width, base.height);
    let at = |fx: f64, fy: f64| Point::new(w * fx, h * fy);
    let area = |fx: f64, fy: f64, fw: f64, fh: f64| Rect::new(w * fx, h * fy, w * fw, h * fh);
    let mut doc = Document::default();
    doc.add(
        Shape::Arrow {
            start: at(0.05, 0.1),
            end: at(0.3, 0.3),
            style: ArrowStyle::Filled,
        },
        Color::RED,
        6.0,
    );
    doc.add(
        Shape::Arrow {
            start: at(0.05, 0.4),
            end: at(0.3, 0.4),
            style: ArrowStyle::Open,
        },
        Color::rgb(0.1, 0.5, 1.0),
        4.0,
    );
    doc.add(
        Shape::Arrow {
            start: at(0.05, 0.5),
            end: at(0.3, 0.55),
            style: ArrowStyle::Double,
        },
        Color::rgb(0.2, 0.75, 0.3),
        4.0,
    );
    doc.add(
        Shape::Line {
            start: at(0.05, 0.65),
            end: at(0.3, 0.65),
        },
        Color::BLACK,
        3.0,
    );
    doc.add(
        Shape::Rectangle {
            rect: area(0.38, 0.08, 0.2, 0.15),
            filled: false,
        },
        Color::RED,
        4.0,
    );
    doc.add(
        Shape::Ellipse {
            rect: area(0.38, 0.3, 0.2, 0.15),
            filled: false,
        },
        Color::rgb(1.0, 0.6, 0.0),
        4.0,
    );
    doc.add(
        Shape::Rectangle {
            rect: area(0.38, 0.52, 0.08, 0.08),
            filled: true,
        },
        Color::rgb(0.5, 0.2, 0.9),
        4.0,
    );
    let wave: Vec<Point> = (0..40)
        .map(|i| {
            at(
                0.62 + i as f64 * 0.006,
                0.12 + 0.04 * ((i as f64) / 4.0).sin(),
            )
        })
        .collect();
    doc.add(Shape::Pen { points: wave }, Color::rgb(0.9, 0.1, 0.5), 4.0);
    doc.add(
        Shape::Highlighter {
            points: vec![at(0.62, 0.3), at(0.9, 0.3)],
        },
        Color::rgb(1.0, 0.9, 0.0),
        6.0,
    );
    for (i, style) in [TextStyle::Plain, TextStyle::Background]
        .into_iter()
        .enumerate()
    {
        let origin = at(0.62, 0.4 + i as f64 * 0.1);
        let s = render::text_size("Hello, CleanShotZ", 22.0, style);
        doc.add(
            Shape::Text {
                origin,
                text: "Hello, CleanShotZ".into(),
                size: 22.0,
                style,
                bounds: Rect::new(origin.x, origin.y, s.width, s.height),
            },
            Color::RED,
            4.0,
        );
    }
    for n in 1..=3 {
        doc.add(
            Shape::Counter {
                center: at(0.62 + n as f64 * 0.05, 0.65),
                number: n,
            },
            Color::RED,
            4.0,
        );
    }
    doc.add(
        Shape::Pixelate {
            rect: area(0.05, 0.75, 0.25, 0.2),
        },
        Color::RED,
        4.0,
    );
    doc.add(
        Shape::Blur {
            rect: area(0.38, 0.75, 0.25, 0.2),
        },
        Color::RED,
        4.0,
    );
    doc.add(
        Shape::Spotlight {
            rect: area(0.7, 0.75, 0.25, 0.2),
        },
        Color::RED,
        4.0,
    );
    doc.background = Some(Background::default());

    let rendered = render::export(&doc, &base).ok_or("render failed")?;
    out::save_to(output, &rendered, ImageFormat::Png, 1.0)?;

    // The project must load back to the same document and image.
    let project_path = output.with_extension(project::EXTENSION);
    project::save(&project_path, &base, &doc)?;
    let (loaded, scale, loaded_doc) = project::load(&project_path)?;
    if loaded_doc != doc
        || scale != base.scale
        || image::pixel_size(&loaded) != image::pixel_size(&base.cg)
    {
        return Err("project did not round-trip".into());
    }
    println!(
        "rendered {} and round-tripped {}",
        output.display(),
        project_path.display()
    );
    Ok(())
}

/// Development aid (debug builds): draws sample text with fake secrets on
/// a blank image, runs OCR and the sensitive-info detectors, and prints
/// what was found. Writes the redacted result to `output`.
#[cfg(debug_assertions)]
pub fn ocr_sample(output: &std::path::Path) -> Result<(), String> {
    use crate::capture::geometry::{Point, Rect};
    use crate::capture::{image, output as out};
    use crate::config::ImageFormat;
    use model::*;

    let (w, h) = (900usize, 360usize);
    let blank = image::bitmap_context(w * 2, h * 2, None).ok_or("no context")?;
    objc2_core_graphics::CGContext::set_rgb_fill_color(Some(&blank), 1.0, 1.0, 1.0, 1.0);
    objc2_core_graphics::CGContext::fill_rect(
        Some(&blank),
        objc2_core_foundation::CGRect::new(
            objc2_core_foundation::CGPoint::new(0.0, 0.0),
            objc2_core_foundation::CGSize::new((w * 2) as f64, (h * 2) as f64),
        ),
    );
    let cg = objc2_core_graphics::CGBitmapContextCreateImage(Some(&blank)).ok_or("no image")?;
    let base = render::BaseImage::new(cg, 2.0);
    let mut doc = Document::default();
    let lines = [
        "Contact: jane.doe@example.com or ops@corp.io",
        "Server 192.168.10.42 key AKIAIOSFODNN7EXAMPLE",
        "Card 4111 1111 1111 1111 expires 12/29",
        "Nothing sensitive on this line at all.",
    ];
    for (i, text) in lines.iter().enumerate() {
        let origin = Point::new(30.0, 30.0 + i as f64 * 70.0);
        let s = render::text_size(text, 28.0, TextStyle::Plain);
        doc.add(
            Shape::Text {
                origin,
                text: (*text).into(),
                size: 28.0,
                style: TextStyle::Plain,
                bounds: Rect::new(origin.x, origin.y, s.width, s.height),
            },
            Color::BLACK,
            1.0,
        );
    }
    let rendered = render::export(&doc, &base).ok_or("render failed")?;

    let started = std::time::Instant::now();
    let found = crate::ocr::recognize(&rendered, &Default::default(), |t| {
        crate::ocr::sensitive::find(t)
            .into_iter()
            .map(|(_, r)| r)
            .collect()
    })?;
    println!("OCR took {:?}", started.elapsed());
    println!("{}", crate::ocr::assemble(&found, true));
    let flat = render::BaseImage::new(rendered, 2.0);
    let mut redacted = Document::default();
    for line in &found {
        for m in &line.matches {
            let r = Rect::new(
                m.x * flat.width,
                m.y * flat.height,
                m.width * flat.width,
                m.height * flat.height,
            );
            println!("match {:?} in {:?}", r, line.text);
            redacted.add(Shape::Pixelate { rect: r }, Color::BLACK, 1.0);
        }
    }
    let result = render::export(&redacted, &flat).ok_or("render failed")?;
    out::save_to(output, &result, ImageFormat::Png, 1.0).map(|_| ())
}
