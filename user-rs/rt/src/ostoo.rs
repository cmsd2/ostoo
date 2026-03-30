//! Safe, high-level wrappers for ostoo custom syscalls.
//!
//! Provides RAII types that automatically close file descriptors on drop,
//! builder methods for [`IoSubmission`], and typed error handling.

use crate::sys;
use crate::syscall;
use core::sync::atomic::Ordering;

/// Error type for ostoo syscalls — wraps a negative errno return.
#[derive(Debug, Clone, Copy)]
pub struct OsError(pub i64);

impl OsError {
    /// The positive errno value (e.g. 22 for EINVAL).
    pub fn errno(&self) -> i64 {
        -self.0
    }
}

/// Convert a raw syscall return to `Result`.
fn check(ret: i64) -> Result<i64, OsError> {
    if ret < 0 {
        Err(OsError(ret))
    } else {
        Ok(ret)
    }
}

// ═══════════════════════════════════════════════════════════════════════
// CompletionPort
// ═══════════════════════════════════════════════════════════════════════

/// RAII wrapper for a completion port file descriptor.
pub struct CompletionPort {
    fd: i32,
}

impl CompletionPort {
    /// Create a new completion port.
    pub fn new() -> Result<Self, OsError> {
        let fd = check(sys::io_create(0))? as i32;
        Ok(CompletionPort { fd })
    }

    /// The raw file descriptor.
    pub fn fd(&self) -> i32 {
        self.fd
    }

    /// Submit one or more operations to the port.
    pub fn submit(&self, entries: &[sys::IoSubmission]) -> Result<i64, OsError> {
        check(sys::io_submit(self.fd, entries))
    }

    /// Wait for completions.  Blocks until at least `min` completions are
    /// available or `timeout_ns` nanoseconds elapse (0 = no timeout).
    pub fn wait(
        &self,
        completions: &mut [sys::IoCompletion],
        min: u32,
        timeout_ns: u64,
    ) -> Result<usize, OsError> {
        let n = check(sys::io_wait(self.fd, completions, min, timeout_ns))?;
        Ok(n as usize)
    }
}

