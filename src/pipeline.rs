//! Post-capture pipelines: named lists of steps in the `[pipelines]`
//! section of the settings file, run on a screenshot instead of the usual
//! copy, save and Quick Access handling. For example:
//!
//! ```toml
//! [pipelines]
//! share = ["resize 50%", "background", "format webp 85%", "upload", "copy-link"]
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use dispatch2::DispatchQueue;
use objc2::MainThreadMarker;
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::CGImage;

use crate::capture::geometry::Rect;
use crate::capture::output::{self, Captured};
use crate::capture::{image, naming};
use crate::config::{CaptureSettings, ImageFormat};
use crate::video_editor::AssertSend;

#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// Scale by a factor (`resize 50%`), to at most a width in pixels
    /// (`resize 1280px`), or to one pixel per point (`resize 1x`).
    Resize(Resize),
    /// Put the image on a saved background preset, or on the editor's
    /// default background.
    Background(Option<String>),
    /// Cover emails, keys, tokens and similar text that was recognized.
    Redact,
    /// Convert to sRGB.
    Srgb,
    /// Put the watermark from Settings > Watermark on the image.
    Watermark,
    /// The format for files written after this step, with a quality for
    /// JPEG and WebP (`format webp` alone is lossless).
    Format(ImageFormat, Option<f64>),
    /// Save to the configured folder, or to the given one.
    Save(Option<PathBuf>),
    Copy,
    Upload,
    CopyLink,
    /// Open the file in its default app, or in the named app.
    Open(Option<String>),
    /// Run a shell command with the file path as `$1` and the link (if
    /// uploaded) in `CLEANSHOTZ_LINK`.
    Run(String),
    Annotate,
    Pin,
    QuickAccess,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Resize {
    Factor(f64),
    MaxWidth(usize),
    OneX,
}

/// Every step's syntax, for error messages.
const SYNTAX: &str = "resize 50% | resize 1280px | resize 1x, background [preset], redact, srgb, \
format png | format jpeg 80% | format webp [85%], save [folder], copy, upload, copy-link, \
open [app], run <command>, annotate, pin, quick-access";

pub fn parse_step(text: &str) -> Result<Step, String> {
    let text = text.trim();
    let (name, arg) = match text.split_once(char::is_whitespace) {
        Some((name, arg)) => (name, Some(arg.trim())),
        None => (text, None),
    };
    let no_arg = |step: Step| match arg {
        None => Ok(step),
        Some(_) => Err(format!("{name:?} takes nothing after it")),
    };
    match name.to_ascii_lowercase().as_str() {
        "resize" => {
            let arg = arg.ok_or("resize needs a size, such as 50%, 1280px or 1x")?;
            let resize = if arg.eq_ignore_ascii_case("1x") {
                Resize::OneX
            } else if let Some(percent) = arg.strip_suffix('%') {
                let p: f64 = percent
                    .trim()
                    .parse()
                    .map_err(|_| format!("not a percentage: {arg}"))?;
                if !(1.0..=400.0).contains(&p) {
                    return Err("resize takes 1% to 400%".into());
                }
                Resize::Factor(p / 100.0)
            } else if let Some(px) = arg.strip_suffix("px") {
                let w: usize = px
                    .trim()
                    .parse()
                    .map_err(|_| format!("not a width: {arg}"))?;
                if w == 0 {
                    return Err("resize needs a width above 0".into());
                }
                Resize::MaxWidth(w)
            } else {
                return Err(format!("resize takes 50%, 1280px or 1x, not {arg:?}"));
            };
            Ok(Step::Resize(resize))
        }
        "background" => Ok(Step::Background(arg.map(str::to_string))),
        "redact" => no_arg(Step::Redact),
        "srgb" => no_arg(Step::Srgb),
        "watermark" => no_arg(Step::Watermark),
        "format" => {
            let arg = arg.ok_or("format needs png, jpeg or webp")?;
            let mut parts = arg.split_whitespace();
            let format = parts
                .next()
                .and_then(ImageFormat::from_extension)
                .ok_or_else(|| format!("format takes png, jpeg or webp, not {arg:?}"))?;
            let quality = parts
                .next()
                .map(|q| {
                    q.trim_end_matches('%')
                        .parse::<f64>()
                        .ok()
                        .filter(|q| (0.0..=100.0).contains(q))
                        .map(|q| q / 100.0)
                        .ok_or_else(|| format!("quality must be 0% to 100%, not {q:?}"))
                })
                .transpose()?;
            if parts.next().is_some() {
                return Err(format!("too many words in {text:?}"));
            }
            if quality.is_some() && format == ImageFormat::Png {
                return Err("PNG has no quality setting".into());
            }
            Ok(Step::Format(format, quality))
        }
        "save" => Ok(Step::Save(arg.map(naming::expand_home))),
        "copy" => no_arg(Step::Copy),
        "upload" => no_arg(Step::Upload),
        "copy-link" => no_arg(Step::CopyLink),
        "open" => Ok(Step::Open(arg.map(str::to_string))),
        "run" => Ok(Step::Run(arg.ok_or("run needs a command")?.to_string())),
        "annotate" => no_arg(Step::Annotate),
        "pin" => no_arg(Step::Pin),
        "quick-access" => no_arg(Step::QuickAccess),
        "" => Err("empty step".into()),
        _ => Err(format!("unknown step {name:?}; steps are: {SYNTAX}")),
    }
}

