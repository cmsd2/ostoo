//! Shared memory syscalls.

use alloc::sync::Arc;

use libkernel::file::{FdObject, FD_CLOEXEC};
use libkernel::shmem::SharedMemInner;

use crate::errno;
use crate::fd_helpers;

/// Flag: set close-on-exec on the returned fd (matches Linux MFD_CLOEXEC).
const SHM_CLOEXEC: u32 = 0x01;

/// `shmem_create(size, flags) → fd`
///
/// Allocate a shared memory object backed by zeroed physical frames and
/// return a file descriptor referring to it.  The fd can be passed to
/// child processes (via inheritance or IPC fd-passing) and both sides
/// can `mmap(MAP_SHARED, fd)` to share the same physical pages.
///
/// Flags: `SHM_CLOEXEC` (0x01) — set close-on-exec on the fd.
pub(crate) fn sys_shmem_create(size: u64, flags: u32) -> i64 {
    if size == 0 {
        return -errno::EINVAL;
    }

    // Reject unknown flags.
    if flags & !SHM_CLOEXEC != 0 {
        return -errno::EINVAL;
    }

    let inner = match SharedMemInner::new(size as usize) {
        Some(s) => s,
        None => return -errno::ENOMEM,
    };

    let fd_flags = if flags & SHM_CLOEXEC != 0 { FD_CLOEXEC } else { 0 };
    let obj = FdObject::SharedMem(Arc::new(inner));
    match fd_helpers::alloc_fd_with_flags(obj, fd_flags) {
        Ok(fd) => fd as i64,
        Err(e) => e,
    }
}
