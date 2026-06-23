//! Keyboard -> terminal byte encoding.
//!
//! Translates iced key events into the byte sequences a remote PTY expects.
//! This is pure and has no dependency on app state, so it is unit-testable in
//! isolation.

use iced::keyboard::{key, Key, Modifiers};

/// Encode a key press into bytes to send to the PTY, or `None` if the key
/// should not be forwarded (e.g. it is an app shortcut, or a modifier-only
/// press).
pub fn encode_key(key: Key, modifiers: Modifiers, text: Option<&str>) -> Option<Vec<u8>> {
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
        Key::Named(key::Named::ArrowUp) => Some(b"\x1b[A".to_vec()),
        Key::Named(key::Named::ArrowDown) => Some(b"\x1b[B".to_vec()),
        Key::Named(key::Named::ArrowRight) => Some(b"\x1b[C".to_vec()),
        Key::Named(key::Named::ArrowLeft) => Some(b"\x1b[D".to_vec()),
        Key::Named(key::Named::Home) => Some(b"\x1b[H".to_vec()),
        Key::Named(key::Named::End) => Some(b"\x1b[F".to_vec()),
        Key::Named(key::Named::PageUp) => Some(b"\x1b[5~".to_vec()),
        Key::Named(key::Named::PageDown) => Some(b"\x1b[6~".to_vec()),
        Key::Named(key::Named::Space) => Some(b" ".to_vec()),
        _ => None,
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
            encode_key(Key::Character("a".into()), Modifiers::empty(), Some("a")),
            Some(b"a".to_vec())
        );
    }

    #[test]
    fn encodes_ctrl_c() {
        assert_eq!(
            encode_key(Key::Character("c".into()), Modifiers::CTRL, None),
            Some(vec![3])
        );
    }

    #[test]
    fn encodes_arrow_up() {
        assert_eq!(
            encode_key(Key::Named(key::Named::ArrowUp), Modifiers::empty(), None),
            Some(b"\x1b[A".to_vec())
        );
    }

    #[test]
    fn ignores_cmd_combos() {
        assert_eq!(
            encode_key(Key::Character("t".into()), Modifiers::LOGO, Some("t")),
            None
        );
    }
}
