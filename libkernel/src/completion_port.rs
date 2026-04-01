//! Kernel completion port object for async I/O notification.
//!
//! Thin wrapper around the generic `spsc-ring` and `completion-port` crates,
//! adding kernel-specific wiring (physical memory allocation, scheduler wake).

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use x86_64::PhysAddr;
use crate::channel::TransferFds;
use crate::memory::PHYS_MAP_BASE;
use crate::task::scheduler;

// ---------------------------------------------------------------------------
// Re-exports — callers continue to import from libkernel::completion_port

pub use completion_port::{
    IoSubmission, IoCompletion, RingHeader,
    RING_ENTRIES_OFFSET, MAX_SQ_ENTRIES, MAX_CQ_ENTRIES,
};

// ---------------------------------------------------------------------------
// Opcode constants (kernel protocol, shared with osl and userspace)

pub const OP_NOP: u32 = 0;
pub const OP_TIMEOUT: u32 = 1;
pub const OP_READ: u32 = 2;
pub const OP_WRITE: u32 = 3;
pub const OP_IRQ_WAIT: u32 = 4;
pub const OP_IPC_SEND: u32 = 5;
pub const OP_IPC_RECV: u32 = 6;
pub const OP_RING_WAIT: u32 = 7;

// ---------------------------------------------------------------------------
// SchedulerWaker — bridges generic Waker trait to kernel scheduler

struct SchedulerWaker;

impl completion_port::Waker for SchedulerWaker {
    fn wake(&self, token: usize) {
        scheduler::unblock(token);
    }
}

// ---------------------------------------------------------------------------
// Completion — kernel-side completion entry (unchanged)

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
// IoRing — kernel wrapper owning physical pages, delegates to SpscRing

/// Shared-memory submission and completion rings.
///
/// The kernel owns the physical frames.  SharedMemInner objects created
/// via `from_existing` borrow them (no Drop release).
pub struct IoRing {
    sq_phys: Vec<PhysAddr>,
    cq_phys: Vec<PhysAddr>,
    sq_entries: u32,
    cq_entries: u32,
    sq_ring: completion_port::SpscRing<IoSubmission>,
    cq_ring: completion_port::SpscRing<IoCompletion>,
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

        // Build SpscRing views over the kernel-mapped pages
        let sq_virt = (PHYS_MAP_BASE + sq_phys_addr.as_u64()) as *mut u8;
        let cq_virt = (PHYS_MAP_BASE + cq_phys_addr.as_u64()) as *mut u8;

        let sq_ring = unsafe {
            let ring = completion_port::SpscRing::<IoSubmission>::from_raw_parts(sq_virt, sq_entries);
            ring.init_header();
            ring
        };

        let cq_ring = unsafe {
            let ring = completion_port::SpscRing::<IoCompletion>::from_raw_parts(cq_virt, cq_entries);
            ring.init_header();
            ring
        };

        let mut sq_frames = Vec::new();
        sq_frames.push(sq_phys_addr);
        let mut cq_frames = Vec::new();
        cq_frames.push(cq_phys_addr);

        Some(IoRing {
            sq_phys: sq_frames,
            cq_phys: cq_frames,
            sq_entries,
            cq_entries,
            sq_ring,
            cq_ring,
        })
    }

    /// Read an SQE from the SQ ring at the given logical index.
    pub fn read_sqe(&self, index: u32) -> IoSubmission {
        self.sq_ring.read_entry(index)
    }

    /// Write a CQE to the CQ ring, advancing the tail.
    ///
    /// Returns `false` if the CQ is full.
    // [spec: spsc_ring/spsc_ring.tla Producer — AcquireHead through ReleaseTail]
    pub fn post_cqe(&self, cqe: IoCompletion) -> bool {
        self.cq_ring.push(&cqe)
    }

    /// Number of pending SQ entries (tail - head).
    pub fn sq_pending(&self) -> u32 {
        // Consumer side: acquire the producer's tail
        let tail = self.sq_ring.tail_acquire();
        let head = self.sq_ring.head();
        tail.wrapping_sub(head)
    }

    /// Read the SQ head value.
    pub fn sq_head(&self) -> u32 {
        self.sq_ring.head()
    }

    /// Read the SQ tail value (written by userspace, acquire ordering).
    pub fn sq_tail(&self) -> u32 {
        self.sq_ring.tail_acquire()
    }

    /// Advance the SQ head by `n` entries (release ordering).
    pub fn advance_sq_head(&self, n: u32) {
        let head = self.sq_ring.head();
        // Release store: SQE reads are visible before head advances
        self.sq_ring.set_head(head.wrapping_add(n));
    }

    /// Number of CQ entries available (not yet consumed by userspace).
    pub fn cq_available(&self) -> u32 {
        // Producer side: acquire the consumer's head
        let head = self.cq_ring.head_acquire();
        let tail = self.cq_ring.tail();
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
// CompletionPort — kernel wrapper

pub struct CompletionPort {
    inner: completion_port::CompletionPort<Completion, SchedulerWaker>,
    io_ring: Option<IoRing>,
}

impl CompletionPort {
    pub fn new() -> Self {
        CompletionPort {
            inner: completion_port::CompletionPort::new(SchedulerWaker),
            io_ring: None,
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
    // [spec: completion_port/completion_port.tla Post — entire body is one atomic step under IrqMutex]
    pub fn post(&mut self, c: Completion) -> Option<usize> {
        // [spec: completion_port/completion_port.tla Post — p_simple branch (cq_count) vs queue branch]
        if let Some(ref ring) = self.io_ring {
            if c.read_buf.is_none() && c.transfer_fds.is_none() {
                // Fast path: write CQE directly to shared ring
                ring.post_cqe(IoCompletion {
                    user_data: c.user_data,
                    result: c.result,
                    flags: c.flags,
                    opcode: c.opcode,
                });
                // Wake waiter without queuing
                return self.inner.wake_waiter();
            }
        }
        // Slow path: queue in kernel memory
        self.inner.post(c)
    }

    /// Drain up to `max` completions from the queue.
    pub fn drain(&mut self, max: usize) -> VecDeque<Completion> {
        self.inner.drain(max)
    }

    /// Register the current thread as waiter. Only one waiter at a time.
    // [spec: completion_port/completion_port.tla CheckAndAct — "waiter := CONSUMER_ID"]
    pub fn set_waiter(&mut self, thread_idx: usize) {
        self.inner.set_waiter(thread_idx);
    }

    /// Number of pending completions in the kernel queue.
    pub fn pending(&self) -> usize {
        self.inner.pending()
    }

    /// Whether this port has shared-memory rings set up.
    pub fn has_ring(&self) -> bool {
        self.io_ring.is_some()
    }

    /// Set up shared-memory rings on this port.
    pub fn setup_ring(&mut self, ring: IoRing) {
        self.io_ring = Some(ring);
    }

    /// Access the ring (if set up).
    pub fn ring(&self) -> Option<&IoRing> {
        self.io_ring.as_ref()
    }
}
