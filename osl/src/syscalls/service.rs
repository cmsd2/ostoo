//! Service registry syscalls (513, 514).

use crate::errno;
use crate::fd_helpers;
use crate::user_mem;
use libkernel::service;

/// `svc_register(name_ptr, fd) → 0 or -errno`
///
/// Register the fd under a null-terminated service name in the kernel-global
/// registry. The fd object is cloned (Arc ref bump) + `notify_dup()`'d.
pub(crate) fn sys_svc_register(name_ptr: u64, fd: i32) -> i64 {
    let name = match user_mem::read_user_string(name_ptr, service::MAX_SERVICE_NAME_LEN) {
        Ok(n) => n,
        Err(e) => return e,
    };
    if name.is_empty() {
        return -errno::EINVAL;
    }

    // Get the fd object from the caller's table.
    let pid = libkernel::process::current_pid();
    let obj = match libkernel::process::with_process_ref(pid, |p| p.get_fd(fd as usize)) {
        Some(Ok(obj)) => obj.clone(),
        _ => return -errno::EBADF,
    };

    // Notify that we're creating a new reference.
    obj.notify_dup();

    match service::register(name, obj) {
        Ok(()) => 0,
        Err(()) => -errno::EBUSY,
    }
}

/// `svc_lookup(name_ptr) → fd or -errno`
///
/// Look up a service by name, clone + `notify_dup()` the FdObject, and
/// allocate a new fd in the caller's table.
pub(crate) fn sys_svc_lookup(name_ptr: u64) -> i64 {
    let name = match user_mem::read_user_string(name_ptr, service::MAX_SERVICE_NAME_LEN) {
        Ok(n) => n,
        Err(e) => return e,
    };
    if name.is_empty() {
        return -errno::EINVAL;
    }

    let obj = match service::lookup(&name) {
        Some(o) => o,
        None => return -errno::ENOENT,
    };

    // The clone from lookup() already bumped Arc; notify the object.
    obj.notify_dup();

    match fd_helpers::alloc_fd(obj) {
        Ok(fd) => fd as i64,
        Err(e) => e,
    }
}
