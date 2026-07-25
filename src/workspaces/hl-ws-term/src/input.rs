//! Keyboard input encoding: map a key + modifiers to the byte sequence a terminal writes to the PTY.
//!
//! This is the half of a terminal that runs *toward* the shell: a keypress (a [`Key`] plus a set of
//! [`Mods`]) becomes the exact bytes an xterm-compatible emulator would push onto the PTY master's
//! write end. The encodings follow the classic DEC/xterm conventions (CSI cursor keys, DECCKM
//! application mode, the `1;<mod>` modifier parameter, the `~`-terminated editing keys, SS3 function
//! keys, and bracketed paste). Everything here is pure: no I/O, no allocation beyond the returned
//! `Vec<u8>`, so it is trivially unit-testable byte-for-byte.

use bitflags::bitflags;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Char(char),
    Enter,
    Backspace,
    Tab,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    F(u8), // F1..F12
}

bitflags! {
    #[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
    pub struct Mods: u8 {
        const CTRL = 1;
        const ALT = 2;
        const SHIFT = 4;
        const SUPER = 8;
    }
}

/// Cursor-key mode: normal (`ESC [ A`) vs application (`ESC O A`) — set by the app from DECCKM.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CursorKeys {
    #[default]
    Normal,
    Application,
}

/// Terminal paste protocol selected by the application through DEC private mode 2004.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PasteMode {
    #[default]
    Raw,
    Bracketed,
}

impl PasteMode {
    /// Encode pasted text for the selected terminal protocol.
    pub fn encode(self, text: &str) -> Vec<u8> {
        match self {
            Self::Raw => text.as_bytes().to_vec(),
            Self::Bracketed => {
                let mut bytes = Vec::with_capacity(text.len() + 12);
                bytes.extend_from_slice(b"\x1b[200~");
                bytes.extend_from_slice(text.as_bytes());
                bytes.extend_from_slice(b"\x1b[201~");
                bytes
            }
        }
    }
}

const ESC: u8 = 0x1b;

/// The xterm modifier parameter: `1 + shift(1) + alt(2) + ctrl(4)`.
///
/// Note this ordering (shift=1, alt=2, ctrl=4) is xterm's own numbering and is deliberately *not*
/// the same as the [`Mods`] bitflag values (CTRL=1, ALT=2, SHIFT=4); we translate explicitly.
/// SUPER has no standard CSI encoding and is ignored here.
impl Mods {
    fn csi_parameter(self) -> u8 {
        1 + u8::from(self.contains(Self::SHIFT))
            + 2 * u8::from(self.contains(Self::ALT))
            + 4 * u8::from(self.contains(Self::CTRL))
    }

    fn has_csi(self) -> bool {
        self.intersects(Self::CTRL | Self::ALT | Self::SHIFT)
    }
}

/// A cursor/edit key with a CSI *letter* final byte (arrows, Home, End).
///
/// Unmodified: `ESC [ <final>` (normal) or `ESC O <final>` (application). Modified: `ESC [ 1 ; <m> <final>`.
impl Key {
    fn letter(final_byte: u8, mods: Mods, application: bool) -> Vec<u8> {
        if !mods.has_csi() {
            return vec![ESC, if application { b'O' } else { b'[' }, final_byte];
        }
        let parameter = mods.csi_parameter();
        vec![ESC, b'[', b'1', b';', b'0' + parameter, final_byte]
    }

    fn tilde(number: u8, mods: Mods) -> Vec<u8> {
        let mut sequence = Vec::with_capacity(8);
        sequence.extend_from_slice(&[ESC, b'[']);
        if number >= 10 {
            sequence.push(b'0' + number / 10);
        }
        sequence.push(b'0' + number % 10);
        if mods.has_csi() {
            let parameter = mods.csi_parameter();
            sequence.extend_from_slice(&[b';', b'0' + parameter]);
        }
        sequence.push(b'~');
        sequence
    }
}

