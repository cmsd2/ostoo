//! Kernel completion port object for async I/O notification.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};
use x86_64::PhysAddr;
use crate::channel::TransferFds;
use crate::memory::PHYS_MAP_BASE;
use crate::task::scheduler;

// ---------------------------------------------------------------------------
// Opcode constants (shared with osl and userspace)

pub const OP_NOP: u32 = 0;
pub const OP_TIMEOUT: u32 = 1;
pub const OP_READ: u32 = 2;
pub const OP_WRITE: u32 = 3;
pub const OP_IRQ_WAIT: u32 = 4;
pub const OP_IPC_SEND: u32 = 5;
pub const OP_IPC_RECV: u32 = 6;
pub const OP_RING_WAIT: u32 = 7;

// ---------------------------------------------------------------------------
// Shared repr(C) structs (used by both kernel ring code and osl syscalls)

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

/// Completion entry — shared layout between userspace and kernel.
#[repr(C)]
#[derive(Clone, Copy)]
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

// ---------------------------------------------------------------------------
// IoRing — shared-memory SQ/CQ ring buffers

/// Offset (in bytes) where ring entries start, cache-line aligned.
const RING_ENTRIES_OFFSET: usize = 64;

/// Maximum SQ entries that fit in a single 4 KiB page:
/// (4096 - 64) / 48 = 84, rounded down to power of 2 = 64
pub const MAX_SQ_ENTRIES: u32 = 64;

/// Maximum CQ entries that fit in a single 4 KiB page:
/// (4096 - 64) / 24 = 168, rounded down to power of 2 = 128
pub const MAX_CQ_ENTRIES: u32 = 128;

/// Shared-memory submission and completion rings.
///
/// The kernel owns the physical frames.  SharedMemInner objects created
/// via `from_existing` borrow them (no Drop release).
pub struct IoRing {
    sq_phys: Vec<PhysAddr>,
    cq_phys: Vec<PhysAddr>,
    sq_entries: u32,
    cq_entries: u32,
}

impl IoRing {
    /// Allocate and initialise SQ/CQ ring pages.
    ///
    /// `sq_entries` and `cq_entries` must be powers of 2 and within limits.
    /// Returns `None` if frame allocation fails.
    pub fn new(sq_entries: u32, cq_entries: u32) -> Option<Self> {
        // Single page each for now
        let sq_phys_addr = crate::memory::with_memory(|mem| {
            let phys = mem.alloc_dma_pages(1)?;
            let dst = mem.phys_mem_offset() + phys.as_u64();
            unsafe { crate::consts::clear_page(dst.as_mut_ptr::<u8>()); }
            Some(phys)
        })?;

        let cq_phys_addr = crate::memory::with_memory(|mem| {
            let phys = mem.alloc_dma_pages(1)?;
            let dst = mem.phys_mem_offset() + phys.as_u64();
            unsafe { crate::consts::clear_page(dst.as_mut_ptr::<u8>()); }
            Some(phys)
        })?;

        // Initialise SQ header
        let sq_virt = PHYS_MAP_BASE + sq_phys_addr.as_u64();
        unsafe {
            let hdr = sq_virt as *mut RingHeader;
            (*hdr).head = AtomicU32::new(0);
            (*hdr).tail = AtomicU32::new(0);
            (*hdr).mask = sq_entries - 1;
            (*hdr).flags = 0;
        }

        // Initialise CQ header
        let cq_virt = PHYS_MAP_BASE + cq_phys_addr.as_u64();
        unsafe {
            let hdr = cq_virt as *mut RingHeader;
            (*hdr).head = AtomicU32::new(0);
            (*hdr).tail = AtomicU32::new(0);
            (*hdr).mask = cq_entries - 1;
            (*hdr).flags = 0;
        }

        let mut sq_frames = Vec::new();
        sq_frames.push(sq_phys_addr);
        let mut cq_frames = Vec::new();
        cq_frames.push(cq_phys_addr);

        Some(IoRing {
            sq_phys: sq_frames,
            cq_phys: cq_frames,
            sq_entries,
            cq_entries,
        })
    }

