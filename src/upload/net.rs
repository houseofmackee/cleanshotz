//! A blocking HTTP request through `NSURLSession` (system TLS, proxies and
//! certificates). Call from a background thread.

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2_foundation::{
    NSData, NSError, NSHTTPURLResponse, NSMutableURLRequest, NSString, NSURL, NSURLResponse,
    NSURLSession,
};

pub struct Response {
    pub status: isize,
    pub body: Vec<u8>,
    /// Lowercase header names.
    pub headers: HashMap<String, String>,
}

pub fn send(
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: Vec<u8>,
) -> Result<Response, String> {
    let url = NSURL::URLWithString(&NSString::from_str(url))
        .ok_or_else(|| format!("invalid URL: {url}"))?;
    let request = NSMutableURLRequest::requestWithURL(&url);
    request.setHTTPMethod(&NSString::from_str(method));
    request.setTimeoutInterval(180.0);
    for (name, value) in headers {
        request.setValue_forHTTPHeaderField(
            Some(&NSString::from_str(value)),
            &NSString::from_str(name),
        );
    }
    let (tx, rx) = mpsc::sync_channel(1);
    let handler = RcBlock::new(
        move |data: *mut NSData, response: *mut NSURLResponse, error: *mut NSError| {
            // SAFETY: each pointer is nil or a valid object for this call.
            let result = unsafe {
                if let Some(error) = error.as_ref() {
                    Err(error.localizedDescription().to_string())
                } else {
                    let body = data.as_ref().map(|d| d.to_vec()).unwrap_or_default();
                    let http: Option<Retained<NSHTTPURLResponse>> = response
                        .as_ref()
                        .and_then(|r| {
                            Retained::retain(r as *const NSURLResponse as *mut NSURLResponse)
                        })
                        .and_then(|r| r.downcast::<NSHTTPURLResponse>().ok());
                    let (status, headers) = match http {
                        Some(h) => {
                            let mut map = HashMap::new();
                            let fields = h.allHeaderFields();
                            for key in fields.allKeys().iter() {
                                if let (Ok(k), Some(v)) = (
                                    key.clone().downcast::<NSString>(),
                                    fields
                                        .objectForKey(&key)
                                        .and_then(|v| v.downcast::<NSString>().ok()),
                                ) {
                                    map.insert(k.to_string().to_ascii_lowercase(), v.to_string());
                                }
                            }
                            (h.statusCode(), map)
                        }
                        None => (0, HashMap::new()),
                    };
                    Ok(Response {
                        status,
                        body,
                        headers,
                    })
                }
            };
            let _ = tx.send(result);
        },
    );
    let session = NSURLSession::sharedSession();
    // SAFETY: valid request and body; the handler has the expected signature.
    let task = unsafe {
        session.uploadTaskWithRequest_fromData_completionHandler(
            &request,
            Some(&NSData::with_bytes(&body)),
            &handler,
        )
    };
    task.resume();
    rx.recv_timeout(Duration::from_secs(200))
        .map_err(|_| "the upload timed out".to_string())?
}
