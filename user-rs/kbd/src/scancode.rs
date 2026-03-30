//! Scancode Set 1 decoder state machine.
//!
//! Translates raw PS/2 scancodes into `(byte, modifiers, key_type)` tuples.

use ostoo_rt::kbd_proto::{KEY_ASCII, KEY_SPECIAL, MOD_SHIFT, MOD_CTRL, MOD_ALT};

/// State of the scancode decoder.
pub struct ScancodeDecoder {
    extended: bool,
    shift: bool,
    ctrl: bool,
    alt: bool,
}

/// Decoded key event.
pub struct KeyEvent {
    pub byte: u8,
    pub modifiers: u64,
    pub key_type: u64,
}

impl ScancodeDecoder {
    pub fn new() -> Self {
        ScancodeDecoder {
            extended: false,
            shift: false,
            ctrl: false,
            alt: false,
        }
    }

    /// Feed a raw scancode byte. Returns `Some(KeyEvent)` on a key press.
    /// Returns `None` for key releases, modifier updates, and incomplete sequences.
    pub fn feed(&mut self, code: u8) -> Option<KeyEvent> {
        if code == 0xE0 {
            self.extended = true;
            return None;
        }

        let is_release = code & 0x80 != 0;
        let make_code = code & 0x7F;

        if self.extended {
            self.extended = false;
            return self.handle_extended(make_code, is_release);
        }

        // Handle modifier keys
        match make_code {
            0x2A | 0x36 => {
                // Left/Right Shift
                self.shift = !is_release;
                return None;
            }
            0x1D => {
                // Left Ctrl
                self.ctrl = !is_release;
                return None;
            }
            0x38 => {
                // Left Alt
                self.alt = !is_release;
                return None;
            }
            _ => {}
        }

        if is_release {
            return None;
        }

        // Translate make code to ASCII
        let ch = if self.shift {
            SCANCODE_SHIFT[make_code as usize]
        } else {
            SCANCODE_NORMAL[make_code as usize]
        };

        if ch == 0 {
            return None;
        }

        let mut byte = ch;
        // Ctrl+letter → control character (1–26)
        if self.ctrl && byte >= b'a' && byte <= b'z' {
            byte = byte - b'a' + 1;
        } else if self.ctrl && byte >= b'A' && byte <= b'Z' {
            byte = byte - b'A' + 1;
        }

        Some(KeyEvent {
            byte,
            modifiers: self.modifier_bits(),
            key_type: KEY_ASCII,
        })
    }

    fn handle_extended(&mut self, make_code: u8, is_release: bool) -> Option<KeyEvent> {
        if is_release {
            // Right Ctrl release
            if make_code == 0x1D {
                self.ctrl = false;
            }
            return None;
        }

        // Right Ctrl press
        if make_code == 0x1D {
            self.ctrl = true;
            return None;
        }

        // Extended key codes (arrows, etc.)
        let special = match make_code {
            0x48 => b'A', // Up arrow → ESC [ A
            0x50 => b'B', // Down arrow → ESC [ B
            0x4D => b'C', // Right arrow → ESC [ C
            0x4B => b'D', // Left arrow → ESC [ D
            0x47 => b'H', // Home → ESC [ H
            0x4F => b'F', // End → ESC [ F
            0x53 => 127,  // Delete
            _ => return None,
        };

        Some(KeyEvent {
            byte: special,
            modifiers: self.modifier_bits(),
            key_type: KEY_SPECIAL,
        })
    }

    fn modifier_bits(&self) -> u64 {
        let mut m = 0u64;
        if self.shift {
            m |= MOD_SHIFT;
        }
        if self.ctrl {
            m |= MOD_CTRL;
        }
        if self.alt {
            m |= MOD_ALT;
        }
        m
    }
}

