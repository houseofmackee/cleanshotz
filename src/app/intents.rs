//! The bridge App Intents (`swift/Intents.swift`) call: one C function that
//! runs a `cleanshotz://` request, like the URL scheme and AppleScript.

use std::ffi::{CStr, CString, c_char};

/// Runs the request `url` on the main thread. Returns null on success, or
/// an error message the caller frees with [`cleanshotz_free_string`].
///
/// # Safety
///
/// `url` must be a valid NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cleanshotz_run_request(url: *const c_char) -> *mut c_char {
    let result = if objc2::MainThreadMarker::new().is_none() {
        Err("App Intents must run on the main thread".to_string())
    } else if url.is_null() {
        Err("no request".to_string())
    } else {
        // SAFETY: per the function's contract.
        let url = unsafe { CStr::from_ptr(url) }.to_string_lossy();
        tracing::info!(%url, "App Intent");
        super::server::handle_url(&url)
    };
    match result {
        Ok(()) => std::ptr::null_mut(),
        Err(e) => {
            CString::new(e.replace('\0', " ")).map_or(std::ptr::null_mut(), CString::into_raw)
        }
    }
}

/// Frees a string returned by [`cleanshotz_run_request`].
///
/// # Safety
///
/// `string` must come from `cleanshotz_run_request` and be freed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cleanshotz_free_string(string: *mut c_char) {
    if !string.is_null() {
        // SAFETY: per the function's contract.
        drop(unsafe { CString::from_raw(string) });
    }
}