    /// Read an SQE from the SQ ring at the given logical index.
    pub fn read_sqe(&self, index: u32) -> IoSubmission {
        let phys = self.sq_phys[0].as_u64();
        let virt = PHYS_MAP_BASE + phys;
        let entry_offset = RING_ENTRIES_OFFSET
            + (index & (self.sq_entries - 1)) as usize * core::mem::size_of::<IoSubmission>();
        unsafe { *((virt + entry_offset as u64) as *const IoSubmission) }
    }

    /// Write a CQE to the CQ ring, advancing the tail.
    ///
    /// Returns `false` if the CQ is full.
    pub fn post_cqe(&self, cqe: IoCompletion) -> bool {
        let phys = self.cq_phys[0].as_u64();
        let virt = PHYS_MAP_BASE + phys;
        let hdr = unsafe { &*(virt as *const RingHeader) };

        // Acquire head (written by userspace consumer)
        let head = hdr.head.load(Ordering::Acquire);
        let tail = hdr.tail.load(Ordering::Relaxed);

        // Full when tail - head == capacity
        if tail.wrapping_sub(head) >= self.cq_entries {
            return false;
        }

        let entry_offset = RING_ENTRIES_OFFSET
            + (tail & (self.cq_entries - 1)) as usize * core::mem::size_of::<IoCompletion>();
        unsafe {
            *((virt + entry_offset as u64) as *mut IoCompletion) = cqe;
        }

        // Release store: CQE data is visible before tail advances
        hdr.tail.store(tail.wrapping_add(1), Ordering::Release);

        true
    }

    /// Number of pending SQ entries (tail - head).
    pub fn sq_pending(&self) -> u32 {
        let phys = self.sq_phys[0].as_u64();
        let virt = PHYS_MAP_BASE + phys;
        let hdr = unsafe { &*(virt as *const RingHeader) };
        let head = hdr.head.load(Ordering::Relaxed);
        // Acquire tail (written by userspace producer)
        let tail = hdr.tail.load(Ordering::Acquire);
        tail.wrapping_sub(head)
    }

    /// Read the SQ head value.
    pub fn sq_head(&self) -> u32 {
        let phys = self.sq_phys[0].as_u64();
        let virt = PHYS_MAP_BASE + phys;
        let hdr = unsafe { &*(virt as *const RingHeader) };
        hdr.head.load(Ordering::Relaxed)
    }

    /// Read the SQ tail value (written by userspace, acquire ordering).
    pub fn sq_tail(&self) -> u32 {
        let phys = self.sq_phys[0].as_u64();
        let virt = PHYS_MAP_BASE + phys;
        let hdr = unsafe { &*(virt as *const RingHeader) };
        hdr.tail.load(Ordering::Acquire)
    }

    /// Advance the SQ head by `n` entries (release ordering).
    pub fn advance_sq_head(&self, n: u32) {
        let phys = self.sq_phys[0].as_u64();
        let virt = PHYS_MAP_BASE + phys;
        let hdr = unsafe { &*(virt as *const RingHeader) };
        let head = hdr.head.load(Ordering::Relaxed);
        // Release store: SQE reads are visible before head advances
        hdr.head.store(head.wrapping_add(n), Ordering::Release);
    }

    /// Number of CQ entries available (not yet consumed by userspace).
    pub fn cq_available(&self) -> u32 {
        let phys = self.cq_phys[0].as_u64();
        let virt = PHYS_MAP_BASE + phys;
        let hdr = unsafe { &*(virt as *const RingHeader) };
        // Acquire head (written by userspace consumer)
        let head = hdr.head.load(Ordering::Acquire);
        let tail = hdr.tail.load(Ordering::Relaxed);
        tail.wrapping_sub(head)
    }

    /// Physical frame addresses backing the SQ ring.
    pub fn sq_frames(&self) -> &[PhysAddr] {
        self.sq_phys.as_slice()
    }

    /// Physical frame addresses backing the CQ ring.
    pub fn cq_frames(&self) -> &[PhysAddr] {
        self.cq_phys.as_slice()
    }

