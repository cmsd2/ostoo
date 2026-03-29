//! Filesystem syscalls: open, close, chdir, getcwd, fstat, dup2, fcntl, pipe2.

use alloc::sync::Arc;

use crate::errno;
use crate::fd_helpers;
use crate::user_mem::{validate_user_buf, read_user_string, user_slice_mut};
use libkernel::file::{FileHandle, FdEntry, FdObject, FD_CLOEXEC};
use libkernel::process;

use super::{resolve_user_path, vfs_read_file, vfs_list_dir};

pub(crate) fn sys_open(path_ptr: u64, flags: u64, _mode: u64) -> i64 {
    let path = match read_user_string(path_ptr, 4096) {
        Ok(p) => p,
        Err(e) => return e,
    };

    let resolved = resolve_user_path(&path);
    let pid = process::current_pid();

    const O_DIRECTORY: u64 = 0o200000;
    let want_dir = flags & O_DIRECTORY != 0;

    if !want_dir {
        match vfs_read_file(&resolved, pid) {
            Ok(data) => {
                let handle: Arc<dyn FileHandle> = Arc::new(crate::file::VfsHandle::new(data));
                return match fd_helpers::alloc_fd(FdObject::File(handle)) {
                    Ok(fd) => fd as i64,
                    Err(e) => e,
                };
            }
            Err(devices::vfs::VfsError::NotFound) | Err(devices::vfs::VfsError::NotAFile) => {
                // Fall through to try as directory.
            }
            Err(ref e) => return errno::vfs_errno(e),
        }
    }

    match vfs_list_dir(&resolved) {
        Ok(entries) => {
            let handle: Arc<dyn FileHandle> = Arc::new(crate::file::DirHandle::new(entries));
            match fd_helpers::alloc_fd(FdObject::File(handle)) {
                Ok(fd) => fd as i64,
                Err(e) => e,
            }
        }
        Err(ref e) => errno::vfs_errno(e),
    }
}

pub(crate) fn sys_close(fd: u64) -> i64 {
    let pid = process::current_pid();
    let result = process::with_process(pid, |p| p.close_fd(fd as usize));
    match result {
        Some(Ok(woken)) => {
            if let Some(thread_idx) = woken {
                libkernel::task::scheduler::set_donate_target(thread_idx);
                libkernel::task::scheduler::yield_now();
            }
            0
        }
        Some(Err(e)) => errno::file_errno(e),
        None => -errno::EBADF,
    }
}

pub(crate) fn sys_fstat(_fd: u64, buf: u64) -> i64 {
    const STAT_SIZE: usize = 144;
    const S_IFCHR: u32 = 0o020000;
    let stat_ptr = buf as *mut u8;
    unsafe {
        core::ptr::write_bytes(stat_ptr, 0, STAT_SIZE);
        let mode_ptr = stat_ptr.add(24) as *mut u32;
        mode_ptr.write(S_IFCHR | 0o666);
    }
    0
}

pub(crate) fn sys_getcwd(buf: u64, size: u64) -> i64 {
    if !validate_user_buf(buf, size) {
        return -errno::EFAULT;
    }
    let pid = process::current_pid();
    let cwd = match process::with_process_ref(pid, |p| p.cwd.clone()) {
        Some(c) => c,
        None => return -errno::EFAULT,
    };
    let cwd_bytes = cwd.as_bytes();
    let needed = cwd_bytes.len() + 1;
    if needed > size as usize {
        return -errno::ERANGE;
    }
    let user_buf = match user_slice_mut(buf, needed as u64) {
        Ok(s) => s,
        Err(e) => return e,
    };
    user_buf[..cwd_bytes.len()].copy_from_slice(cwd_bytes);
    user_buf[cwd_bytes.len()] = 0;
    buf as i64
}

pub(crate) fn sys_chdir(path_ptr: u64) -> i64 {
    let path = match read_user_string(path_ptr, 4096) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let resolved = resolve_user_path(&path);

    match vfs_list_dir(&resolved) {
        Ok(_) => {
            let pid = process::current_pid();
            process::with_process(pid, |p| {
                p.cwd = resolved;
            });
            0
        }
        Err(ref e) => errno::vfs_errno(e),
    }
}

pub(crate) fn sys_fcntl(fd: u64, cmd: u64, arg: u64) -> i64 {
    const F_GETFD: u64 = 1;
    const F_SETFD: u64 = 2;
    const F_GETFL: u64 = 3;

    let pid = process::current_pid();
    match cmd {
        F_GETFD => {
            match process::with_process_ref(pid, |p| p.get_fd_flags(fd as usize)) {
                Some(Ok(flags)) => flags as i64,
                _ => -errno::EBADF,
            }
        }
        F_SETFD => {
            match process::with_process(pid, |p| p.set_fd_flags(fd as usize, arg as u32)) {
                Some(Ok(())) => 0,
                _ => -errno::EBADF,
            }
        }
        F_GETFL => 0,
        _ => -errno::EINVAL,
    }
}

pub(crate) fn sys_dup2(oldfd: u64, newfd: u64) -> i64 {
    let oldfd = oldfd as usize;
    let newfd = newfd as usize;
    if oldfd == newfd {
        let pid = process::current_pid();
        return match process::with_process_ref(pid, |p| p.get_fd(oldfd)) {
            Some(Ok(_)) => newfd as i64,
            _ => -errno::EBADF,
        };
    }
    let pid = process::current_pid();
    match process::with_process(pid, |p| {
        let entry = p.get_fd_entry(oldfd)?;
        entry.object.notify_dup();
        p.set_fd(newfd, FdEntry::from_object(entry.object, 0));
        Ok(newfd)
    }) {
        Some(Ok(fd)) => fd as i64,
        Some(Err(e)) => errno::file_errno(e),
        None => -errno::EBADF,
    }
}

pub(crate) fn sys_pipe2(fds_ptr: u64, flags: u64) -> i64 {
    const O_CLOEXEC: u64 = 0o2000000;

    if !validate_user_buf(fds_ptr, 8) {
        return -errno::EFAULT;
    }

    let (reader, writer) = libkernel::file::make_pipe();
    let fd_flags = if flags & O_CLOEXEC != 0 { FD_CLOEXEC } else { 0 };

    let pid = process::current_pid();
    match process::with_process(pid, |p| {
        let rfd = p.alloc_fd_with_flags(FdObject::File(Arc::new(reader)), fd_flags)?;
        let wfd = match p.alloc_fd_with_flags(FdObject::File(Arc::new(writer)), fd_flags) {
            Ok(fd) => fd,
            Err(e) => {
                p.close_fd(rfd).ok();
                return Err(e);
            }
        };
        Ok((rfd, wfd))
    }) {
        Some(Ok((rfd, wfd))) => {
            let fds = unsafe { core::slice::from_raw_parts_mut(fds_ptr as *mut i32, 2) };
            fds[0] = rfd as i32;
            fds[1] = wfd as i32;
            0
        }
        Some(Err(e)) => errno::file_errno(e),
        None => -errno::EBADF,
    }
}
