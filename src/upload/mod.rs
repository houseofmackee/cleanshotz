//! Uploading captures to an HTTP endpoint, S3-compatible storage or a
//! script, then copying the link.

pub mod keychain;
mod net;
pub mod request;
pub mod sigv4;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use dispatch2::DispatchQueue;
use objc2::MainThreadMarker;

use crate::config::{Destination, HttpUpload, S3Upload, ScriptUpload, UploadSettings};
use crate::ui::toast;

static SETTINGS: Mutex<Option<UploadSettings>> = Mutex::new(None);

pub fn configure(settings: UploadSettings) {
    if let Ok(mut s) = SETTINGS.lock() {
        *s = Some(settings);
    }
}

fn settings() -> UploadSettings {
    SETTINGS
        .lock()
        .ok()
        .and_then(|s| s.clone())
        .unwrap_or_default()
}

/// Whether an upload destination is configured.
pub fn enabled() -> bool {
    settings().destination != Destination::None
}

pub fn auto_upload() -> bool {
    let s = settings();
    s.auto_upload && s.destination != Destination::None
}

/// Uploads `path` and returns its link. Blocks; call off the main thread.
pub fn upload_file(path: &Path) -> Result<String, String> {
    let settings = settings();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or("the file has no name")?;
    match settings.destination {
        Destination::None => {
            Err("Uploading is off: set [upload] destination in the settings file".into())
        }
        Destination::Http => http(&settings.http, path, &name),
        Destination::S3 => s3(&settings.s3, path, &name),
        Destination::Script => script(&settings.script, path),
    }
}

fn check_status(response: &net::Response) -> Result<(), String> {
    if (200..300).contains(&response.status) {
        Ok(())
    } else {
        let body = String::from_utf8_lossy(&response.body);
        let excerpt: String = body.chars().take(200).collect();
        Err(format!(
            "the server answered {}: {}",
            response.status,
            excerpt.trim()
        ))
    }
}

fn http(settings: &HttpUpload, path: &Path, name: &str) -> Result<String, String> {
    if settings.url.is_empty() {
        return Err("set upload.http.url in the settings file".into());
    }
    let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let content_type = request::content_type(path);
    let mut headers: Vec<(String, String)> = settings
        .headers
        .iter()
        .map(|(k, v)| Ok((k.clone(), keychain::resolve(v)?)))
        .collect::<Result<_, String>>()?;
    let method = settings.method.to_ascii_uppercase();
    let body = if method == "PUT" {
        headers.push(("Content-Type".into(), content_type.into()));
        data
    } else {
        let boundary = format!(
            "CleanShotZ{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        );
        let form: Vec<(String, String)> = settings
            .form
            .iter()
            .map(|(k, v)| Ok((k.clone(), keychain::resolve(v)?)))
            .collect::<Result<_, String>>()?;
        let fields: Vec<(&str, &str)> =
            form.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        headers.push((
            "Content-Type".into(),
            format!("multipart/form-data; boundary={boundary}"),
        ));
        request::multipart(
            &boundary,
            &fields,
            &settings.field,
            name,
            content_type,
            &data,
        )
    };
    let url = keychain::resolve(&settings.url)?;
    let response = net::send(&method, &url, &headers, body)?;
    check_status(&response)?;
    request::extract_link(&settings.link, &response.body, |h| {
        response.headers.get(&h.to_ascii_lowercase()).cloned()
    })
}

fn s3(settings: &S3Upload, path: &Path, name: &str) -> Result<String, String> {
    let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let secret = keychain::resolve(&settings.secret_access_key)?;
    let amz_date = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let put = request::s3_put(
        settings,
        &secret,
        name,
        request::content_type(path),
        &data,
        &amz_date,
    )?;
    let response = net::send("PUT", &put.url, &put.headers, data)?;
    check_status(&response)?;
    Ok(put.link)
}

fn script(settings: &ScriptUpload, path: &Path) -> Result<String, String> {
    if settings.command.trim().is_empty() {
        return Err("set upload.script.command in the settings file".into());
    }
    // The command runs in a shell with the file path as "$1".
    let output = Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("{} \"$1\"", settings.command))
        .arg("cleanshotz-upload")
        .arg(path)
        .output()
        .map_err(|e| format!("could not run the upload script: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "the upload script failed ({}): {}",
            output.status,
            stderr.trim()
        ));
    }
    request::extract_link("text", &output.stdout, |_| None)
}

/// Uploads in the background, then copies the link, records it in history
/// and shows a toast.
pub fn start(mtm: MainThreadMarker, path: PathBuf) {
    if !enabled() {
        toast::show(mtm, "Set an upload destination in the settings file first");
        return;
    }
    toast::show(mtm, "Uploading…");
    std::thread::spawn(move || {
        let result = upload_file(&path);
        DispatchQueue::main().exec_async(move || {
            let mtm = MainThreadMarker::new().expect("runs on the main thread");
            match result {
                Ok(link) => {
                    tracing::info!(path = %path.display(), %link, "uploaded");
                    crate::ocr::copy_string(&link);
                    crate::history::set_link(path, link);
                    toast::show(mtm, "Uploaded · link copied");
                }
                Err(e) => {
                    tracing::error!(path = %path.display(), error = %e, "upload failed");
                    crate::app::alert::show(mtm, "Upload failed", &e, &["OK"]);
                }
            }
        });
    });
}
