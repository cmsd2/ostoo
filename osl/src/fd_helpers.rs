//! File-descriptor lookup and allocation helpers.
//!
//! Consolidates the repeated pattern of extracting a specific `FdObject`
//! variant from the current process's fd table.

use alloc::sync::Arc;

use libkernel::completion_port::CompletionPort;
use libkernel::file::{FileHandle, FdObject};
use libkernel::irq_handle::IrqInner;
use libkernel::irq_mutex::IrqMutex;
use libkernel::process;
use libkernel::notify::NotifyInner;
use libkernel::shmem::SharedMemInner;

use crate::errno;

/// Get a `FileHandle` from the current process's fd table.
///
/// Returns `-EBADF` if the fd is invalid or refers to a non-file object.
pub fn get_fd_file(fd: usize) -> Result<Arc<dyn FileHandle>, i64> {
    let pid = process::current_pid();
    match process::with_process_ref(pid, |p| p.get_fd(fd)) {
        Some(Ok(obj)) => match obj.as_file() {
            Some(h) => Ok(h.clone()),
            None => Err(-errno::EBADF),
        },
        Some(Err(e)) => Err(errno::file_errno(e)),
        None => Err(-errno::EBADF),
    }
}

/// Get a `CompletionPort` from the current process's fd table.
///
/// Returns `-EBADF` if the fd is invalid or refers to a non-port object.
pub fn get_fd_port(fd: usize) -> Result<Arc<IrqMutex<CompletionPort>>, i64> {
    let pid = process::current_pid();
    match process::with_process_ref(pid, |p| p.get_fd(fd)) {
        Some(Ok(obj)) => match obj.as_port() {
            Some(p) => Ok(p.clone()),
            None => Err(-errno::EBADF),
        },
        _ => Err(-errno::EBADF),
    }
}

/// Get an `IrqInner` handle from the current process's fd table.
///
/// Returns `-EBADF` if the fd is invalid or refers to a non-IRQ object.
pub fn get_fd_irq(fd: usize) -> Result<Arc<IrqMutex<IrqInner>>, i64> {
    let pid = process::current_pid();
    match process::with_process_ref(pid, |p| p.get_fd(fd)) {
        Some(Ok(obj)) => match obj.as_irq() {
            Some(i) => Ok(i.clone()),
            None => Err(-errno::EBADF),
        },
        _ => Err(-errno::EBADF),
    }
}

/// Get a `SharedMemInner` from the current process's fd table.
///
/// Returns `-EBADF` if the fd is invalid or refers to a non-shmem object.
pub fn get_fd_shmem(fd: usize) -> Result<Arc<SharedMemInner>, i64> {
    let pid = process::current_pid();
    match process::with_process_ref(pid, |p| p.get_fd(fd)) {
        Some(Ok(obj)) => match obj.as_shmem() {
            Some(s) => Ok(s.clone()),
            None => Err(-errno::EBADF),
        },
        _ => Err(-errno::EBADF),
    }
}

/// Get a `NotifyInner` handle from the current process's fd table.
///
/// Returns `-EBADF` if the fd is invalid or refers to a non-notify object.
pub fn get_fd_notify(fd: usize) -> Result<Arc<IrqMutex<NotifyInner>>, i64> {
    let pid = process::current_pid();
    match process::with_process_ref(pid, |p| p.get_fd(fd)) {
        Some(Ok(obj)) => match obj.as_notify() {
            Some(n) => Ok(n.clone()),
            None => Err(-errno::EBADF),
        },
        _ => Err(-errno::EBADF),
    }
}

/// Allocate a file descriptor for the given object in the current process.
///
/// Returns the fd number on success, or a negative errno on failure.
pub fn alloc_fd(obj: FdObject) -> Result<usize, i64> {
    alloc_fd_with_flags(obj, 0)
}

/// Allocate a file descriptor with flags (e.g. `FD_CLOEXEC`) in the current process.
///
/// Returns the fd number on success, or a negative errno on failure.
pub fn alloc_fd_with_flags(obj: FdObject, flags: u32) -> Result<usize, i64> {
    let pid = process::current_pid();
    match process::with_process(pid, |p| p.alloc_fd_with_flags(obj, flags)) {
        Some(Ok(fd)) => Ok(fd),
        Some(Err(e)) => Err(errno::file_errno(e)),
        None => Err(-errno::EBADF),
    }
}