/// Encode one keypress into the bytes to write to the PTY. Returns empty for keys with no encoding.
pub fn encode_key(key: Key, mods: Mods, cursor: CursorKeys) -> Vec<u8> {
    let app = cursor == CursorKeys::Application;
    match key {
        Key::Char(c) => {
            // Build the base byte(s): a control byte when Ctrl maps, otherwise the char's UTF-8.
            let mut base: Vec<u8> = if mods.contains(Mods::CTRL) {
                let control = match c {
                    ' ' | '@' => Some(0x00),
                    '?' => Some(0x7f),
                    // Letters and the 0x40..=0x5f symbol block map through the low five bits.
                    'a'..='z' | 'A'..='Z' | '[' | '\\' | ']' | '^' | '_' => Some(c as u8 & 0x1f),
                    _ => None,
                };
                match control {
                    Some(b) => vec![b],
                    None => {
                        let mut buf = [0u8; 4];
                        c.encode_utf8(&mut buf).as_bytes().to_vec()
                    }
                }
            } else {
                let mut buf = [0u8; 4];
                c.encode_utf8(&mut buf).as_bytes().to_vec()
            };
            // ALT (Meta) prefixes the whole sequence with ESC.
            if mods.contains(Mods::ALT) {
                let mut v = vec![ESC];
                v.append(&mut base);
                v
            } else {
                base
            }
        }
        Key::Enter => vec![b'\r'],
        Key::Backspace => vec![0x7f],
        Key::Tab => {
            if mods.contains(Mods::SHIFT) {
                vec![ESC, b'[', b'Z'] // back-tab (CBT)
            } else {
                vec![b'\t']
            }
        }
        Key::Escape => vec![ESC],
        Key::Up => Key::letter(b'A', mods, app),
        Key::Down => Key::letter(b'B', mods, app),
        Key::Right => Key::letter(b'C', mods, app),
        Key::Left => Key::letter(b'D', mods, app),
        Key::Home => Key::letter(b'H', mods, app),
        Key::End => Key::letter(b'F', mods, app),
        Key::Insert => Key::tilde(2, mods),
        Key::Delete => Key::tilde(3, mods),
        Key::PageUp => Key::tilde(5, mods),
        Key::PageDown => Key::tilde(6, mods),
        Key::F(n) => match n {
            // F1..F4 are SS3-introduced (ESC O P..S), matching xterm's default vt100 keypad.
            1 => vec![ESC, b'O', b'P'],
            2 => vec![ESC, b'O', b'Q'],
            3 => vec![ESC, b'O', b'R'],
            4 => vec![ESC, b'O', b'S'],
            // F5..F12 use the CSI `~` editing-key form with fixed key numbers.
            5 => Key::tilde(15, Mods::empty()),
            6 => Key::tilde(17, Mods::empty()),
            7 => Key::tilde(18, Mods::empty()),
            8 => Key::tilde(19, Mods::empty()),
            9 => Key::tilde(20, Mods::empty()),
            10 => Key::tilde(21, Mods::empty()),
            11 => Key::tilde(23, Mods::empty()),
            12 => Key::tilde(24, Mods::empty()),
            _ => Vec::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(key: Key, mods: Mods) -> Vec<u8> {
        encode_key(key, mods, CursorKeys::Normal)
    }

    #[test]
    fn plain_char() {
        assert_eq!(enc(Key::Char('a'), Mods::empty()), b"a");
        // A pre-shifted char is supplied by the caller; SHIFT alone changes nothing here.
        assert_eq!(enc(Key::Char('A'), Mods::SHIFT), b"A");
    }

    #[test]
    fn plain_char_utf8() {
        assert_eq!(enc(Key::Char('é'), Mods::empty()), "é".as_bytes());
        assert_eq!(enc(Key::Char('→'), Mods::empty()), "→".as_bytes());
    }

    #[test]
    fn ctrl_a_and_ctrl_c() {
        assert_eq!(enc(Key::Char('a'), Mods::CTRL), vec![0x01]);
        assert_eq!(enc(Key::Char('A'), Mods::CTRL), vec![0x01]);
        assert_eq!(enc(Key::Char('c'), Mods::CTRL), vec![0x03]);
    }

    #[test]
    fn ctrl_special_bytes() {
        assert_eq!(enc(Key::Char(' '), Mods::CTRL), vec![0x00]); // Ctrl+Space = NUL
        assert_eq!(enc(Key::Char('['), Mods::CTRL), vec![0x1b]); // Ctrl+[ = ESC
        assert_eq!(enc(Key::Char('?'), Mods::CTRL), vec![0x7f]); // Ctrl+? = DEL
                                                                 // A char with no control mapping passes through unchanged.
        assert_eq!(enc(Key::Char('1'), Mods::CTRL), b"1");
    }

    #[test]
    fn alt_x_prefixes_esc() {
        assert_eq!(enc(Key::Char('x'), Mods::ALT), vec![ESC, b'x']);
    }

    #[test]
    fn ctrl_alt_combines() {
        // Alt+Ctrl+a = ESC then the control byte.
        assert_eq!(enc(Key::Char('a'), Mods::CTRL | Mods::ALT), vec![ESC, 0x01]);
    }

    #[test]
    fn enter_backspace_tab_esc() {
        assert_eq!(enc(Key::Enter, Mods::empty()), vec![0x0d]);
        assert_eq!(enc(Key::Backspace, Mods::empty()), vec![0x7f]);
        assert_eq!(enc(Key::Tab, Mods::empty()), vec![b'\t']);
        assert_eq!(enc(Key::Escape, Mods::empty()), vec![0x1b]);
    }

    #[test]
    fn shift_tab_is_back_tab() {
        assert_eq!(enc(Key::Tab, Mods::SHIFT), b"\x1b[Z");
    }

    #[test]
    fn arrows_normal_mode() {
        assert_eq!(enc(Key::Up, Mods::empty()), b"\x1b[A");
        assert_eq!(enc(Key::Down, Mods::empty()), b"\x1b[B");
        assert_eq!(enc(Key::Right, Mods::empty()), b"\x1b[C");
        assert_eq!(enc(Key::Left, Mods::empty()), b"\x1b[D");
    }

    #[test]
    fn arrows_application_mode() {
        let app = CursorKeys::Application;
        assert_eq!(encode_key(Key::Up, Mods::empty(), app), b"\x1bOA");
        assert_eq!(encode_key(Key::Down, Mods::empty(), app), b"\x1bOB");
        assert_eq!(encode_key(Key::Right, Mods::empty(), app), b"\x1bOC");
        assert_eq!(encode_key(Key::Left, Mods::empty(), app), b"\x1bOD");
    }

    #[test]
    fn ctrl_arrow_is_modified() {
        // Ctrl → xterm mod param 1 + 4 = 5.
        assert_eq!(enc(Key::Right, Mods::CTRL), b"\x1b[1;5C");
        assert_eq!(enc(Key::Left, Mods::CTRL), b"\x1b[1;5D");
        // Even in application mode, a modifier forces the CSI `1;m` form.
        assert_eq!(
            encode_key(Key::Up, Mods::CTRL, CursorKeys::Application),
            b"\x1b[1;5A"
        );
    }

    #[test]
    fn shift_alt_arrow_mod_param() {
        // Shift(1) + Alt(2) → 1 + 1 + 2 = 4.
        assert_eq!(enc(Key::Down, Mods::SHIFT | Mods::ALT), b"\x1b[1;4B");
        // Shift + Ctrl → 1 + 1 + 4 = 6.
        assert_eq!(enc(Key::Up, Mods::SHIFT | Mods::CTRL), b"\x1b[1;6A");
    }

    #[test]
    fn home_and_end() {
        assert_eq!(enc(Key::Home, Mods::empty()), b"\x1b[H");
        assert_eq!(enc(Key::End, Mods::empty()), b"\x1b[F");
        assert_eq!(enc(Key::Home, Mods::CTRL), b"\x1b[1;5H");
        assert_eq!(enc(Key::End, Mods::SHIFT), b"\x1b[1;2F");
    }

    #[test]
    fn editing_keys_tilde() {
        assert_eq!(enc(Key::Insert, Mods::empty()), b"\x1b[2~");
        assert_eq!(enc(Key::Delete, Mods::empty()), b"\x1b[3~");
        assert_eq!(enc(Key::PageUp, Mods::empty()), b"\x1b[5~");
        assert_eq!(enc(Key::PageDown, Mods::empty()), b"\x1b[6~");
    }

    #[test]
    fn editing_keys_tilde_modified() {
        // Ctrl+Delete → ESC [ 3 ; 5 ~
        assert_eq!(enc(Key::Delete, Mods::CTRL), b"\x1b[3;5~");
        // Shift+PageUp → ESC [ 5 ; 2 ~
        assert_eq!(enc(Key::PageUp, Mods::SHIFT), b"\x1b[5;2~");
    }

    #[test]
    fn function_keys() {
        assert_eq!(enc(Key::F(1), Mods::empty()), b"\x1bOP");
        assert_eq!(enc(Key::F(2), Mods::empty()), b"\x1bOQ");
        assert_eq!(enc(Key::F(3), Mods::empty()), b"\x1bOR");
        assert_eq!(enc(Key::F(4), Mods::empty()), b"\x1bOS");
        assert_eq!(enc(Key::F(5), Mods::empty()), b"\x1b[15~");
        assert_eq!(enc(Key::F(6), Mods::empty()), b"\x1b[17~");
        assert_eq!(enc(Key::F(10), Mods::empty()), b"\x1b[21~");
        assert_eq!(enc(Key::F(11), Mods::empty()), b"\x1b[23~");
        assert_eq!(enc(Key::F(12), Mods::empty()), b"\x1b[24~");
    }

    #[test]
    fn unknown_function_key_is_empty() {
        assert_eq!(enc(Key::F(13), Mods::empty()), Vec::<u8>::new());
    }

    #[test]
    fn paste_raw_vs_bracketed() {
        assert_eq!(PasteMode::Raw.encode("hi\nthere"), b"hi\nthere");
        assert_eq!(
            PasteMode::Bracketed.encode("hi"),
            b"\x1b[200~hi\x1b[201~".to_vec()
        );
        // Nothing inside the payload is stripped or escaped.
        assert_eq!(
            PasteMode::Bracketed.encode("a\x1b[201~b"),
            b"\x1b[200~a\x1b[201~b\x1b[201~".to_vec()
        );
    }
}
