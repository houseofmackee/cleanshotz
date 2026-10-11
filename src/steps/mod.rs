//! Step capture: while it runs, every click saves a screenshot of the
//! window under the pointer, with the click marked. When it is stopped,
//! the steps open as a guide with a title and a caption per step, to
//! export as Markdown (with the images), HTML (one file) or PDF.

mod export;
mod session;
mod window;

pub use session::{is_running, toggle};

use objc2_core_foundation::CFRetained;
use objc2_core_graphics::CGImage;

use crate::capture::elements::Control;

/// One captured click.
pub struct Step {
    /// The window, with the click marked.
    pub image: CFRetained<CGImage>,
    /// Pixels per point of `image`.
    pub scale: f64,
    pub caption: String,
}

/// The caption a step starts with: the clicked control's name when the
/// accessibility API gives one, else the app.
pub fn default_caption(control: Option<(Control, &str)>, app: &str) -> String {
    match control {
        Some((Control::MenuItem, name)) => t!("Choose “{name}”.", name),
        Some((Control::Field, name)) => t!("Click the “{name}” field.", name),
        Some((Control::Tab, name)) => t!("Click the “{name}” tab.", name),
        Some((Control::Button | Control::Link, name)) => t!("Click “{name}”.", name),
        None if !app.is_empty() => t!("Click in {app}.", app),
        None => t!("Click."),
    }
}

/// A file name for a guide titled `title`: no path separators or leading
/// dots, and not empty.
pub fn file_stem(title: &str) -> String {
    let cleaned: String = title
        .trim()
        .chars()
        .map(|c| {
            if matches!(c, '/' | ':' | '\\') {
                '-'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim_start_matches('.').trim();
    if cleaned.is_empty() {
        "Guide".into()
    } else {
        cleaned.chars().take(80).collect()
    }
}

/// The image file of step `index` (from 0).
pub fn image_name(index: usize) -> String {
    format!("step-{:02}.png", index + 1)
}

/// A Markdown guide whose images are in the folder `images` next to it.
pub fn markdown(title: &str, captions: &[String], images: &str) -> String {
    let mut out = format!("# {}\n", title.trim());
    let folder = percent_encode(images);
    for (i, caption) in captions.iter().enumerate() {
        out.push_str(&format!(
            "\n## {}\n\n{}\n\n![{}]({folder}/{})\n",
            t!("Step {n}", n = i + 1),
            caption.trim(),
            t!("Step {n}", n = i + 1),
            image_name(i)
        ));
    }
    out
}

/// A self-contained HTML guide: `images` are PNGs with their width in
/// points, embedded as data URLs.
pub fn html(title: &str, captions: &[String], images: &[(Vec<u8>, f64)]) -> String {
    use base64::Engine;
    let title = escape(title.trim());
    let mut out = format!(
        "<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n<title>{title}</title>\n\
         <style>\n\
         body {{ font: 16px/1.5 -apple-system, BlinkMacSystemFont, sans-serif; max-width: 900px; margin: 40px auto; padding: 0 20px; color: #1d1d1f; }}\n\
         h2 {{ margin-top: 40px; font-size: 18px; }}\n\
         img {{ max-width: 100%; height: auto; border-radius: 8px; box-shadow: 0 2px 12px rgba(0,0,0,.18); }}\n\
         </style>\n</head>\n<body>\n<h1>{title}</h1>\n"
    );
    for (i, (caption, (png, width))) in captions.iter().zip(images).enumerate() {
        let step = escape(&t!("Step {n}", n = i + 1));
        out.push_str(&format!(
            "<section>\n<h2>{step}</h2>\n<p>{}</p>\n<img src=\"data:image/png;base64,{}\" width=\"{}\" alt=\"{step}\">\n</section>\n",
            escape(caption.trim()).replace('\n', "<br>"),
            base64::engine::general_purpose::STANDARD.encode(png),
            width.round()
        ));
    }
    out.push_str("</body>\n</html>\n");
    out
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Encodes what Markdown link targets cannot hold as is.
fn percent_encode(path: &str) -> String {
    path.chars()
        .map(|c| match c {
            ' ' => "%20".to_string(),
            '(' => "%28".to_string(),
            ')' => "%29".to_string(),
            '<' => "%3C".to_string(),
            '>' => "%3E".to_string(),
            c => c.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_markdown_with_relative_images() {
        let md = markdown(
            " Export a report ",
            &["Click “Reports”.".into(), "Click “Export”.\n".into()],
            "Export a report (1)",
        );
        assert_eq!(
            md,
            "# Export a report\n\
             \n## Step 1\n\nClick “Reports”.\n\n![Step 1](Export%20a%20report%20%281%29/step-01.png)\n\
             \n## Step 2\n\nClick “Export”.\n\n![Step 2](Export%20a%20report%20%281%29/step-02.png)\n"
        );
    }

    #[test]
    fn writes_escaped_html_with_embedded_images() {
        let page = html(
            "A <b> & C",
            &["Type 1 < 2\nthen save".into()],
            &[(vec![1, 2, 3], 640.4)],
        );
        assert!(page.contains("<title>A &lt;b&gt; &amp; C</title>"));
        assert!(page.contains("<p>Type 1 &lt; 2<br>then save</p>"));
        assert!(page.contains("src=\"data:image/png;base64,AQID\" width=\"640\""));
    }

    #[test]
    fn makes_safe_file_names() {
        assert_eq!(file_stem(" ../a/b:c "), "-a-b-c");
        assert_eq!(file_stem("  "), "Guide");
        assert_eq!(file_stem("Set up SSO"), "Set up SSO");
        assert_eq!(image_name(0), "step-01.png");
        assert_eq!(image_name(11), "step-12.png");
    }

    #[test]
    fn captions_name_the_clicked_element() {
        assert_eq!(
            default_caption(Some((Control::Button, "Save")), "Notes"),
            "Click “Save”."
        );
        assert_eq!(
            default_caption(Some((Control::MenuItem, "Export…")), "Notes"),
            "Choose “Export…”."
        );
        assert_eq!(
            default_caption(Some((Control::Field, "Email")), "Mail"),
            "Click the “Email” field."
        );
        assert_eq!(default_caption(None, "Notes"), "Click in Notes.");
        assert_eq!(default_caption(None, ""), "Click.");
    }
}
