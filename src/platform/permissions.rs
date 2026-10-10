//! Privacy (TCC) permission checks and requests.

use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_app_kit::NSWorkspace;
use objc2_av_foundation::{
    AVAuthorizationStatus, AVCaptureDevice, AVMediaType, AVMediaTypeAudio, AVMediaTypeVideo,
};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFString};
use objc2_core_graphics::{CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess};
use objc2_foundation::{NSString, NSURL};

// Input Monitoring. CoreGraphics' CGRequestListenEventAccess only checks
// access on current macOS (through the window server) and never adds the
// app to the list; IOKit's request goes to the privacy service directly.
#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOHIDCheckAccess(request_type: u32) -> u32;
    fn IOHIDRequestAccess(request_type: u32) -> bool;
}

const K_IOHID_REQUEST_TYPE_LISTEN_EVENT: u32 = 1;
const K_IOHID_ACCESS_TYPE_GRANTED: u32 = 0;
const K_IOHID_ACCESS_TYPE_UNKNOWN: u32 = 2;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: Option<&CFDictionary>) -> bool;
    static kAXTrustedCheckOptionPrompt: &'static CFString;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permission {
    ScreenRecording,
    Accessibility,
    Microphone,
    Camera,
    /// Listening to key presses (the keystroke display in recordings).
    InputMonitoring,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Granted,
    Denied,
    /// The user has not been asked yet; requesting shows the system prompt.
    NotDetermined,
}

impl Permission {
    pub fn status(self) -> Status {
        match self {
            Permission::ScreenRecording => granted_or_denied(CGPreflightScreenCaptureAccess()),
            // SAFETY: plain IOKit query.
            Permission::InputMonitoring => {
                match unsafe { IOHIDCheckAccess(K_IOHID_REQUEST_TYPE_LISTEN_EVENT) } {
                    K_IOHID_ACCESS_TYPE_GRANTED => Status::Granted,
                    K_IOHID_ACCESS_TYPE_UNKNOWN => Status::NotDetermined,
                    _ => Status::Denied,
                }
            }
            // SAFETY: no preconditions.
            Permission::Accessibility => granted_or_denied(unsafe { AXIsProcessTrusted() }),
            Permission::Microphone | Permission::Camera => {
                let Some(media_type) = self.media_type() else {
                    return Status::Denied;
                };
                // SAFETY: media_type is AVMediaTypeAudio or AVMediaTypeVideo.
                match unsafe { AVCaptureDevice::authorizationStatusForMediaType(media_type) } {
                    AVAuthorizationStatus::Authorized => Status::Granted,
                    AVAuthorizationStatus::NotDetermined => Status::NotDetermined,
                    _ => Status::Denied,
                }
            }
        }
    }

    /// Shows the system prompt when the user has not been asked yet, and
    /// otherwise opens the matching System Settings pane, since macOS only
    /// prompts once.
    ///
    /// Screen Recording and Accessibility cannot tell "not asked yet" from
    /// "denied", so for them the request call is always made: it is what
    /// adds the app to the list in System Settings (and prompts the first
    /// time). Without it the app never appears there.
    pub fn request(self) {
        match (self, self.status()) {
            (_, Status::Granted) => {}
            (Permission::ScreenRecording, _) => {
                if !CGRequestScreenCaptureAccess() {
                    self.open_settings();
                }
            }
            (Permission::InputMonitoring, status) => {
                // Prompts the first time and adds the app to the list.
                // SAFETY: plain IOKit request.
                let granted = unsafe { IOHIDRequestAccess(K_IOHID_REQUEST_TYPE_LISTEN_EVENT) };
                // After the first answer macOS no longer prompts.
                if !granted && status == Status::Denied {
                    self.open_settings();
                }
            }
            (Permission::Accessibility, _) => {
                // SAFETY: a dictionary with the documented prompt option.
                let trusted = unsafe {
                    let options = CFDictionary::from_slices(
                        &[kAXTrustedCheckOptionPrompt],
                        &[CFBoolean::new(true)],
                    );
                    AXIsProcessTrustedWithOptions(Some(options.as_opaque()))
                };
                if !trusted {
                    self.open_settings();
                }
            }
            (Permission::Microphone | Permission::Camera, Status::NotDetermined) => {
                let Some(media_type) = self.media_type() else {
                    return;
                };
                let block = RcBlock::new(move |granted: Bool| {
                    tracing::info!(permission = ?self, granted = granted.as_bool(), "permission answered");
                });
                // SAFETY: valid media type; the block is Send-safe (captures a Copy enum).
                unsafe {
                    AVCaptureDevice::requestAccessForMediaType_completionHandler(media_type, &block)
                };
            }
            _ => self.open_settings(),
        }
    }

    pub fn open_settings(self) {
        let anchor = match self {
            Permission::ScreenRecording => "Privacy_ScreenCapture",
            Permission::Accessibility => "Privacy_Accessibility",
            Permission::Microphone => "Privacy_Microphone",
            Permission::Camera => "Privacy_Camera",
            Permission::InputMonitoring => "Privacy_ListenEvent",
        };
        let url = format!("x-apple.systempreferences:com.apple.preference.security?{anchor}");
        if let Some(url) = NSURL::URLWithString(&NSString::from_str(&url)) {
            NSWorkspace::sharedWorkspace().openURL(&url);
        }
    }

    fn media_type(self) -> Option<&'static AVMediaType> {
        // SAFETY: reading framework-provided constant strings.
        unsafe {
            match self {
                Permission::Microphone => AVMediaTypeAudio,
                Permission::Camera => AVMediaTypeVideo,
                _ => None,
            }
        }
    }
}

fn granted_or_denied(granted: bool) -> Status {
    if granted {
        Status::Granted
    } else {
        Status::Denied
    }
}
