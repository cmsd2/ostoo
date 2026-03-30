//! Compositor wire protocol constants.
//!
//! Defines message tags and the well-known service name used by the
//! compositor and its clients to communicate over IPC channels.

/// Well-known service name (null-terminated).
pub const SERVICE_NAME: &[u8] = b"compositor\0";

/// Client → compositor (registration channel): request a new window.
///
/// `data = [width, height, 0]`
/// `fds  = [c2s_recv, s2c_send, -1, -1]`
pub const MSG_CONNECT: u64 = 1;

/// Compositor → client (per-client channel): window created.
///
/// `data = [window_id, width, height]`
/// `fds  = [buffer_fd, damage_notify_fd, -1, -1]`
pub const MSG_WINDOW_CREATED: u64 = 2;

/// Client → compositor (per-client channel): present the buffer.
///
/// `data = [window_id, 0, 0]`
/// `fds  = [-1, -1, -1, -1]`
pub const MSG_PRESENT: u64 = 3;

/// Client → compositor (per-client channel): close the window.
///
/// `data = [window_id, 0, 0]`
/// `fds  = [-1, -1, -1, -1]`
pub const MSG_CLOSE: u64 = 4;

/// Compositor → client (per-client channel): key event.
///
/// `data = [byte, modifiers, key_type]`
/// `fds  = [-1, -1, -1, -1]`
pub const MSG_KEY_EVENT: u64 = 5;

/// Compositor → client (per-client channel): mouse event (reserved for future use).
///
/// `data = [x, y, buttons]`
/// `fds  = [-1, -1, -1, -1]`
pub const MSG_MOUSE_EVENT: u64 = 6;

/// Compositor → client (per-client channel): window resized.
///
/// `data = [new_width, new_height, 0]`
/// `fds  = [new_buffer_fd, -1, -1, -1]`
pub const MSG_WINDOW_RESIZED: u64 = 7;
