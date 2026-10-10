//! Launch at login via `SMAppService` (macOS 13+).

use objc2_service_management::{SMAppService, SMAppServiceStatus};

pub fn is_enabled() -> bool {
    // SAFETY: no preconditions.
    unsafe { SMAppService::mainAppService().status() == SMAppServiceStatus::Enabled }
}

pub fn set_enabled(enabled: bool) -> Result<(), String> {
    // SAFETY: no preconditions; errors are returned, not thrown.
    let result = unsafe {
        let service = SMAppService::mainAppService();
        if enabled {
            service.registerAndReturnError()
        } else {
            service.unregisterAndReturnError()
        }
    };
    result.map_err(|e| e.localizedDescription().to_string())
}
