//! Notification fd syscall implementations: notify_create, notify.

use alloc::sync::Arc;

use libkernel::file::{FdObject, FD_CLOEXEC};
use libkernel::irq_mutex::IrqMutex;
use libkernel::notify::NotifyInner;
use libkernel::task::scheduler;

use crate::errno;
use crate::fd_helpers;

/// Flag: set close-on-exec on the returned fd.
const NOTIFY_CLOEXEC: u32 = 0x01;

/// `notify_create(flags) → fd`
///
/// Create a notification file descriptor for inter-process signaling.
/// The returned fd can be armed with `OP_RING_WAIT` via `io_submit` and
/// signaled with `notify(fd)`.
pub fn sys_notify_create(flags: u32) -> i64 {
    if flags & !NOTIFY_CLOEXEC != 0 {
        return -errno::EINVAL;
    }

    let inner = Arc::new(IrqMutex::new(NotifyInner::new()));
    let fd_flags = if flags & NOTIFY_CLOEXEC != 0 { FD_CLOEXEC } else { 0 };
    let obj = FdObject::Notify(inner);

    match fd_helpers::alloc_fd_with_flags(obj, fd_flags) {
        Ok(fd) => fd as i64,
        Err(e) => e,
    }
}

/// `notify(fd) → 0 or -errno`
///
/// Signal a notification fd, waking a consumer waiting via `OP_RING_WAIT`.
/// If no consumer is waiting, the notification is buffered (coalesced) so
/// the next `OP_RING_WAIT` completes immediately.
pub fn sys_notify(fd: i32) -> i64 {
    let inner = match fd_helpers::get_fd_notify(fd as usize) {
        Ok(n) => n,
        Err(e) => return e,
    };

    if let Some(thread_idx) = libkernel::notify::signal_notify(&inner) {
        scheduler::set_donate_target(thread_idx);
        scheduler::yield_now();
    }

    0
}
