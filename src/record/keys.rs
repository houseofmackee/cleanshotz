//! Labels for the keystroke display, from key codes and modifier flags.

use crate::config::KeystrokeMode;
use crate::shortcut::{Modifiers, key_name};

/// Keys that never type text, labeled as on Apple keyboards.
fn special_label(code: u32) -> Option<&'static str> {
    Some(match code {
        36 | 76 => "↩",
        48 => "⇥",
        51 => "⌫",
        53 => "esc",
        117 => "⌦",
        115 => "↖",
        119 => "↘",
        116 => "⇞",
        121 => "⇟",
        123 => "←",
        124 => "→",
        125 => "↓",
        126 => "↑",
        _ => return None,
    })
}

/// The label for a key press, or `None` if it should not be shown.
///
/// In [`KeystrokeMode::Shortcuts`] only presses with Command, Control or
/// Option, and keys that never type text (arrows, Return, Escape, F-keys)
/// are shown, so typed text such as passwords never appears on screen.
pub fn label(code: u32, modifiers: Modifiers, mode: KeystrokeMode) -> Option<String> {
    let special = special_label(code);
    let name = key_name(code);
    let function_key = name.is_some_and(|n| n.starts_with('f') && n.len() > 1);
    let key = match (special, name) {
        (Some(s), _) => s.to_string(),
        (None, Some("space")) => t!("Space"),
        (None, Some(n)) => n.to_uppercase(),
        (None, None) => return None,
    };
    let shortcut = modifiers.command || modifiers.control || modifiers.option;
    let show = match mode {
        KeystrokeMode::Off => false,
        KeystrokeMode::Shortcuts => shortcut || special.is_some() || function_key,
        KeystrokeMode::All => true,
    };
    if !show {
        return None;
    }
    let mut out = String::new();
    for (on, symbol) in [
        (modifiers.control, "⌃"),
        (modifiers.option, "⌥"),
        (modifiers.shift, "⇧"),
        (modifiers.command, "⌘"),
    ] {
        if on {
            out.push_str(symbol);
        }
    }
    // A shifted letter alone reads better as the capital letter.
    if mode == KeystrokeMode::All && !shortcut && modifiers.shift && key.len() == 1 {
        return Some(key);
    }
    out.push_str(&key);
    Some(out)
}

/// Modifier state from CoreGraphics event flags.
pub fn modifiers_from_flags(flags: u64) -> Modifiers {
    Modifiers {
        shift: flags & 0x0002_0000 != 0,
        control: flags & 0x0004_0000 != 0,
        option: flags & 0x0008_0000 != 0,
        command: flags & 0x0010_0000 != 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Modifiers = Modifiers {
        command: false,
        shift: false,
        option: false,
        control: false,
    };

    fn with(command: bool, shift: bool, option: bool, control: bool) -> Modifiers {
        Modifiers {
            command,
            shift,
            option,
            control,
        }
    }

    #[test]
    fn shortcuts_mode_hides_typing() {
        let m = KeystrokeMode::Shortcuts;
        assert_eq!(label(0, NONE, m), None); // a
        assert_eq!(label(0, with(false, true, false, false), m), None); // A
        assert_eq!(label(49, NONE, m), None); // space
        assert_eq!(
            label(8, with(true, false, false, false), m),
            Some("⌘C".into())
        );
        assert_eq!(
            label(21, with(true, true, false, false), m),
            Some("⇧⌘4".into())
        );
        assert_eq!(label(36, NONE, m), Some("↩".into()));
        assert_eq!(label(96, NONE, m), Some("F5".into()));
        assert_eq!(
            label(126, with(false, false, true, false), m),
            Some("⌥↑".into())
        );
    }

    #[test]
    fn all_mode_shows_every_key() {
        let m = KeystrokeMode::All;
        assert_eq!(label(0, NONE, m), Some("A".into()));
        assert_eq!(
            label(0, with(false, true, false, false), m),
            Some("A".into())
        );
        assert_eq!(label(49, NONE, m), Some("Space".into()));
        assert_eq!(
            label(8, with(true, false, false, true), m),
            Some("⌃⌘C".into())
        );
        assert_eq!(label(0, NONE, KeystrokeMode::Off), None);
    }

    #[test]
    fn reads_event_flags() {
        let m = modifiers_from_flags(0x0010_0000 | 0x0002_0000);
        assert!(m.command && m.shift && !m.option && !m.control);
    }
}
