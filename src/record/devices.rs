//! Cameras and microphones to record with.

use objc2::Message;
use objc2::rc::Retained;
use objc2_av_foundation::{
    AVCaptureDevice, AVCaptureDeviceDiscoverySession, AVCaptureDevicePosition,
    AVCaptureDeviceTypeBuiltInWideAngleCamera, AVCaptureDeviceTypeContinuityCamera,
    AVCaptureDeviceTypeExternal, AVCaptureDeviceTypeMicrophone, AVMediaType, AVMediaTypeAudio,
    AVMediaTypeVideo,
};
use objc2_foundation::{NSArray, NSString};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    /// Stable across launches and reconnections.
    pub id: String,
    pub name: String,
}

fn discover(
    types: &[&objc2_av_foundation::AVCaptureDeviceType],
    media: &AVMediaType,
) -> Vec<Device> {
    let types: Vec<Retained<NSString>> = types.iter().map(|t| (*t).retain()).collect();
    // SAFETY: a discovery session over known device types.
    let devices = unsafe {
        AVCaptureDeviceDiscoverySession::discoverySessionWithDeviceTypes_mediaType_position(
            &NSArray::from_retained_slice(&types),
            Some(media),
            AVCaptureDevicePosition::Unspecified,
        )
        .devices()
    };
    devices
        .iter()
        .map(|d| Device {
            // SAFETY: plain property reads.
            id: unsafe { d.uniqueID() }.to_string(),
            name: unsafe { d.localizedName() }.to_string(),
        })
        .collect()
}

/// Built-in, external and Continuity cameras.
pub fn cameras() -> Vec<Device> {
    // SAFETY: reading AVFoundation constants.
    unsafe {
        let Some(video) = AVMediaTypeVideo else {
            return Vec::new();
        };
        discover(
            &[
                AVCaptureDeviceTypeBuiltInWideAngleCamera,
                AVCaptureDeviceTypeExternal,
                AVCaptureDeviceTypeContinuityCamera,
            ],
            video,
        )
    }
}

/// Built-in and external microphones.
pub fn microphones() -> Vec<Device> {
    // SAFETY: reading AVFoundation constants.
    unsafe {
        let Some(audio) = AVMediaTypeAudio else {
            return Vec::new();
        };
        discover(
            &[AVCaptureDeviceTypeMicrophone, AVCaptureDeviceTypeExternal],
            audio,
        )
    }
}

/// The system's default device of a kind, if any.
fn default_id(media: Option<&AVMediaType>) -> Option<String> {
    // SAFETY: plain queries.
    unsafe { AVCaptureDevice::defaultDeviceWithMediaType(media?).map(|d| d.uniqueID().to_string()) }
}

pub fn default_camera() -> Option<String> {
    // SAFETY: reading an AVFoundation constant.
    default_id(unsafe { AVMediaTypeVideo })
}

pub fn default_microphone() -> Option<String> {
    // SAFETY: reading an AVFoundation constant.
    default_id(unsafe { AVMediaTypeAudio })
}

/// The device `id` (empty: the system's default) of a kind.
pub fn device(id: &str, media: &AVMediaType) -> Option<Retained<AVCaptureDevice>> {
    // SAFETY: plain queries.
    unsafe {
        if !id.is_empty()
            && let Some(device) = AVCaptureDevice::deviceWithUniqueID(&NSString::from_str(id))
        {
            return Some(device);
        }
        AVCaptureDevice::defaultDeviceWithMediaType(media)
    }
}