pub fn parse(steps: &[String]) -> Result<Vec<Step>, String> {
    if steps.is_empty() {
        return Err("the pipeline has no steps".into());
    }
    steps.iter().map(|s| parse_step(s)).collect()
}

pub type Pipelines = BTreeMap<String, Vec<String>>;

static PIPELINES: Mutex<Pipelines> = Mutex::new(BTreeMap::new());

pub fn configure(pipelines: Pipelines) {
    if let Ok(mut p) = PIPELINES.lock() {
        *p = pipelines;
    }
}

/// The pipelines from the settings file.
pub fn configured() -> Pipelines {
    PIPELINES.lock().map(|p| p.clone()).unwrap_or_default()
}

/// The steps of the pipeline called `name`.
pub fn named(pipelines: &Pipelines, name: &str) -> Result<Vec<Step>, String> {
    let steps = pipelines.get(name).ok_or_else(|| {
        let names: Vec<&str> = pipelines.keys().map(String::as_str).collect();
        if names.is_empty() {
            format!("there is no pipeline {name:?}; add one to [pipelines] in the settings file")
        } else {
            format!(
                "there is no pipeline {name:?}; there are: {}",
                names.join(", ")
            )
        }
    })?;
    parse(steps).map_err(|e| format!("pipeline {name:?}: {e}"))
}

/// A screenshot moving through a pipeline.
struct Job {
    image: CFRetained<CGImage>,
    scale: f64,
    source: Option<Rect>,
    settings: CaptureSettings,
    format: ImageFormat,
    quality: f64,
    /// The file holding the current image, if it was written since the
    /// last change.
    file: Option<PathBuf>,
    saved: bool,
    link: Option<String>,
    /// The name of the file the image came from (`run-pipeline` on a
    /// file), for naming a copy on the clipboard.
    name: Option<String>,
}

/// Runs `steps` on a capture. Image work, files and uploads happen on a
/// background thread, windows on the main thread. Stops at the first
/// failing step and shows why.
pub fn run(captured: Captured, settings: &CaptureSettings, steps: Vec<Step>) {
    run_named(captured, settings, steps, None);
}

