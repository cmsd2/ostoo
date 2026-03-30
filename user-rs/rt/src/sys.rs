//! Raw wrappers for ostoo custom syscalls (501-512).
//!
//! All functions in this module are thin syscall wrappers that return raw
//! `i64` values.  Negative return values are negated errno codes.
//!
//! Struct definitions match the kernel's `repr(C)` layouts exactly.

use crate::syscall;
use core::sync::atomic::AtomicU32;

// ---- Syscall numbers (must match osl/src/syscall_nr.rs) ----

pub const SYS_IO_CREATE: u64 = 501;
pub const SYS_IO_SUBMIT: u64 = 502;
pub const SYS_IO_WAIT: u64 = 503;
pub const SYS_IRQ_CREATE: u64 = 504;
pub const SYS_IPC_CREATE: u64 = 505;
pub const SYS_IPC_SEND: u64 = 506;
pub const SYS_IPC_RECV: u64 = 507;
pub const SYS_SHMEM_CREATE: u64 = 508;
pub const SYS_NOTIFY_CREATE: u64 = 509;
pub const SYS_NOTIFY: u64 = 510;
pub const SYS_IO_SETUP_RINGS: u64 = 511;
pub const SYS_IO_RING_ENTER: u64 = 512;

// ---- Opcodes (must match libkernel/src/completion_port.rs) ----

pub const OP_NOP: u32 = 0;
pub const OP_TIMEOUT: u32 = 1;
pub const OP_READ: u32 = 2;
pub const OP_WRITE: u32 = 3;
pub const OP_IRQ_WAIT: u32 = 4;
pub const OP_IPC_SEND: u32 = 5;
pub const OP_IPC_RECV: u32 = 6;
pub const OP_RING_WAIT: u32 = 7;

// ---- Flags ----

pub const IPC_NONBLOCK: u32 = 0x1;
pub const IPC_CLOEXEC: u32 = 0x1;
pub const SHM_CLOEXEC: u32 = 0x01;
pub const NOTIFY_CLOEXEC: u32 = 0x01;

// ---- Ring layout ----

pub const RING_ENTRIES_OFFSET: usize = 64;

// ---- Struct definitions ----

/// Submission entry — shared layout between userspace and kernel (48 bytes).
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

/// Completion entry — shared layout between userspace and kernel (24 bytes).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct IoCompletion {
    pub user_data: u64,
    pub result: i64,
    pub flags: u32,
    pub opcode: u32,
}

/// Fixed-size IPC message (48 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct IpcMessage {
    pub tag: u64,
    pub data: [u64; 3],
    pub fds: [i32; 4],
}

impl Default for IpcMessage {
    fn default() -> Self {
        IpcMessage {
            tag: 0,
            data: [0; 3],
            fds: [-1; 4],
        }
    }
}

/// Ring buffer header — shared between kernel and userspace (16 bytes).
///
/// `head` and `tail` use atomic operations since they are shared across
/// address spaces.
#[repr(C)]
pub struct RingHeader {
    pub head: AtomicU32,
    pub tail: AtomicU32,
    pub mask: u32,
    pub flags: u32,
}

/// Parameters for `io_setup_rings` (16 bytes).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct IoRingParams {
    pub sq_entries: u32,
    pub cq_entries: u32,
    pub sq_fd: i32,
    pub cq_fd: i32,
}

// Size assertions
const _: () = assert!(core::mem::size_of::<IoSubmission>() == 48);
const _: () = assert!(core::mem::size_of::<IoCompletion>() == 24);
const _: () = assert!(core::mem::size_of::<IpcMessage>() == 48);
const _: () = assert!(core::mem::size_of::<IoRingParams>() == 16);

// ---- Syscall wrappers ----

pub fn io_create(flags: u32) -> i64 {
    unsafe { syscall::syscall1(SYS_IO_CREATE, flags as u64) }
}

pub fn io_submit(port_fd: i32, entries: &[IoSubmission]) -> i64 {
    unsafe {
        syscall::syscall3(
            SYS_IO_SUBMIT,
            port_fd as u64,
            entries.as_ptr() as u64,
            entries.len() as u64,
        )
    }
}

pub fn io_wait(
    port_fd: i32,
    completions: &mut [IoCompletion],
    min: u32,
    timeout_ns: u64,
) -> i64 {
    unsafe {
        syscall::syscall5(
            SYS_IO_WAIT,
            port_fd as u64,
            completions.as_mut_ptr() as u64,
            completions.len() as u64,
            min as u64,
            timeout_ns,
        )
    }
}

pub fn irq_create(gsi: u32) -> i64 {
    unsafe { syscall::syscall1(SYS_IRQ_CREATE, gsi as u64) }
}

pub fn ipc_create(fds: &mut [i32; 2], capacity: u32, flags: u32) -> i64 {
    unsafe {
        syscall::syscall3(
            SYS_IPC_CREATE,
            fds.as_mut_ptr() as u64,
            capacity as u64,
            flags as u64,
        )
    }
}

pub fn ipc_send(fd: i32, msg: &IpcMessage, flags: u32) -> i64 {
    unsafe {
        syscall::syscall3(
            SYS_IPC_SEND,
            fd as u64,
            msg as *const IpcMessage as u64,
            flags as u64,
        )
    }
}

pub fn ipc_recv(fd: i32, msg: &mut IpcMessage, flags: u32) -> i64 {
    unsafe {
        syscall::syscall3(
            SYS_IPC_RECV,
            fd as u64,
            msg as *mut IpcMessage as u64,
            flags as u64,
        )
    }
}

pub fn shmem_create(size: u64, flags: u32) -> i64 {
    unsafe { syscall::syscall2(SYS_SHMEM_CREATE, size, flags as u64) }
}

pub fn notify_create(flags: u32) -> i64 {
    unsafe { syscall::syscall1(SYS_NOTIFY_CREATE, flags as u64) }
}

pub fn notify_signal(fd: i32) -> i64 {
    unsafe { syscall::syscall1(SYS_NOTIFY, fd as u64) }
}

pub fn io_setup_rings(port_fd: i32, params: &mut IoRingParams) -> i64 {
    unsafe {
        syscall::syscall2(
            SYS_IO_SETUP_RINGS,
            port_fd as u64,
            params as *mut IoRingParams as u64,
        )
    }
}

pub fn io_ring_enter(port_fd: i32, to_submit: u32, min_complete: u32, flags: u32) -> i64 {
    unsafe {
        syscall::syscall4(
            SYS_IO_RING_ENTER,
            port_fd as u64,
            to_submit as u64,
            min_complete as u64,
            flags as u64,
        )
    }
}
