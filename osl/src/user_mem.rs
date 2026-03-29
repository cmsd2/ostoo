//! User-space memory access helpers.
//!
//! Centralises pointer validation and unsafe slice construction so that
//! individual syscall handlers don't each re-implement the same patterns.

use alloc::string::String;
use alloc::vec::Vec;

use crate::errno;

/// Upper bound of valid user-space addresses (non-canonical boundary).
pub const USER_LIMIT: u64 = 0x0000_8000_0000_0000;

/// Validate that a user buffer `[ptr .. ptr+len)` is within user address space.
pub fn validate_user_buf(ptr: u64, len: u64) -> bool {
    ptr != 0
        && len <= USER_LIMIT
        && ptr.checked_add(len).map_or(false, |end| end <= USER_LIMIT)
}

/// Return a shared slice over user memory, or `-EFAULT` if the pointer is invalid.
///
/// # Safety
///
/// The caller must ensure the current page tables actually map `[ptr..ptr+len)`
/// with at least read permission and that no mutable alias exists.
pub fn user_slice(ptr: u64, len: u64) -> Result<&'static [u8], i64> {
    if !validate_user_buf(ptr, len) {
        return Err(-errno::EFAULT);
    }
    Ok(unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) })
}

/// Return a mutable slice over user memory, or `-EFAULT` if the pointer is invalid.
///
/// # Safety
///
/// The caller must ensure the current page tables actually map `[ptr..ptr+len)`
/// with write permission and that no other alias exists.
pub fn user_slice_mut(ptr: u64, len: u64) -> Result<&'static mut [u8], i64> {
    if !validate_user_buf(ptr, len) {
        return Err(-errno::EFAULT);
    }
    Ok(unsafe { core::slice::from_raw_parts_mut(ptr as *mut u8, len as usize) })
}

/// Read a null-terminated string from user space. Returns `-EFAULT` on bad pointer.
pub fn read_user_string(ptr: u64, max_len: usize) -> Result<String, i64> {
    if ptr == 0 || ptr >= USER_LIMIT {
        return Err(-errno::EFAULT);
    }
    let mut len = 0usize;
    while len < max_len {
        let addr = ptr + len as u64;
        if addr >= USER_LIMIT {
            return Err(-errno::EFAULT);
        }
        let b = unsafe { *(addr as *const u8) };
        if b == 0 {
            break;
        }
        len += 1;
    }
    let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len) };
    core::str::from_utf8(bytes)
        .map(|s| String::from(s))
        .map_err(|_| -errno::EINVAL)
}

/// Read a NULL-terminated array of `char*` pointers from userspace.
///
/// Each pointer in the array is followed until a NULL sentinel.
/// Returns the collected strings, or a negative errno on failure.
pub fn read_user_string_array(ptr: u64) -> Result<Vec<String>, i64> {
    let mut result = Vec::new();
    if ptr == 0 {
        return Ok(result);
    }
    let mut i = 0usize;
    loop {
        let entry_addr = ptr + (i * 8) as u64;
        if entry_addr + 8 > USER_LIMIT {
            return Err(-errno::EFAULT);
        }
        let str_ptr = unsafe { *(entry_addr as *const u64) };
        if str_ptr == 0 {
            break; // NULL terminator
        }
        result.push(read_user_string(str_ptr, 4096)?);
        i += 1;
        if i > 256 {
            return Err(-errno::EINVAL); // safety limit
        }
    }
    Ok(result)
}