/// Like [`run`], for an image that came from a file called `name`.
fn run_named(
    captured: Captured,
    settings: &CaptureSettings,
    steps: Vec<Step>,
    name: Option<String>,
) {
    let job = Job {
        image: captured.image,
        scale: captured.scale,
        source: captured.source,
        format: settings.format,
        quality: settings.quality(settings.format),
        settings: settings.clone(),
        file: None,
        saved: false,
        link: None,
        name,
    };
    let job = AssertSend(job);
    std::thread::spawn(move || {
        let mut job = job.into_inner();
        let mut failure = None;
        for step in &steps {
            if let Err(e) = job.step(step) {
                failure = Some(format!("{}: {e}", describe(step)));
                break;
            }
        }
        // Record whatever the pipeline produced, even if a step failed.
        let file = job.ensure_file().ok();
        let done = AssertSend((job.image.clone(), file, job.link.clone(), failure));
        DispatchQueue::main().exec_async(move || {
            let mtm = MainThreadMarker::new().expect("runs on the main thread");
            let (image, file, link, failure) = done.into_inner();
            if let Some(file) = file {
                crate::history::record_image(image, file, link);
            }
            if let Some(e) = failure {
                tracing::error!(error = %e, "pipeline stopped");
                crate::app::alert::show(mtm, "The pipeline stopped", &e, &["OK"]);
            }
        });
    });
}

/// Runs a pipeline on an image file (the `run-pipeline` command).
pub fn run_on_file(
    path: &Path,
    settings: &CaptureSettings,
    steps: Vec<Step>,
) -> Result<(), String> {
    let image = image::load(path).ok_or_else(|| format!("{} is not an image", path.display()))?;
    let scale = image::scale_for_path(path);
    run_named(
        Captured {
            image,
            scale,
            source: None,
        },
        settings,
        steps,
        Some(output::name_of(path)),
    );
    Ok(())
}

fn describe(step: &Step) -> String {
    match step {
        Step::Resize(_) => "resize",
        Step::Background(_) => "background",
        Step::Redact => "redact",
        Step::Srgb => "srgb",
        Step::Watermark => "watermark",
        Step::Format(..) => "format",
        Step::Save(_) => "save",
        Step::Copy => "copy",
        Step::Upload => "upload",
        Step::CopyLink => "copy-link",
        Step::Open(_) => "open",
        Step::Run(_) => "run",
        Step::Annotate => "annotate",
        Step::Pin => "pin",
        Step::QuickAccess => "quick-access",
    }
    .to_string()
}

/// Runs `f` on the main thread and waits for it.
fn on_main<T: Send>(f: impl FnOnce(MainThreadMarker) -> T + Send) -> T {
    let mut result = None;
    DispatchQueue::main().exec_sync(|| {
        let mtm = MainThreadMarker::new().expect("runs on the main thread");
        result = Some(f(mtm));
    });
    result.expect("the main queue ran the block")
}

