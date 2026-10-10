//! Building upload requests and reading links from responses, without
//! networking.

use std::path::Path;

use super::sigv4::{self, Credentials, Request};
use crate::config::S3Upload;

pub fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("mp4" | "m4v") => "video/mp4",
        Some("mov") => "video/quicktime",
        _ => "application/octet-stream",
    }
}

/// A `multipart/form-data` body with `fields` and then the file.
pub fn multipart(
    boundary: &str,
    fields: &[(&str, &str)],
    file_field: &str,
    file_name: &str,
    content_type: &str,
    data: &[u8],
) -> Vec<u8> {
    let quote = |s: &str| s.replace('"', "%22").replace(['\r', '\n'], " ");
    let mut body = Vec::with_capacity(data.len() + 512);
    for (name, value) in fields {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"\r\n\r\n{value}\r\n",
                quote(name)
            )
            .as_bytes(),
        );
    }
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\nContent-Type: {content_type}\r\n\r\n",
            quote(file_field),
            quote(file_name)
        )
        .as_bytes(),
    );
    body.extend_from_slice(data);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

/// Reads the link from a response, per the `link` setting: `text`,
/// `json:path.to.field` or `header:Name`.
pub fn extract_link(
    spec: &str,
    body: &[u8],
    header: impl Fn(&str) -> Option<String>,
) -> Result<String, String> {
    let link = if let Some(name) = spec.strip_prefix("header:") {
        header(name.trim()).ok_or_else(|| format!("the response has no {name} header"))?
    } else if let Some(path) = spec.strip_prefix("json:") {
        let json: serde_json::Value =
            serde_json::from_slice(body).map_err(|e| format!("the response is not JSON: {e}"))?;
        let pointer = format!("/{}", path.trim().replace('.', "/"));
        match json.pointer(&pointer) {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
            None => return Err(format!("the response has no {path}")),
        }
    } else {
        String::from_utf8_lossy(body)
            .lines()
            .next()
            .unwrap_or_default()
            .to_string()
    };
    let link = link.trim().to_string();
    if link.is_empty() {
        Err("the response contained no link".into())
    } else {
        Ok(link)
    }
}

/// A signed S3 `PUT` request for `file_name`.
pub struct S3Put {
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// The link to copy.
    pub link: String,
}

