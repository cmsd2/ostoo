//! Kernel completion port object for async I/O notification.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use crate::task::scheduler;

// ---------------------------------------------------------------------------
// Opcode constants (shared with osl and userspace)

pub const OP_NOP: u32 = 0;
pub const OP_TIMEOUT: u32 = 1;
pub const OP_READ: u32 = 2;
pub const OP_WRITE: u32 = 3;

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
}

// ---------------------------------------------------------------------------
// CompletionPort — the kernel object

const DEFAULT_MAX_QUEUED: usize = 256;

pub struct CompletionPort {
    queue: VecDeque<Completion>,
    waiter: Option<usize>,
    max_queued: usize,
}

impl CompletionPort {
    pub fn new() -> Self {
        CompletionPort {
            queue: VecDeque::new(),
            waiter: None,
            max_queued: DEFAULT_MAX_QUEUED,
        }
    }

    /// Post a completion to the queue. Wakes the waiter if one is registered.
    pub fn post(&mut self, c: Completion) {
        if self.queue.len() < self.max_queued {
            self.queue.push_back(c);
        }
        if let Some(t) = self.waiter.take() {
            scheduler::unblock(t);
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

    /// Number of pending completions.
    pub fn pending(&self) -> usize {
        self.queue.len()
    }
}
