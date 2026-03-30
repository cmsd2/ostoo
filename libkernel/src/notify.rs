//! Notification fd infrastructure — allows processes to signal each other
//! through file descriptors and completion ports.
//!
//! This is the userspace-to-userspace counterpart of `irq_handle.rs` (which
//! handles hardware-to-userspace signaling).  A process creates a notification
//! fd via `notify_create`, arms it with `OP_RING_WAIT` via `io_submit`, and
//! another process signals it with `notify(fd)`.

use alloc::sync::Arc;

use crate::completion_port::{Completion, CompletionPort, OP_RING_WAIT};
use crate::irq_mutex::IrqMutex;

// ---------------------------------------------------------------------------
// NotifyInner — per-notification-fd kernel state

pub struct NotifyInner {
    /// When OP_RING_WAIT is active: (port, user_data) to post on signal.
    pending: Option<(Arc<IrqMutex<CompletionPort>>, u64)>,
    /// True if notify() was called while no OP_RING_WAIT was pending.
    /// Consumed by arm_notify if set (buffered notification, coalescing).
    notified: bool,
}

impl NotifyInner {
    pub fn new() -> Self {
        Self {
            pending: None,
            notified: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Arm and signal — called from osl (io_submit and notify syscall)

/// Arm a notification fd for OP_RING_WAIT: register the port to post to
/// when `notify()` is called.  If a notification is already buffered,
/// complete immediately without waiting.
///
/// Returns the thread index that was woken (if any), for scheduler donate.
pub fn arm_notify(
    inner: &Arc<IrqMutex<NotifyInner>>,
    port: Arc<IrqMutex<CompletionPort>>,
    user_data: u64,
) -> Option<usize> {
    let mut guard = inner.lock();

    // If already notified, satisfy immediately (like buffered scancode).
    if guard.notified {
        guard.notified = false;
        return port.lock().post(Completion {
            user_data,
            result: 0,
            flags: 0,
            opcode: OP_RING_WAIT,
            read_buf: None,
            read_dest: 0,
            transfer_fds: None,
        });
    }

    guard.pending = Some((port, user_data));
    None
}

/// Signal a notification fd.  If OP_RING_WAIT is armed, post a completion
/// to the registered port.  Otherwise, buffer the notification for the
/// next arm (coalescing: multiple signals before an arm produce one event).
///
/// Returns the thread index that was woken (if any), for scheduler donate.
pub fn signal_notify(inner: &Arc<IrqMutex<NotifyInner>>) -> Option<usize> {
    let mut guard = inner.lock();

    if let Some((port, user_data)) = guard.pending.take() {
        port.lock().post(Completion {
            user_data,
            result: 0,
            flags: 0,
            opcode: OP_RING_WAIT,
            read_buf: None,
            read_dest: 0,
            transfer_fds: None,
        })
    } else {
        guard.notified = true;
        None
    }
}
