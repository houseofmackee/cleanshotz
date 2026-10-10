//! Parsing of keyboard shortcut strings such as `"cmd+shift+4"`.
//!
//! Key codes are macOS virtual key codes for the ANSI layout (the values
//! Carbon's `RegisterEventHotKey` expects).

use std::fmt;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub command: bool,
    pub shift: bool,
    pub option: bool,
    pub control: bool,
}

impl Modifiers {
    pub fn is_empty(self) -> bool {
        self == Self::default()
    }

    /// Carbon modifier mask (`cmdKey`, `shiftKey`, `optionKey`, `controlKey`).
    pub fn carbon_mask(self) -> u32 {
        let mut mask = 0;
        if self.command {
            mask |= 1 << 8;
        }
        if self.shift {
            mask |= 1 << 9;
        }
        if self.option {
            mask |= 1 << 11;
        }
        if self.control {
            mask |= 1 << 12;
        }
        mask
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shortcut {
    pub modifiers: Modifiers,
    pub key: &'static Key,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Key {
    pub name: &'static str,
    pub code: u32,
    /// Character for `NSMenuItem.keyEquivalent`.
    pub equivalent: char,
}

impl Key {
    fn is_function_key(&self) -> bool {
        self.name.starts_with('f') && self.name.len() > 1
    }
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("shortcut is empty")]
    Empty,
    #[error("unknown key or modifier {0:?}")]
    UnknownKey(String),
    #[error("shortcut has no key, only modifiers")]
    MissingKey,
    #[error("shortcut has more than one key ({0:?} and {1:?})")]
    MultipleKeys(String, String),
    #[error("shortcut needs at least one modifier unless the key is F1 to F12")]
    MissingModifier,
}

macro_rules! keys {
    ($($name:literal => $code:literal, $eq:literal;)*) => {
        &[$(Key { name: $name, code: $code, equivalent: $eq }),*]
    };
}

static KEYS: &[Key] = keys! {
    "a" => 0, 'a'; "s" => 1, 's'; "d" => 2, 'd'; "f" => 3, 'f'; "h" => 4, 'h';
    "g" => 5, 'g'; "z" => 6, 'z'; "x" => 7, 'x'; "c" => 8, 'c'; "v" => 9, 'v';
    "b" => 11, 'b'; "q" => 12, 'q'; "w" => 13, 'w'; "e" => 14, 'e'; "r" => 15, 'r';
    "y" => 16, 'y'; "t" => 17, 't'; "1" => 18, '1'; "2" => 19, '2'; "3" => 20, '3';
    "4" => 21, '4'; "6" => 22, '6'; "5" => 23, '5'; "=" => 24, '='; "9" => 25, '9';
    "7" => 26, '7'; "-" => 27, '-'; "8" => 28, '8'; "0" => 29, '0'; "]" => 30, ']';
    "o" => 31, 'o'; "u" => 32, 'u'; "[" => 33, '['; "i" => 34, 'i'; "p" => 35, 'p';
    "return" => 36, '\r'; "l" => 37, 'l'; "j" => 38, 'j'; "'" => 39, '\''; "k" => 40, 'k';
    ";" => 41, ';'; "\\" => 42, '\\'; "," => 43, ','; "/" => 44, '/'; "n" => 45, 'n';
    "m" => 46, 'm'; "." => 47, '.'; "tab" => 48, '\t'; "space" => 49, ' '; "`" => 50, '`';
    "delete" => 51, '\u{8}'; "escape" => 53, '\u{1b}';
    "f1" => 122, '\u{F704}'; "f2" => 120, '\u{F705}'; "f3" => 99, '\u{F706}';
    "f4" => 118, '\u{F707}'; "f5" => 96, '\u{F708}'; "f6" => 97, '\u{F709}';
    "f7" => 98, '\u{F70A}'; "f8" => 100, '\u{F70B}'; "f9" => 101, '\u{F70C}';
    "f10" => 109, '\u{F70D}'; "f11" => 103, '\u{F70E}'; "f12" => 111, '\u{F70F}';
};

/// The name of a virtual key code in the ANSI table (`"a"`, `"return"`,
/// `"f5"`), if known.
pub fn key_name(code: u32) -> Option<&'static str> {
    KEYS.iter().find(|k| k.code == code).map(|k| k.name)
}