    /// Number of SQ entries.
    pub fn sq_entry_count(&self) -> u32 {
        self.sq_entries
    }

    /// Number of CQ entries.
    pub fn cq_entry_count(&self) -> u32 {
        self.cq_entries
    }
}

impl Drop for IoRing {
    fn drop(&mut self) {
        // Free the ring pages — these are owned by IoRing, not SharedMemInner.
        crate::memory::with_memory(|mem| {
            for &phys in &self.sq_phys {
                mem.release_shared_frame(phys);
            }
            for &phys in &self.cq_phys {
                mem.release_shared_frame(phys);
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Completion — kernel-side completion entry

/// A completion entry stored in the kernel queue.
pub struct Completion {
    pub user_data: u64,
    pub result: i64,
    pub flags: u32,
    pub opcode: u32,
    /// For async OP_READ: kernel buffer containing read data, copied to user
    /// space by io_wait (which runs in the process's syscall context).
    pub read_buf: Option<Vec<u8>>,
    /// For async OP_READ: user-space destination address for read_buf data.
    pub read_dest: u64,
    /// For OP_IPC_RECV: transferred fd objects to install in receiver's fd table.
    pub transfer_fds: Option<TransferFds>,
}

// ---------------------------------------------------------------------------
// CompletionPort — the kernel object

const DEFAULT_MAX_QUEUED: usize = 256;

pub struct CompletionPort {
    queue: VecDeque<Completion>,
    waiter: Option<usize>,
    max_queued: usize,
    ring: Option<IoRing>,
}

impl CompletionPort {
    pub fn new() -> Self {
        CompletionPort {
            queue: VecDeque::new(),
            waiter: None,
            max_queued: DEFAULT_MAX_QUEUED,
            ring: None,
        }
    }

    /// Post a completion to the queue (or CQ ring). Wakes the waiter if one
    /// is registered.
    ///
    /// In ring mode, simple completions (no read_buf, no transfer_fds) are
    /// written directly to the shared CQ ring.  Complex completions that need
    /// syscall-context data copies are deferred to the kernel queue and
    /// flushed by `io_ring_enter`.
    ///
    /// Returns the thread index that was unblocked (if any), so syscall-context
    /// callers can use it for scheduler donate.  ISR callers can ignore the
    /// return value.
    pub fn post(&mut self, c: Completion) -> Option<usize> {
        if let Some(ref ring) = self.ring {
            if c.read_buf.is_none() && c.transfer_fds.is_none() {
                // Fast path: write CQE directly to shared ring
                ring.post_cqe(IoCompletion {
                    user_data: c.user_data,
                    result: c.result,
                    flags: c.flags,
                    opcode: c.opcode,
                });
            } else {
                // Deferred: needs syscall context for data copy / fd install
                if self.queue.len() < self.max_queued {
                    self.queue.push_back(c);
                }
            }
        } else {
            if self.queue.len() < self.max_queued {
                self.queue.push_back(c);
            }
        }
        if let Some(t) = self.waiter.take() {
            scheduler::unblock(t);
            Some(t)
        } else {
            None
        }
    }

    /// Drain up to `max` completions from the queue.
    pub fn drain(&mut self, max: usize) -> VecDeque<Completion> {
        let count = max.min(self.queue.len());
        self.queue.drain(..count).collect()
    }

    /// Register the current thread as waiter. Only one waiter at a time.
    pub fn set_waiter(&mut self, thread_idx: usize) {
        self.waiter = Some(thread_idx);
    }

    /// Number of pending completions in the kernel queue.
    pub fn pending(&self) -> usize {
        self.queue.len()
    }

    /// Whether this port has shared-memory rings set up.
    pub fn has_ring(&self) -> bool {
        self.ring.is_some()
    }

    /// Set up shared-memory rings on this port.
    pub fn setup_ring(&mut self, ring: IoRing) {
        self.ring = Some(ring);
    }

    /// Access the ring (if set up).
    pub fn ring(&self) -> Option<&IoRing> {
        self.ring.as_ref()
    }
}
