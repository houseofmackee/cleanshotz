//! User-facing commands. Menu items, global hotkeys and (later) the URL
//! scheme and CLI all resolve to an [`Action`].

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    CaptureArea,
    CapturePreviousArea,
    CaptureFullscreen,
    CaptureWindow,
    CaptureScrolling,
    CaptureText,
    RecordScreen,
    OpenHistory,
    OpenSettingsFile,
    ReloadSettings,
    ToggleLaunchAtLogin,
    PermissionScreenRecording,
    PermissionAccessibility,
    PermissionMicrophone,
    PermissionCamera,
    About,
    Quit,
}

impl Action {
    pub const ALL: [Action; 17] = [
        Action::CaptureArea,
        Action::CapturePreviousArea,
        Action::CaptureFullscreen,
        Action::CaptureWindow,
        Action::CaptureScrolling,
        Action::CaptureText,
        Action::RecordScreen,
        Action::OpenHistory,
        Action::OpenSettingsFile,
        Action::ReloadSettings,
        Action::ToggleLaunchAtLogin,
        Action::PermissionScreenRecording,
        Action::PermissionAccessibility,
        Action::PermissionMicrophone,
        Action::PermissionCamera,
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
            Action::CaptureScrolling => "Scrolling Capture",
            Action::CaptureText => "Capture Text",
            Action::RecordScreen => "Record Screen",
            Action::OpenHistory => "Capture History…",
            Action::OpenSettingsFile => "Open Settings File…",
            Action::ReloadSettings => "Reload Settings",
            Action::ToggleLaunchAtLogin => "Launch at Login",
            Action::PermissionScreenRecording => "Screen Recording",
            Action::PermissionAccessibility => "Accessibility",
            Action::PermissionMicrophone => "Microphone",
            Action::PermissionCamera => "Camera",
            Action::About => "About CleanShotZ",
            Action::Quit => "Quit CleanShotZ",
        }
    }

    /// The milestone that implements this action, for actions that are not
    /// available yet.
    pub fn pending_milestone(self) -> Option<&'static str> {
        match self {
            Action::CaptureArea
            | Action::CapturePreviousArea
            | Action::CaptureFullscreen
            | Action::CaptureWindow => Some("M2"),
            Action::CaptureText => Some("M5"),
            Action::RecordScreen => Some("M6"),
            Action::CaptureScrolling => Some("M8"),
            Action::OpenHistory => Some("M9"),
            _ => None,
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
