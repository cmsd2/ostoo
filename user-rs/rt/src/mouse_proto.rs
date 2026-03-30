//! Mouse service wire protocol constants.
//!
//! The userspace mouse driver registers as `"mouse\0"` and accepts
//! `MSG_MOUSE_CONNECT` messages.  Mouse events are delivered via `MSG_MOUSE_MOVE`.

/// Well-known service name (null-terminated).
pub const SERVICE_NAME: &[u8] = b"mouse\0";

/// Client → mouse service (registration channel): connect.
///
/// `data = [0, 0, 0]`
/// `fds  = [event_send_fd, -1, -1, -1]`  — client passes a send-end for events
pub const MSG_MOUSE_CONNECT: u64 = 1;

/// Mouse service → client (via passed channel): mouse move/button event.
///
/// `data = [x, y, buttons]`
/// `fds  = [-1, -1, -1, -1]`
///
/// `buttons`: bitmask (bit 0 = left, bit 1 = right, bit 2 = middle)
pub const MSG_MOUSE_MOVE: u64 = 1;

// Button bitmask values
pub const BTN_LEFT: u64 = 1;
pub const BTN_RIGHT: u64 = 2;
pub const BTN_MIDDLE: u64 = 4;