impl Drop for CompletionPort {
    fn drop(&mut self) {
        syscall::close(self.fd as u32);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// IPC Channel
// ═══════════════════════════════════════════════════════════════════════

/// Send end of an IPC channel.
pub struct IpcSend {
    fd: i32,
}

/// Receive end of an IPC channel.
pub struct IpcRecv {
    fd: i32,
}

/// Create a channel pair `(send, recv)`.
pub fn ipc_channel(capacity: u32, flags: u32) -> Result<(IpcSend, IpcRecv), OsError> {
    let mut fds = [0i32; 2];
    check(sys::ipc_create(&mut fds, capacity, flags))?;
    Ok((IpcSend { fd: fds[0] }, IpcRecv { fd: fds[1] }))
}

impl IpcSend {
    pub fn fd(&self) -> i32 {
        self.fd
    }

    pub fn send(&self, msg: &sys::IpcMessage, flags: u32) -> Result<(), OsError> {
        check(sys::ipc_send(self.fd, msg, flags))?;
        Ok(())
    }
}

impl Drop for IpcSend {
    fn drop(&mut self) {
        syscall::close(self.fd as u32);
    }
}

impl IpcRecv {
    pub fn fd(&self) -> i32 {
        self.fd
    }

    pub fn recv(&self, flags: u32) -> Result<sys::IpcMessage, OsError> {
        let mut msg = sys::IpcMessage::default();
        check(sys::ipc_recv(self.fd, &mut msg, flags))?;
        Ok(msg)
    }
}

impl Drop for IpcRecv {
    fn drop(&mut self) {
        syscall::close(self.fd as u32);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// SharedMem
// ═══════════════════════════════════════════════════════════════════════

/// RAII wrapper for a shared memory file descriptor.
pub struct SharedMem {
    fd: i32,
    size: usize,
}

impl SharedMem {
    /// Create a new shared memory object of the given size.
    pub fn new(size: usize, flags: u32) -> Result<Self, OsError> {
        let fd = check(sys::shmem_create(size as u64, flags))? as i32;
        Ok(SharedMem { fd, size })
    }

    pub fn fd(&self) -> i32 {
        self.fd
    }

    pub fn size(&self) -> usize {
        self.size
    }

    /// Map this shared memory into the address space.  Returns a raw pointer.
    ///
    /// # Safety
    ///
    /// The caller must ensure the pointer is not used after `munmap` or
    /// after this `SharedMem` is dropped (which does NOT automatically unmap).
    pub fn mmap(&self) -> Result<*mut u8, OsError> {
        const SYS_MMAP: u64 = 9;
        const PROT_READ: u64 = 1;
        const PROT_WRITE: u64 = 2;
        const MAP_SHARED: u64 = 0x01;
        let ret = unsafe {
            syscall::syscall6(
                SYS_MMAP,
                0,
                self.size as u64,
                PROT_READ | PROT_WRITE,
                MAP_SHARED,
                self.fd as u64,
                0,
            )
        };
        if ret < 0 {
            return Err(OsError(ret));
        }
        Ok(ret as *mut u8)
    }
}

impl Drop for SharedMem {
    fn drop(&mut self) {
        syscall::close(self.fd as u32);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// NotifyFd
// ═══════════════════════════════════════════════════════════════════════

/// RAII wrapper for a notification file descriptor.
pub struct NotifyFd {
    fd: i32,
}

impl NotifyFd {
    pub fn new(flags: u32) -> Result<Self, OsError> {
        let fd = check(sys::notify_create(flags))? as i32;
        Ok(NotifyFd { fd })
    }

    pub fn fd(&self) -> i32 {
        self.fd
    }

    /// Signal this notification fd.
    pub fn signal(&self) -> Result<(), OsError> {
        check(sys::notify_signal(self.fd))?;
        Ok(())
    }
}

impl Drop for NotifyFd {
    fn drop(&mut self) {
        syscall::close(self.fd as u32);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// IrqFd
// ═══════════════════════════════════════════════════════════════════════

/// RAII wrapper for an IRQ file descriptor.
pub struct IrqFd {
    fd: i32,
}

impl IrqFd {
    pub fn new(gsi: u32) -> Result<Self, OsError> {
        let fd = check(sys::irq_create(gsi))? as i32;
        Ok(IrqFd { fd })
    }

    pub fn fd(&self) -> i32 {
        self.fd
    }
}

impl Drop for IrqFd {
    fn drop(&mut self) {
        syscall::close(self.fd as u32);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// IoRing — shared-memory submission/completion rings
// ═══════════════════════════════════════════════════════════════════════

const PAGE_SIZE: usize = 4096;

/// Userspace view of shared-memory SQ/CQ rings.
///
/// Handles the full lifecycle: create completion port, set up rings,
/// mmap both pages, and tear down on drop.
pub struct IoRing {
    port: CompletionPort,
    sq_fd: i32,
    cq_fd: i32,
    sq_base: *mut u8,
    cq_base: *mut u8,
    sq_entries: u32,
    cq_entries: u32,
}

impl IoRing {
    /// Create a new completion port with shared-memory rings.
    pub fn new(sq_entries: u32, cq_entries: u32) -> Result<Self, OsError> {
        let port = CompletionPort::new()?;
        let mut params = sys::IoRingParams {
            sq_entries,
            cq_entries,
            sq_fd: 0,
            cq_fd: 0,
        };
        check(sys::io_setup_rings(port.fd(), &mut params))?;

        const SYS_MMAP: u64 = 9;
        const PROT_RW: u64 = 1 | 2;
        const MAP_SHARED: u64 = 0x01;

        let sq_base = unsafe {
            syscall::syscall6(
                SYS_MMAP,
                0,
                PAGE_SIZE as u64,
                PROT_RW,
                MAP_SHARED,
                params.sq_fd as u64,
                0,
            )
        };
        if sq_base < 0 {
            return Err(OsError(sq_base));
        }

        let cq_base = unsafe {
            syscall::syscall6(
                SYS_MMAP,
                0,
                PAGE_SIZE as u64,
                PROT_RW,
                MAP_SHARED,
                params.cq_fd as u64,
                0,
            )
        };
        if cq_base < 0 {
            return Err(OsError(cq_base));
        }

        Ok(IoRing {
            port,
            sq_fd: params.sq_fd,
            cq_fd: params.cq_fd,
            sq_base: sq_base as *mut u8,
            cq_base: cq_base as *mut u8,
            sq_entries: params.sq_entries,
            cq_entries: params.cq_entries,
        })
    }

    /// The completion port's file descriptor.
    pub fn port_fd(&self) -> i32 {
        self.port.fd()
    }

    /// Number of SQ entries (power of 2).
    pub fn sq_entries(&self) -> u32 {
        self.sq_entries
    }

    /// Number of CQ entries (power of 2).
    pub fn cq_entries(&self) -> u32 {
        self.cq_entries
    }

    fn sq_header(&self) -> &sys::RingHeader {
        unsafe { &*(self.sq_base as *const sys::RingHeader) }
    }

    fn cq_header(&self) -> &sys::RingHeader {
        unsafe { &*(self.cq_base as *const sys::RingHeader) }
    }

    /// Push a submission entry to the SQ ring.  Returns `false` if full.
    pub fn push_sqe(&self, sqe: &sys::IoSubmission) -> bool {
        let hdr = self.sq_header();
        let tail = hdr.tail.load(Ordering::Relaxed);
        let head = hdr.head.load(Ordering::Acquire);
        if tail.wrapping_sub(head) >= self.sq_entries {
            return false;
        }
        let slot = (tail & hdr.mask) as usize;
        let offset =
            sys::RING_ENTRIES_OFFSET + slot * core::mem::size_of::<sys::IoSubmission>();
        unsafe {
            let dst = self.sq_base.add(offset) as *mut sys::IoSubmission;
            core::ptr::write(dst, *sqe);
        }
        hdr.tail.store(tail.wrapping_add(1), Ordering::Release);
        true
    }

    /// Pop a completion entry from the CQ ring.  Returns `None` if empty.
    pub fn pop_cqe(&self) -> Option<sys::IoCompletion> {
        let hdr = self.cq_header();
        let head = hdr.head.load(Ordering::Relaxed);
        let tail = hdr.tail.load(Ordering::Acquire);
        if head == tail {
            return None;
        }
        let slot = (head & hdr.mask) as usize;
        let offset =
            sys::RING_ENTRIES_OFFSET + slot * core::mem::size_of::<sys::IoCompletion>();
        let cqe = unsafe {
            let src = self.cq_base.add(offset) as *const sys::IoCompletion;
            core::ptr::read(src)
        };
        hdr.head.store(head.wrapping_add(1), Ordering::Release);
        Some(cqe)
    }

    /// Enter the kernel: process up to `to_submit` SQ entries and wait
    /// until at least `min_complete` CQ entries are available.
    pub fn enter(&self, to_submit: u32, min_complete: u32) -> Result<i64, OsError> {
        check(sys::io_ring_enter(
            self.port.fd(),
            to_submit,
            min_complete,
            0,
        ))
    }

    /// Submit operations via the legacy `io_submit` path (still works in
    /// ring mode — completions go to the CQ ring).
    pub fn submit(&self, entries: &[sys::IoSubmission]) -> Result<i64, OsError> {
        self.port.submit(entries)
    }
}

impl Drop for IoRing {
    fn drop(&mut self) {
        const SYS_MUNMAP: u64 = 11;
        unsafe {
            syscall::syscall2(SYS_MUNMAP, self.sq_base as u64, PAGE_SIZE as u64);
            syscall::syscall2(SYS_MUNMAP, self.cq_base as u64, PAGE_SIZE as u64);
        }
        syscall::close(self.sq_fd as u32);
        syscall::close(self.cq_fd as u32);
        // port dropped automatically via CompletionPort::drop
    }
}

// ═══════════════════════════════════════════════════════════════════════
// IoSubmission builder methods
// ═══════════════════════════════════════════════════════════════════════

impl sys::IoSubmission {
    pub fn nop(user_data: u64) -> Self {
        sys::IoSubmission {
            user_data,
            opcode: sys::OP_NOP,
            ..Default::default()
        }
    }

    pub fn timeout(user_data: u64, ns: u64) -> Self {
        sys::IoSubmission {
            user_data,
            opcode: sys::OP_TIMEOUT,
            timeout_ns: ns,
            ..Default::default()
        }
    }

    pub fn read(user_data: u64, fd: i32, buf: &mut [u8]) -> Self {
        sys::IoSubmission {
            user_data,
            opcode: sys::OP_READ,
            fd,
            buf_addr: buf.as_mut_ptr() as u64,
            buf_len: buf.len() as u32,
            ..Default::default()
        }
    }

    /// Build an OP_WRITE submission (named `write_op` to avoid shadowing
    /// `core::ptr::write`).
    pub fn write_op(user_data: u64, fd: i32, buf: &[u8]) -> Self {
        sys::IoSubmission {
            user_data,
            opcode: sys::OP_WRITE,
            fd,
            buf_addr: buf.as_ptr() as u64,
            buf_len: buf.len() as u32,
            ..Default::default()
        }
    }

    pub fn irq_wait(user_data: u64, irq_fd: i32) -> Self {
        sys::IoSubmission {
            user_data,
            opcode: sys::OP_IRQ_WAIT,
            fd: irq_fd,
            ..Default::default()
        }
    }

    pub fn ipc_send(user_data: u64, send_fd: i32, msg: &sys::IpcMessage) -> Self {
        sys::IoSubmission {
            user_data,
            opcode: sys::OP_IPC_SEND,
            fd: send_fd,
            buf_addr: msg as *const sys::IpcMessage as u64,
            ..Default::default()
        }
    }

    pub fn ipc_recv(user_data: u64, recv_fd: i32, msg: &mut sys::IpcMessage) -> Self {
        sys::IoSubmission {
            user_data,
            opcode: sys::OP_IPC_RECV,
            fd: recv_fd,
            buf_addr: msg as *mut sys::IpcMessage as u64,
            ..Default::default()
        }
    }

    pub fn ring_wait(user_data: u64, notify_fd: i32) -> Self {
        sys::IoSubmission {
            user_data,
            opcode: sys::OP_RING_WAIT,
            fd: notify_fd,
            ..Default::default()
        }
    }
}