fn key_alias(token: &str) -> &str {
    match token {
        "enter" => "return",
        "esc" => "escape",
        "backspace" => "delete",
        "minus" => "-",
        "equal" | "equals" => "=",
        "comma" => ",",
        "period" => ".",
        "slash" => "/",
        other => other,
    }
}

impl Shortcut {
    /// Parses `+`-separated tokens, case-insensitively. Modifiers: `cmd`,
    /// `command`, `shift`, `alt`, `opt`, `option`, `ctrl`, `control`.
    pub fn parse(input: &str) -> Result<Shortcut, ParseError> {
        let input = input.trim();
        if input.is_empty() {
            return Err(ParseError::Empty);
        }
        let mut modifiers = Modifiers::default();
        let mut key: Option<&'static Key> = None;
        for raw in input.split('+') {
            let token = raw.trim().to_ascii_lowercase();
            match token.as_str() {
                "cmd" | "command" => modifiers.command = true,
                "shift" => modifiers.shift = true,
                "alt" | "opt" | "option" => modifiers.option = true,
                "ctrl" | "control" => modifiers.control = true,
                _ => {
                    let name = key_alias(&token);
                    let found = KEYS
                        .iter()
                        .find(|k| k.name == name)
                        .ok_or_else(|| ParseError::UnknownKey(raw.trim().to_string()))?;
                    if let Some(previous) = key {
                        return Err(ParseError::MultipleKeys(
                            previous.name.to_string(),
                            found.name.to_string(),
                        ));
                    }
                    key = Some(found);
                }
            }
        }
        let key = key.ok_or(ParseError::MissingKey)?;
        if modifiers.is_empty() && !key.is_function_key() {
            return Err(ParseError::MissingModifier);
        }
        Ok(Shortcut { modifiers, key })
    }
}

impl fmt::Display for Shortcut {
    /// Formats in macOS menu order: ⌃⌥⇧⌘ then the key.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = self.modifiers;
        for (on, symbol) in [
            (m.control, "⌃"),
            (m.option, "⌥"),
            (m.shift, "⇧"),
            (m.command, "⌘"),
        ] {
            if on {
                f.write_str(symbol)?;
            }
        }
        f.write_str(&self.key.name.to_uppercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modifiers_and_key() {
        let s = Shortcut::parse("Cmd + Shift + 4").unwrap();
        assert!(s.modifiers.command && s.modifiers.shift);
        assert!(!s.modifiers.option && !s.modifiers.control);
        assert_eq!(s.key.code, 21);
        assert_eq!(s.modifiers.carbon_mask(), 256 | 512);
    }

    #[test]
    fn accepts_aliases() {
        let s = Shortcut::parse("opt+ctrl+esc").unwrap();
        assert!(s.modifiers.option && s.modifiers.control);
        assert_eq!(s.key.name, "escape");
    }

    #[test]
    fn function_keys_need_no_modifier() {
        assert_eq!(Shortcut::parse("F5").unwrap().key.code, 96);
        assert_eq!(Shortcut::parse("a"), Err(ParseError::MissingModifier));
    }

    #[test]
    fn rejects_invalid_input() {
        assert_eq!(Shortcut::parse(""), Err(ParseError::Empty));
        assert_eq!(Shortcut::parse("cmd+shift"), Err(ParseError::MissingKey));
        assert_eq!(
            Shortcut::parse("cmd+hyper+a"),
            Err(ParseError::UnknownKey("hyper".into()))
        );
        assert!(matches!(
            Shortcut::parse("cmd+a+b"),
            Err(ParseError::MultipleKeys(..))
        ));
    }

    #[test]
    fn displays_in_menu_order() {
        let s = Shortcut::parse("cmd+shift+alt+ctrl+k").unwrap();
        assert_eq!(s.to_string(), "⌃⌥⇧⌘K");
    }
}
