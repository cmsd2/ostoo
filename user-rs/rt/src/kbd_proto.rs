//! Keyboard service wire protocol constants.
//!
//! The userspace keyboard driver registers as `"keyboard\0"` and accepts
//! `MSG_KB_CONNECT` messages.  Key events are delivered via `MSG_KB_KEY`.

/// Well-known service name (null-terminated).
pub const SERVICE_NAME: &[u8] = b"keyboard\0";

/// Client → keyboard service (registration channel): connect.
///
/// `data = [0, 0, 0]`
/// `fds  = [event_send_fd, -1, -1, -1]`  — client passes a send-end for events
pub const MSG_KB_CONNECT: u64 = 1;

/// Keyboard service → client (via passed channel): key event.
///
/// `data = [byte, modifiers, key_type]`
/// `fds  = [-1, -1, -1, -1]`
///
/// `key_type`: 0 = ASCII byte, 1 = special key (arrow, F-key, etc.)
/// `modifiers`: bitmask (bit 0 = shift, bit 1 = ctrl, bit 2 = alt)
pub const MSG_KB_KEY: u64 = 1;

// Modifier bitmask values
pub const MOD_SHIFT: u64 = 1;
pub const MOD_CTRL: u64 = 2;
pub const MOD_ALT: u64 = 4;

// Key types
pub const KEY_ASCII: u64 = 0;
pub const KEY_SPECIAL: u64 = 1;
