//! Input from a viewer to the host: pointer, buttons, wheel and keys.
//!
//! Viewers send `{"input": InputEvent}` text messages on a monitor's WebSocket. Positions
//! are fractions of the monitor's picture (0.0 left/top … 1.0 right/bottom), so they don't
//! depend on how big it is shown. Keys are named like the web's `KeyboardEvent.code`
//! (`KeyA`, `Enter`, `ShiftLeft`), the physical key, whatever the layout.

use serde::{Deserialize, Serialize};

/// A WebSocket text message from a viewer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientMessage {
    pub input: InputEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Button {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum InputEvent {
    /// The pointer is at (`x`, `y`) of the picture.
    Move { x: f32, y: f32 },
    Button { button: Button, down: bool },
    /// Scrolling, in 1/120ths of a wheel notch (the kernel's high-resolution unit);
    /// positive `dy` scrolls down, positive `dx` right.
    Wheel { dx: i32, dy: i32 },
    /// A key, by its `KeyboardEvent.code` name.
    Key { code: String, down: bool },
}

impl InputEvent {
    /// Whether the event makes sense (positions inside the picture, a known key).
    pub fn is_valid(&self) -> bool {
        match self {
            InputEvent::Move { x, y } => (0.0..=1.0).contains(x) && (0.0..=1.0).contains(y),
            InputEvent::Button { .. } => true,
            InputEvent::Wheel { dx, dy } => dx.abs() <= 120 * 50 && dy.abs() <= 120 * 50,
            InputEvent::Key { code, .. } => evdev_key(code).is_some(),
        }
    }
}

/// Linux input key code (`KEY_*`, linux/input-event-codes.h) for a `KeyboardEvent.code`.
pub fn evdev_key(code: &str) -> Option<u16> {
    const LETTERS: [u16; 26] = [
        30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17, 45, 21, 44,
    ];
    if let Some(letter) = code.strip_prefix("Key") {
        let b = *letter.as_bytes().first()?;
        return (letter.len() == 1 && b.is_ascii_uppercase()).then(|| LETTERS[(b - b'A') as usize]);
    }
    if let Some(digit) = code.strip_prefix("Digit") {
        let d: u16 = digit.parse().ok().filter(|d| *d <= 9)?;
        return Some(if d == 0 { 11 } else { d + 1 });
    }
    if let Some(n) = code.strip_prefix('F') {
        let n: u16 = n.parse().ok()?;
        return match n {
            1..=10 => Some(58 + n),
            11 => Some(87),
            12 => Some(88),
            _ => None,
        };
    }
    Some(match code {
        "Escape" => 1,
        "Minus" => 12,
        "Equal" => 13,
        "Backspace" => 14,
        "Tab" => 15,
        "BracketLeft" => 26,
        "BracketRight" => 27,
        "Enter" => 28,
        "ControlLeft" => 29,
        "Semicolon" => 39,
        "Quote" => 40,
        "Backquote" => 41,
        "ShiftLeft" => 42,
        "Backslash" => 43,
        "Comma" => 51,
        "Period" => 52,
        "Slash" => 53,
        "ShiftRight" => 54,
        "NumpadMultiply" => 55,
        "AltLeft" => 56,
        "Space" => 57,
        "CapsLock" => 58,
        "NumLock" => 69,
        "ScrollLock" => 70,
        "Numpad7" => 71,
        "Numpad8" => 72,
        "Numpad9" => 73,
        "NumpadSubtract" => 74,
        "Numpad4" => 75,
        "Numpad5" => 76,
        "Numpad6" => 77,
        "NumpadAdd" => 78,
        "Numpad1" => 79,
        "Numpad2" => 80,
        "Numpad3" => 81,
        "Numpad0" => 82,
        "NumpadDecimal" => 83,
        "IntlBackslash" => 86,
        "NumpadEnter" => 96,
        "ControlRight" => 97,
        "NumpadDivide" => 98,
        "PrintScreen" => 99,
        "AltRight" => 100,
        "Home" => 102,
        "ArrowUp" => 103,
        "PageUp" => 104,
        "ArrowLeft" => 105,
        "ArrowRight" => 106,
        "End" => 107,
        "ArrowDown" => 108,
        "PageDown" => 109,
        "Insert" => 110,
        "Delete" => 111,
        "AudioVolumeMute" => 113,
        "AudioVolumeDown" => 114,
        "AudioVolumeUp" => 115,
        "Pause" => 119,
        "MetaLeft" => 125,
        "MetaRight" => 126,
        "ContextMenu" => 127,
        _ => return None,
    })
}

/// PC keyboard scan code (set 1) for a `KeyboardEvent.code`, and whether it's an extended
/// (`E0`) key: what Windows' `SendInput` takes. Pause has none (it's sent by its key code).
pub fn pc_scancode(code: &str) -> Option<(u16, bool)> {
    let key = evdev_key(code)?;
    // Up to F12 the kernel's key codes are the scan codes.
    if key <= 88 {
        return Some((key, false));
    }
    let extended = match key {
        96 => 0x1C,  // NumpadEnter
        97 => 0x1D,  // ControlRight
        98 => 0x35,  // NumpadDivide
        99 => 0x37,  // PrintScreen
        100 => 0x38, // AltRight
        102 => 0x47, // Home
        103 => 0x48, // ArrowUp
        104 => 0x49, // PageUp
        105 => 0x4B, // ArrowLeft
        106 => 0x4D, // ArrowRight
        107 => 0x4F, // End
        108 => 0x50, // ArrowDown
        109 => 0x51, // PageDown
        110 => 0x52, // Insert
        111 => 0x53, // Delete
        113 => 0x20, // AudioVolumeMute
        114 => 0x2E, // AudioVolumeDown
        115 => 0x30, // AudioVolumeUp
        125 => 0x5B, // MetaLeft
        126 => 0x5C, // MetaRight
        127 => 0x5D, // ContextMenu
        _ => return None,
    };
    Some((extended, true))
}

/// The keys `evdev_key` knows, for setting up a keyboard device.
pub fn evdev_keys() -> impl Iterator<Item = u16> {
    (1..=127u16).filter(|k| ![84, 85, 89, 90, 91, 92, 93, 94, 95, 101, 112, 116, 117, 118, 120, 121, 122, 123, 124].contains(k))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_codes() {
        assert_eq!(evdev_key("KeyA"), Some(30));
        assert_eq!(evdev_key("KeyZ"), Some(44));
        assert_eq!(evdev_key("KeyQ"), Some(16));
        assert_eq!(evdev_key("Digit1"), Some(2));
        assert_eq!(evdev_key("Digit0"), Some(11));
        assert_eq!(evdev_key("F1"), Some(59));
        assert_eq!(evdev_key("F10"), Some(68));
        assert_eq!(evdev_key("F12"), Some(88));
        assert_eq!(evdev_key("Enter"), Some(28));
        assert_eq!(evdev_key("ArrowDown"), Some(108));
        assert_eq!(evdev_key("MetaLeft"), Some(125));
        for bad in ["Keya", "KeyAB", "Digit10", "F13", "Fn", "Unidentified", ""] {
            assert_eq!(evdev_key(bad), None, "{bad}");
        }
    }

    #[test]
    fn scan_codes() {
        assert_eq!(pc_scancode("KeyA"), Some((0x1E, false)));
        assert_eq!(pc_scancode("Escape"), Some((0x01, false)));
        assert_eq!(pc_scancode("F12"), Some((0x58, false)));
        assert_eq!(pc_scancode("ArrowUp"), Some((0x48, true)));
        assert_eq!(pc_scancode("ControlRight"), Some((0x1D, true)));
        assert_eq!(pc_scancode("Pause"), None);
        // Every key but Pause can be typed on Windows.
        let missing: Vec<u16> = evdev_keys().filter(|k| *k != 119 && *k > 88).collect();
        let known = ["NumpadEnter", "ControlRight", "NumpadDivide", "PrintScreen", "AltRight", "Home", "ArrowUp", "PageUp",
            "ArrowLeft", "ArrowRight", "End", "ArrowDown", "PageDown", "Insert", "Delete", "AudioVolumeMute",
            "AudioVolumeDown", "AudioVolumeUp", "MetaLeft", "MetaRight", "ContextMenu"];
        assert_eq!(missing.len(), known.len());
        for code in known {
            assert!(pc_scancode(code).is_some_and(|(_, e)| e), "{code}");
        }
    }

    #[test]
    fn messages() {
        let m: ClientMessage = serde_json::from_str(r#"{"input":{"t":"move","x":0.5,"y":0.25}}"#).unwrap();
        assert_eq!(m.input, InputEvent::Move { x: 0.5, y: 0.25 });
        let m: ClientMessage = serde_json::from_str(r#"{"input":{"t":"button","button":"right","down":true}}"#).unwrap();
        assert_eq!(m.input, InputEvent::Button { button: Button::Right, down: true });
        assert!(InputEvent::Key { code: "KeyA".into(), down: true }.is_valid());
        assert!(!InputEvent::Key { code: "Nope".into(), down: true }.is_valid());
        assert!(!InputEvent::Move { x: 1.5, y: 0.0 }.is_valid());
        assert!(InputEvent::Wheel { dx: 0, dy: -240 }.is_valid());
    }
}
