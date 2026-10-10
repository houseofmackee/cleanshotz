//! User-facing commands from menu items and global hotkeys. URLs and the
//! command line use [`crate::command`].

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    CaptureArea,
    CapturePreviousArea,
    CaptureFullscreen,
    CaptureWindow,
    SelfTimer,
    CaptureScrolling,
    CaptureText,
    RecordScreen,
    OpenHistory,
    OpenInEditor,
    AnnotateClipboard,
    RestoreRecentlyClosed,
    UnlockPins,
    ArrangePins,
    CloseAllPins,
    OpenSettings,
    OpenSettingsFile,
    ReloadSettings,
    ToggleLaunchAtLogin,
    PermissionScreenRecording,
    PermissionAccessibility,
    PermissionMicrophone,
    PermissionCamera,
    PermissionInputMonitoring,
    About,
    Quit,
}

impl Action {
    pub const ALL: [Action; 26] = [
        Action::CaptureArea,
        Action::CapturePreviousArea,
        Action::CaptureFullscreen,
        Action::CaptureWindow,
        Action::SelfTimer,
        Action::CaptureScrolling,
        Action::CaptureText,
        Action::RecordScreen,
        Action::OpenHistory,
        Action::OpenInEditor,
        Action::AnnotateClipboard,
        Action::RestoreRecentlyClosed,
        Action::UnlockPins,
        Action::ArrangePins,
        Action::CloseAllPins,
        Action::OpenSettings,
        Action::OpenSettingsFile,
        Action::ReloadSettings,
        Action::ToggleLaunchAtLogin,
        Action::PermissionScreenRecording,
        Action::PermissionAccessibility,
        Action::PermissionMicrophone,
        Action::PermissionCamera,
        Action::PermissionInputMonitoring,
        Action::About,
        Action::Quit,
    ];

    /// Stable integer used as the `NSMenuItem` tag and the Carbon hotkey id.
    /// Starts at 1 because 0 is the default tag of every menu item.
    pub fn tag(self) -> isize {
        Self::ALL.iter().position(|a| *a == self).unwrap() as isize + 1
    }

    pub fn from_tag(tag: isize) -> Option<Self> {
        usize::try_from(tag - 1)
            .ok()
            .and_then(|i| Self::ALL.get(i).copied())
    }

    pub fn title(self) -> &'static str {
        match self {
            Action::CaptureArea => "Capture Area",
            Action::CapturePreviousArea => "Capture Previous Area",
            Action::CaptureFullscreen => "Capture Fullscreen",
            Action::CaptureWindow => "Capture Window",
            Action::SelfTimer => "Self-Timer",
            Action::CaptureScrolling => "Scrolling Capture",
            Action::CaptureText => "Capture Text",
            Action::RecordScreen => "Record Screen",
            Action::OpenHistory => "Capture History…",
            Action::OpenInEditor => "Open in Editor…",
            Action::AnnotateClipboard => "Annotate Clipboard Image",
            Action::RestoreRecentlyClosed => "Restore Recently Closed",
            Action::UnlockPins => "Unlock Pinned Screenshots",
            Action::ArrangePins => "Arrange Pinned Screenshots",
            Action::CloseAllPins => "Close All Pinned Screenshots",
            Action::OpenSettings => "Settings…",
            Action::OpenSettingsFile => "Open Settings File…",
            Action::ReloadSettings => "Reload Settings",
            Action::ToggleLaunchAtLogin => "Launch at Login",
            Action::PermissionScreenRecording => "Screen Recording",
            Action::PermissionAccessibility => "Accessibility",
            Action::PermissionMicrophone => "Microphone",
            Action::PermissionCamera => "Camera",
            Action::PermissionInputMonitoring => "Input Monitoring",
            Action::About => "About CleanShotZ",
            Action::Quit => "Quit CleanShotZ",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_round_trip() {
        for action in Action::ALL {
            assert_eq!(Action::from_tag(action.tag()), Some(action));
        }
        assert_eq!(Action::from_tag(0), None);
        assert_eq!(Action::from_tag(Action::ALL.len() as isize + 1), None);
    }
}