impl Job {
    fn step(&mut self, step: &Step) -> Result<(), String> {
        match step {
            Step::Resize(resize) => self.resize(*resize),
            Step::Srgb => {
                if let Some(srgb) = image::to_srgb(&self.image) {
                    self.set_image(srgb);
                }
                Ok(())
            }
            Step::Watermark => {
                let style = crate::watermark::settings().style;
                let overlay = on_main(move |_| {
                    crate::watermark::mark(&style).map(|mark| crate::watermark::Overlay {
                        mark,
                        style,
                        free: None,
                    })
                })?;
                let marked = overlay
                    .apply(&self.image)
                    .ok_or("could not draw the watermark")?;
                self.set_image(marked);
                Ok(())
            }
            Step::Background(preset) => {
                let (image, scale) = (AssertSend(self.image.clone()), self.scale);
                let preset = preset.clone();
                let image = on_main(move |mtm| {
                    crate::editor::with_background(
                        mtm,
                        &image.into_inner(),
                        scale,
                        preset.as_deref(),
                    )
                    .map(AssertSend)
                })?;
                self.set_image(image.into_inner());
                Ok(())
            }
            Step::Redact => {
                let rects = crate::editor::sensitive_rects(&self.image, self.scale)?;
                if rects.is_empty() {
                    return Ok(());
                }
                let (image, scale) = (AssertSend(self.image.clone()), self.scale);
                let image = on_main(move |_| {
                    crate::editor::with_redactions(&image.into_inner(), scale, &rects)
                        .map(AssertSend)
                });
                self.set_image(image.ok_or("could not cover the text")?.into_inner());
                Ok(())
            }
            Step::Format(format, quality) => {
                self.format = *format;
                self.quality = match (format, quality) {
                    (_, Some(q)) => *q,
                    // WebP without a quality is lossless.
                    (ImageFormat::Webp, None) => 1.0,
                    (_, None) => self.settings.quality(*format),
                };
                self.file = None;
                self.saved = false;
                Ok(())
            }
            Step::Save(folder) => {
                let settings = CaptureSettings {
                    save_directory: folder
                        .as_ref()
                        .map(|f| f.to_string_lossy().into_owned())
                        .unwrap_or_else(|| self.settings.save_directory.clone()),
                    format: self.format,
                    jpeg_quality: self.quality,
                    webp_quality: self.quality,
                    ..self.settings.clone()
                };
                let (path, _) =
                    output::save_to_directory(&self.image, self.scale, &settings, None)?;
                tracing::info!(path = %path.display(), "pipeline saved the capture");
                self.file = Some(path);
                self.saved = true;
                Ok(())
            }
            Step::Copy => {
                let png = image::encode(&self.image, ImageFormat::Png, 1.0)
                    .ok_or("could not encode the image")?;
                // Named like the file it has (or would have).
                let name = match (&self.file, &self.name) {
                    (Some(file), _) => output::name_of(file),
                    (None, Some(name)) => name.clone(),
                    (None, None) => naming::file_stem(
                        &self.settings.file_name,
                        &chrono::Local::now(),
                        self.scale > 1.0,
                    ),
                };
                on_main(move |_| output::copy_png(&png, &name));
                Ok(())
            }
            Step::Upload => {
                let file = self.ensure_file()?;
                self.link = Some(crate::upload::upload_file(&file)?);
                Ok(())
            }
            Step::CopyLink => {
                let link = self
                    .link
                    .clone()
                    .ok_or("nothing was uploaded before this step")?;
                on_main(move |_| crate::ocr::copy_string(&link));
                Ok(())
            }
            Step::Open(app) => {
                let file = self.ensure_file()?;
                let mut command = Command::new("/usr/bin/open");
                if let Some(app) = app {
                    command.arg("-a").arg(app);
                }
                let status = command
                    .arg(&file)
                    .status()
                    .map_err(|e| format!("could not run open: {e}"))?;
                if status.success() {
                    Ok(())
                } else {
                    Err(format!("open failed ({status})"))
                }
            }
            Step::Run(script) => {
                let file = self.ensure_file()?;
                let output = Command::new("/bin/sh")
                    .arg("-c")
                    .arg(format!("{script} \"$1\""))
                    .arg("cleanshotz-pipeline")
                    .arg(&file)
                    .env("CLEANSHOTZ_FILE", &file)
                    .env("CLEANSHOTZ_LINK", self.link.as_deref().unwrap_or(""))
                    .output()
                    .map_err(|e| format!("could not run the command: {e}"))?;
                if output.status.success() {
                    Ok(())
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    Err(format!(
                        "the command failed ({}): {}",
                        output.status,
                        stderr.trim()
                    ))
                }
            }
            Step::Annotate | Step::Pin | Step::QuickAccess => {
                let file = self.ensure_file()?;
                let item = AssertSend((
                    self.image.clone(),
                    self.scale,
                    self.source,
                    file,
                    self.saved,
                ));
                let step = step.clone();
                on_main(move |mtm| {
                    let (image, scale, source, file, saved) = item.into_inner();
                    let item = crate::quick_access::Item::new(
                        image.clone(),
                        scale,
                        source,
                        file.clone(),
                        saved,
                    );
                    match step {
                        Step::Annotate => crate::editor::open_capture(mtm, &item),
                        Step::Pin => crate::pin::pin(mtm, &item),
                        _ => crate::quick_access::add(
                            mtm,
                            output::Delivered {
                                image,
                                scale,
                                source,
                                file,
                                saved,
                                png: None,
                                unmarked: None,
                            },
                        ),
                    }
                });
                Ok(())
            }
        }
    }