pub fn s3_put(
    settings: &S3Upload,
    secret: &str,
    file_name: &str,
    content_type: &str,
    body: &[u8],
    amz_date: &str,
) -> Result<S3Put, String> {
    if settings.bucket.is_empty() || settings.access_key_id.is_empty() {
        return Err("set upload.s3.bucket and upload.s3.access_key_id".into());
    }
    let key = format!("{}{}", settings.prefix, file_name);
    let encoded_key = sigv4::uri_encode(&key, true);
    // AWS: virtual-hosted style. Custom endpoints: path style.
    let (scheme_host, path) = if settings.endpoint.is_empty() {
        (
            format!(
                "https://{}.s3.{}.amazonaws.com",
                settings.bucket, settings.region
            ),
            format!("/{encoded_key}"),
        )
    } else {
        (
            settings.endpoint.trim_end_matches('/').to_string(),
            format!(
                "/{}/{encoded_key}",
                sigv4::uri_encode(&settings.bucket, false)
            ),
        )
    };
    let host = scheme_host
        .split("://")
        .nth(1)
        .unwrap_or(&scheme_host)
        .to_string();
    let payload_hash = sigv4::hex(&sigv4::sha256(body));
    let mut headers: Vec<(&str, String)> = vec![
        ("content-type", content_type.to_string()),
        ("host", host),
        ("x-amz-content-sha256", payload_hash.clone()),
        ("x-amz-date", amz_date.to_string()),
    ];
    if !settings.acl.is_empty() {
        headers.push(("x-amz-acl", settings.acl.clone()));
    }
    let authorization = sigv4::authorization(
        &Request {
            method: "PUT",
            path: &path,
            query: "",
            headers: &headers,
            payload_hash: &payload_hash,
        },
        &Credentials {
            access_key_id: &settings.access_key_id,
            secret_access_key: secret,
            region: &settings.region,
            service: "s3",
        },
        amz_date,
    );
    let url = format!("{scheme_host}{path}");
    let link = if settings.public_url.is_empty() {
        url.clone()
    } else {
        format!(
            "{}/{encoded_key}",
            settings.public_url.trim_end_matches('/')
        )
    };
    let mut headers: Vec<(String, String)> = headers
        .into_iter()
        .filter(|(k, _)| *k != "host")
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    headers.push(("authorization".into(), authorization));
    Ok(S3Put { url, headers, link })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_types() {
        assert_eq!(content_type(Path::new("a@2x.PNG")), "image/png");
        assert_eq!(content_type(Path::new("a.mov")), "video/quicktime");
        assert_eq!(content_type(Path::new("a")), "application/octet-stream");
    }

    #[test]
    fn builds_multipart_bodies() {
        let body = multipart(
            "XYZ",
            &[("title", "Hi")],
            "file",
            "a \"b\".png",
            "image/png",
            b"DATA",
        );
        let text = String::from_utf8(body).unwrap();
        assert_eq!(
            text,
            "--XYZ\r\nContent-Disposition: form-data; name=\"title\"\r\n\r\nHi\r\n\
             --XYZ\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a %22b%22.png\"\r\nContent-Type: image/png\r\n\r\nDATA\r\n--XYZ--\r\n"
        );
    }

    #[test]
    fn extracts_links() {
        let none = |_: &str| None;
        assert_eq!(
            extract_link("text", b"https://x/y\n", none).unwrap(),
            "https://x/y"
        );
        let imgur = br#"{"data":{"link":"https://i.imgur.com/a.png"},"success":true}"#;
        assert_eq!(
            extract_link("json:data.link", imgur, none).unwrap(),
            "https://i.imgur.com/a.png"
        );
        assert!(extract_link("json:data.url", imgur, none).is_err());
        let location = |n: &str| (n == "Location").then(|| "https://l".to_string());
        assert_eq!(
            extract_link("header:Location", b"", location).unwrap(),
            "https://l"
        );
        assert!(extract_link("text", b"  ", none).is_err());
    }

    #[test]
    fn signs_s3_puts() {
        let settings = S3Upload {
            bucket: "shots".into(),
            region: "eu-west-1".into(),
            access_key_id: "AKID".into(),
            prefix: "cs/".into(),
            public_url: "https://cdn.example.com/".into(),
            acl: "public-read".into(),
            ..Default::default()
        };
        let put = s3_put(
            &settings,
            "SECRET",
            "a b@2x.png",
            "image/png",
            b"x",
            "20261010T120000Z",
        )
        .unwrap();
        assert_eq!(
            put.url,
            "https://shots.s3.eu-west-1.amazonaws.com/cs/a%20b%402x.png"
        );
        assert_eq!(put.link, "https://cdn.example.com/cs/a%20b%402x.png");
        let auth = &put
            .headers
            .iter()
            .find(|(k, _)| k == "authorization")
            .unwrap()
            .1;
        assert!(auth.starts_with("AWS4-HMAC-SHA256 Credential=AKID/20261010/eu-west-1/s3/aws4_request, SignedHeaders=content-type;host;x-amz-acl;x-amz-content-sha256;x-amz-date, Signature="));
        // Custom endpoints use path-style URLs.
        let r2 = S3Upload {
            endpoint: "https://acct.r2.cloudflarestorage.com".into(),
            public_url: String::new(),
            ..settings
        };
        let put = s3_put(
            &r2,
            "SECRET",
            "a.png",
            "image/png",
            b"x",
            "20261010T120000Z",
        )
        .unwrap();
        assert_eq!(
            put.url,
            "https://acct.r2.cloudflarestorage.com/shots/cs/a.png"
        );
        assert_eq!(put.link, put.url);
        assert!(s3_put(&S3Upload::default(), "", "a", "b", b"", "20261010T120000Z").is_err());
    }
}
