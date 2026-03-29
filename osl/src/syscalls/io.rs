//! I/O syscalls: read, write, writev, getdents64.

use crate::errno;
use crate::fd_helpers;
use crate::user_mem::{user_slice, user_slice_mut};

pub(crate) fn sys_write(fd: u64, buf: u64, count: u64) -> i64 {
    let bytes = match user_slice(buf, count) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let handle = match fd_helpers::get_fd_file(fd as usize) {
        Ok(h) => h,
        Err(e) => return e,
    };
    match handle.write(bytes) {
        Ok(n) => n as i64,
        Err(e) => errno::file_errno(e),
    }
}

pub(crate) fn sys_read(fd: u64, buf: u64, count: u64) -> i64 {
    if count == 0 { return 0; }
    let user_buf = match user_slice_mut(buf, count) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let handle = match fd_helpers::get_fd_file(fd as usize) {
        Ok(h) => h,
        Err(e) => return e,
    };
    match handle.read(user_buf) {
        Ok(n) => n as i64,
        Err(e) => errno::file_errno(e),
    }
}

pub(crate) fn sys_writev(fd: u64, iov_ptr: u64, iovcnt: u64) -> i64 {
    let handle = match fd_helpers::get_fd_file(fd as usize) {
        Ok(h) => h,
        Err(e) => return e,
    };
    let mut total: usize = 0;
    for i in 0..iovcnt as usize {
        let iov_addr = iov_ptr + (i * 16) as u64;
        let iov_base = unsafe { *(iov_addr as *const u64) };
        let iov_len = unsafe { *((iov_addr + 8) as *const u64) } as usize;
        if iov_len == 0 {
            continue;
        }
        let bytes = unsafe { core::slice::from_raw_parts(iov_base as *const u8, iov_len) };
        match handle.write(bytes) {
            Ok(n) => total += n,
            Err(e) => return errno::file_errno(e),
        }
    }
    total as i64
}

pub(crate) fn sys_getdents64(fd: u64, buf: u64, count: u64) -> i64 {
    let user_buf = match user_slice_mut(buf, count) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let handle = match fd_helpers::get_fd_file(fd as usize) {
        Ok(h) => h,
        Err(e) => return e,
    };
    match handle.getdents64(user_buf) {
        Ok(n) => n as i64,
        Err(e) => errno::file_errno(e),
    }
}