    fn resize(&mut self, resize: Resize) -> Result<(), String> {
        let (w, h) = image::pixel_size(&self.image);
        let factor = match resize {
            Resize::Factor(f) => f,
            Resize::MaxWidth(max) => (max as f64 / w as f64).min(1.0),
            Resize::OneX => 1.0 / self.scale,
        };
        if (factor - 1.0).abs() < f64::EPSILON {
            return Ok(());
        }
        let width = ((w as f64 * factor).round() as usize).max(1);
        let height = ((h as f64 * factor).round() as usize).max(1);
        let resized = image::resize(&self.image, width, height).ok_or("could not resize")?;
        // Keep the size in points: a 50% Retina capture becomes 1x.
        self.scale = (self.scale * factor).max(f64::MIN_POSITIVE);
        self.set_image(resized);
        Ok(())
    }

    fn set_image(&mut self, image: CFRetained<CGImage>) {
        self.image = image;
        self.file = None;
        self.saved = false;
    }

    /// The file for the current image, writing it to the unsaved folder
    /// if no step wrote it yet.
    fn ensure_file(&mut self) -> Result<PathBuf, String> {
        if let Some(file) = &self.file {
            return Ok(file.clone());
        }
        let dir = output::unsaved_dir();
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let stem = naming::file_stem(
            &self.settings.file_name,
            &chrono::Local::now(),
            self.scale > 1.0,
        );
        let path = naming::unique_path(&dir, &stem, self.format.extension());
        output::save_to(&path, &self.image, self.format, self.quality)?;
        self.file = Some(path.clone());
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_steps() {
        assert_eq!(
            parse_step("resize 50%"),
            Ok(Step::Resize(Resize::Factor(0.5)))
        );
        assert_eq!(
            parse_step("resize 1280px"),
            Ok(Step::Resize(Resize::MaxWidth(1280)))
        );
        assert_eq!(parse_step("Resize 1x"), Ok(Step::Resize(Resize::OneX)));
        assert_eq!(parse_step("srgb"), Ok(Step::Srgb));
        assert_eq!(parse_step("watermark"), Ok(Step::Watermark));
        assert!(parse_step("srgb now").is_err());
        assert_eq!(
            parse_step("format webp"),
            Ok(Step::Format(ImageFormat::Webp, None))
        );
        assert_eq!(
            parse_step("format jpeg 80%"),
            Ok(Step::Format(ImageFormat::Jpeg, Some(0.8)))
        );
        assert_eq!(parse_step("save"), Ok(Step::Save(None)));
        assert_eq!(
            parse_step("save /tmp/shots"),
            Ok(Step::Save(Some(PathBuf::from("/tmp/shots"))))
        );
        assert_eq!(
            parse_step("open Preview"),
            Ok(Step::Open(Some("Preview".into())))
        );
        assert_eq!(
            parse_step("run ~/bin/post.sh --fast"),
            Ok(Step::Run("~/bin/post.sh --fast".into()))
        );
        assert_eq!(parse_step(" copy-link "), Ok(Step::CopyLink));
        assert_eq!(parse_step("background"), Ok(Step::Background(None)));
        assert_eq!(
            parse_step("background Sunset Card"),
            Ok(Step::Background(Some("Sunset Card".into())))
        );
    }

    #[test]
    fn rejects_bad_steps() {
        for step in [
            "",
            "resize",
            "resize big",
            "resize 0%",
            "resize 0px",
            "format gif",
            "format png 80%",
            "format jpeg 120%",
            "copy now",
            "run",
            "teleport",
        ] {
            assert!(parse_step(step).is_err(), "{step:?} should be rejected");
        }
        assert!(parse(&[]).is_err());
    }

    #[test]
    fn finds_named_pipelines() {
        let mut pipelines = Pipelines::new();
        assert!(
            named(&pipelines, "share")
                .unwrap_err()
                .contains("[pipelines]")
        );
        pipelines.insert("share".into(), vec!["upload".into(), "copy-link".into()]);
        assert_eq!(
            named(&pipelines, "share"),
            Ok(vec![Step::Upload, Step::CopyLink])
        );
        pipelines.insert("bad".into(), vec!["nope".into()]);
        assert!(
            named(&pipelines, "bad")
                .unwrap_err()
                .starts_with("pipeline \"bad\"")
        );
        assert!(
            named(&pipelines, "other")
                .unwrap_err()
                .contains("bad, share")
        );
    }
}