/// Normal (unshifted) scancode-to-ASCII table for set 1.
/// Index = make code (0x00–0x7F), value = ASCII byte (0 = no mapping).
#[rustfmt::skip]
static SCANCODE_NORMAL: [u8; 128] = {
    let mut t = [0u8; 128];
    t[0x01] = 0x1B; // Esc
    t[0x02] = b'1'; t[0x03] = b'2'; t[0x04] = b'3'; t[0x05] = b'4';
    t[0x06] = b'5'; t[0x07] = b'6'; t[0x08] = b'7'; t[0x09] = b'8';
    t[0x0A] = b'9'; t[0x0B] = b'0'; t[0x0C] = b'-'; t[0x0D] = b'=';
    t[0x0E] = 0x7F; // Backspace → DEL (matches what terminals send)
    t[0x0F] = b'\t';
    t[0x10] = b'q'; t[0x11] = b'w'; t[0x12] = b'e'; t[0x13] = b'r';
    t[0x14] = b't'; t[0x15] = b'y'; t[0x16] = b'u'; t[0x17] = b'i';
    t[0x18] = b'o'; t[0x19] = b'p'; t[0x1A] = b'['; t[0x1B] = b']';
    t[0x1C] = b'\n'; // Enter
    // 0x1D = Ctrl (handled separately)
    t[0x1E] = b'a'; t[0x1F] = b's'; t[0x20] = b'd'; t[0x21] = b'f';
    t[0x22] = b'g'; t[0x23] = b'h'; t[0x24] = b'j'; t[0x25] = b'k';
    t[0x26] = b'l'; t[0x27] = b';'; t[0x28] = b'\''; t[0x29] = b'`';
    // 0x2A = LShift
    t[0x2B] = b'\\';
    t[0x2C] = b'z'; t[0x2D] = b'x'; t[0x2E] = b'c'; t[0x2F] = b'v';
    t[0x30] = b'b'; t[0x31] = b'n'; t[0x32] = b'm'; t[0x33] = b',';
    t[0x34] = b'.'; t[0x35] = b'/';
    // 0x36 = RShift
    t[0x37] = b'*'; // Keypad *
    // 0x38 = Alt
    t[0x39] = b' '; // Space
    t
};

/// Shifted scancode-to-ASCII table for set 1.
#[rustfmt::skip]
static SCANCODE_SHIFT: [u8; 128] = {
    let mut t = [0u8; 128];
    t[0x01] = 0x1B;
    t[0x02] = b'!'; t[0x03] = b'@'; t[0x04] = b'#'; t[0x05] = b'$';
    t[0x06] = b'%'; t[0x07] = b'^'; t[0x08] = b'&'; t[0x09] = b'*';
    t[0x0A] = b'('; t[0x0B] = b')'; t[0x0C] = b'_'; t[0x0D] = b'+';
    t[0x0E] = 0x7F;
    t[0x0F] = b'\t';
    t[0x10] = b'Q'; t[0x11] = b'W'; t[0x12] = b'E'; t[0x13] = b'R';
    t[0x14] = b'T'; t[0x15] = b'Y'; t[0x16] = b'U'; t[0x17] = b'I';
    t[0x18] = b'O'; t[0x19] = b'P'; t[0x1A] = b'{'; t[0x1B] = b'}';
    t[0x1C] = b'\n';
    t[0x1E] = b'A'; t[0x1F] = b'S'; t[0x20] = b'D'; t[0x21] = b'F';
    t[0x22] = b'G'; t[0x23] = b'H'; t[0x24] = b'J'; t[0x25] = b'K';
    t[0x26] = b'L'; t[0x27] = b':'; t[0x28] = b'"'; t[0x29] = b'~';
    t[0x2B] = b'|';
    t[0x2C] = b'Z'; t[0x2D] = b'X'; t[0x2E] = b'C'; t[0x2F] = b'V';
    t[0x30] = b'B'; t[0x31] = b'N'; t[0x32] = b'M'; t[0x33] = b'<';
    t[0x34] = b'>'; t[0x35] = b'?';
    t[0x37] = b'*';
    t[0x39] = b' ';
    t
};
