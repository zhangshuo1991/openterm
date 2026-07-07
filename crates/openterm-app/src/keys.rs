//! Keyboard -> terminal byte encoding.
//!
//! Translates iced key events into the byte sequences a remote PTY expects.
//! This is pure and has no dependency on app state, so it is unit-testable in
//! isolation.

use iced::keyboard::{key, Key, Modifiers};

/// Encode a key press into bytes to send to the PTY, or `None` if the key
/// should not be forwarded (e.g. it is an app shortcut, or a modifier-only
/// press).
pub fn encode_key(
    key: Key,
    modifiers: Modifiers,
    text: Option<&str>,
    app_cursor: bool,
) -> Option<Vec<u8>> {
    // Printable text first — this covers IME output, accented characters
    // produced by Option+key on macOS, and ordinary typing. We must check
    // this before the modifier guard so that non-English input is not dropped.
    if !modifiers.logo() && !modifiers.control() {
        if let Some(text) = text {
            if !text.is_empty() && !text.chars().any(char::is_control) {
                return Some(text.as_bytes().to_vec());
            }
        }
    }

    // Cmd/Alt combos without printable output are reserved (app shortcuts /
    // terminal Alt sequences that have no text component).
    if modifiers.logo() || modifiers.alt() {
        return None;
    }

    // Control codes: Ctrl-A..Ctrl-Z -> 0x01..0x1a.
    if modifiers.control() {
        if let Key::Character(value) = key.as_ref() {
            let mut chars = value.chars();
            let ch = chars.next()?;
            if chars.next().is_none() {
                let lower = ch.to_ascii_lowercase();
                if lower.is_ascii_alphabetic() {
                    return Some(vec![(lower as u8) - b'a' + 1]);
                }
            }
        }
        return None;
    }

    match key.as_ref() {
        Key::Named(key::Named::Enter) => Some(b"\r".to_vec()),
        Key::Named(key::Named::Tab) => Some(b"\t".to_vec()),
        Key::Named(key::Named::Backspace) => Some(vec![0x7f]),
        Key::Named(key::Named::Delete) => Some(b"\x1b[3~".to_vec()),
        Key::Named(key::Named::Escape) => Some(vec![0x1b]),
        // Cursor keys: in DECCKM (application cursor) mode — which vim, less,
        // and most full-screen apps enable — arrows and Home/End use the SS3
        // prefix (ESC O x) instead of CSI (ESC [ x). Sending the wrong form
        // makes vim see ESC + [ + A, kicking insert mode and scrambling edits.
        Key::Named(key::Named::ArrowUp) => Some(cursor_seq(app_cursor, b'A')),
        Key::Named(key::Named::ArrowDown) => Some(cursor_seq(app_cursor, b'B')),
        Key::Named(key::Named::ArrowRight) => Some(cursor_seq(app_cursor, b'C')),
        Key::Named(key::Named::ArrowLeft) => Some(cursor_seq(app_cursor, b'D')),
        Key::Named(key::Named::Home) => Some(cursor_seq(app_cursor, b'H')),
        Key::Named(key::Named::End) => Some(cursor_seq(app_cursor, b'F')),
        Key::Named(key::Named::PageUp) => Some(b"\x1b[5~".to_vec()),
        Key::Named(key::Named::PageDown) => Some(b"\x1b[6~".to_vec()),
        Key::Named(key::Named::Insert) => Some(b"\x1b[2~".to_vec()),
        Key::Named(key::Named::Space) => Some(b" ".to_vec()),
        // Function keys. F1–F4 use SS3 (ESC O …); F5–F12 use CSI (ESC [ n ~),
        // matching xterm / the `xterm-256color` terminfo we advertise.
        Key::Named(key::Named::F1) => Some(b"\x1bOP".to_vec()),
        Key::Named(key::Named::F2) => Some(b"\x1bOQ".to_vec()),
        Key::Named(key::Named::F3) => Some(b"\x1bOR".to_vec()),
        Key::Named(key::Named::F4) => Some(b"\x1bOS".to_vec()),
        Key::Named(key::Named::F5) => Some(b"\x1b[15~".to_vec()),
        Key::Named(key::Named::F6) => Some(b"\x1b[17~".to_vec()),
        Key::Named(key::Named::F7) => Some(b"\x1b[18~".to_vec()),
        Key::Named(key::Named::F8) => Some(b"\x1b[19~".to_vec()),
        Key::Named(key::Named::F9) => Some(b"\x1b[20~".to_vec()),
        Key::Named(key::Named::F10) => Some(b"\x1b[21~".to_vec()),
        Key::Named(key::Named::F11) => Some(b"\x1b[23~".to_vec()),
        Key::Named(key::Named::F12) => Some(b"\x1b[24~".to_vec()),
        _ => None,
    }
}

/// Build a cursor-key escape sequence. `final_byte` is the trailing letter
/// (A/B/C/D for arrows, H/F for Home/End). In application-cursor mode the
/// prefix is SS3 (`ESC O`); otherwise it's CSI (`ESC [`).
fn cursor_seq(app_cursor: bool, final_byte: u8) -> Vec<u8> {
    if app_cursor {
        vec![0x1b, b'O', final_byte]
    } else {
        vec![0x1b, b'[', final_byte]
    }
}

/// Wrap pasted text in bracketed-paste markers so the remote shell treats it
/// as a single literal block.
pub fn bracketed_paste(contents: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(contents.len() + 12);
    bytes.extend_from_slice(b"\x1b[200~");
    bytes.extend_from_slice(contents.as_bytes());
    bytes.extend_from_slice(b"\x1b[201~");
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_plain_text() {
        assert_eq!(
            encode_key(Key::Character("a".into()), Modifiers::empty(), Some("a"), false),
            Some(b"a".to_vec())
        );
    }

    #[test]
    fn encodes_ctrl_c() {
        assert_eq!(
            encode_key(Key::Character("c".into()), Modifiers::CTRL, None, false),
            Some(vec![3])
        );
    }

    #[test]
    fn encodes_arrow_up() {
        // Normal (CSI) mode.
        assert_eq!(
            encode_key(Key::Named(key::Named::ArrowUp), Modifiers::empty(), None, false),
            Some(b"\x1b[A".to_vec())
        );
    }

    #[test]
    fn encodes_arrows_in_application_cursor_mode() {
        // DECCKM on: arrows and Home/End use SS3 (ESC O x), as vim expects.
        assert_eq!(
            encode_key(Key::Named(key::Named::ArrowUp), Modifiers::empty(), None, true),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(
            encode_key(Key::Named(key::Named::ArrowLeft), Modifiers::empty(), None, true),
            Some(b"\x1bOD".to_vec())
        );
        assert_eq!(
            encode_key(Key::Named(key::Named::Home), Modifiers::empty(), None, true),
            Some(b"\x1bOH".to_vec())
        );
    }

    #[test]
    fn ignores_cmd_combos() {
        assert_eq!(
            encode_key(Key::Character("t".into()), Modifiers::LOGO, Some("t"), false),
            None
        );
    }

    #[test]
    fn encodes_function_keys() {
        // F1 uses SS3, F5 and F12 use CSI.
        assert_eq!(
            encode_key(Key::Named(key::Named::F1), Modifiers::empty(), None, false),
            Some(b"\x1bOP".to_vec())
        );
        assert_eq!(
            encode_key(Key::Named(key::Named::F5), Modifiers::empty(), None, false),
            Some(b"\x1b[15~".to_vec())
        );
        assert_eq!(
            encode_key(Key::Named(key::Named::F12), Modifiers::empty(), None, false),
            Some(b"\x1b[24~".to_vec())
        );
    }
}
