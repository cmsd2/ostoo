//! Wire-format types shared between kernel and userspace.

use core::sync::atomic::AtomicU32;

/// Submission entry — shared layout between userspace and kernel.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct IoSubmission {
    pub user_data: u64,
    pub opcode: u32,
    pub flags: u32,
    pub fd: i32,
    pub _pad: i32,
    pub buf_addr: u64,
    pub buf_len: u32,
    pub offset: u32,
    pub timeout_ns: u64,
}

impl Default for IoSubmission {
    fn default() -> Self {
        IoSubmission {
            user_data: 0,
            opcode: 0,
            flags: 0,
            fd: -1,
            _pad: 0,
            buf_addr: 0,
            buf_len: 0,
            offset: 0,
            timeout_ns: 0,
        }
    }
}

/// Completion entry — shared layout between userspace and kernel.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct IoCompletion {
    pub user_data: u64,
    pub result: i64,
    pub flags: u32,
    pub opcode: u32,
}

/// Ring buffer header — shared between kernel and userspace.
///
/// `head` and `tail` are accessed atomically since they are shared across
/// address spaces (kernel writes, userspace reads, or vice versa).
#[repr(C)]
pub struct RingHeader {
    pub head: AtomicU32,
    pub tail: AtomicU32,
    pub mask: u32,
    pub flags: u32,
}

// Size assertions
const _: () = assert!(core::mem::size_of::<IoSubmission>() == 48);
const _: () = assert!(core::mem::size_of::<IoCompletion>() == 24);
