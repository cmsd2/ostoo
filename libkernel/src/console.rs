//! Console input buffer for raw keypress delivery to userspace.

use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

use crate::process::ProcessId;
use crate::task::scheduler;

const INPUT_BUF_CAP: usize = 256;

struct ConsoleInner {
    buf: VecDeque<u8>,
    /// Thread index of a reader blocked waiting for input.
    blocked_reader: Option<usize>,
    /// Waker for async readers (completion port OP_READ).
    blocked_waker: Option<core::task::Waker>,
}

static CONSOLE_INPUT: Mutex<ConsoleInner> = Mutex::new(ConsoleInner {
    buf: VecDeque::new(),
    blocked_reader: None,
    blocked_waker: None,
});

/// PID of the foreground process that receives keyboard input.
/// 0 = kernel shell.
static FOREGROUND_PID: AtomicU64 = AtomicU64::new(0);

/// Push a byte into the console input buffer.
/// Called from the keyboard ISR when the foreground is a user process.
pub fn push_input(byte: u8) {
    let mut inner = CONSOLE_INPUT.lock();
    if inner.buf.len() < INPUT_BUF_CAP {
        inner.buf.push_back(byte);
    }
    // Wake blocked reader (scheduler thread or async waker).
    if let Some(thread_idx) = inner.blocked_reader.take() {
        scheduler::unblock(thread_idx);
    }
    if let Some(waker) = inner.blocked_waker.take() {
        waker.wake();
    }
}

/// Read bytes from the console input buffer into `buf`.
/// If the buffer is empty, blocks the current thread until input arrives.
/// Returns the number of bytes read (always >= 1, unless the process should exit).
pub fn read_input(buf: &mut [u8]) -> usize {
    loop {
        let mut inner = CONSOLE_INPUT.lock();
        if !inner.buf.is_empty() {
            let count = buf.len().min(inner.buf.len());
            for i in 0..count {
                buf[i] = inner.buf.pop_front().unwrap();
            }
            return count;
        }
        // Buffer empty — register ourselves as blocked reader and sleep.
        let thread_idx = scheduler::current_thread_idx();
        inner.blocked_reader = Some(thread_idx);
        drop(inner); // release lock before blocking
        scheduler::block_current_thread();
        // Loop back to retry after being woken.
    }
}

/// Async-capable version of `read_input`. Returns `Pending` instead of
/// blocking when the buffer is empty, registering the waker for later wake.
pub fn poll_read_input(cx: &mut core::task::Context<'_>, buf: &mut [u8])
    -> core::task::Poll<usize>
{
    let mut inner = CONSOLE_INPUT.lock();
    if !inner.buf.is_empty() {
        let count = buf.len().min(inner.buf.len());
        for i in 0..count {
            buf[i] = inner.buf.pop_front().unwrap();
        }
        return core::task::Poll::Ready(count);
    }
    inner.blocked_waker = Some(cx.waker().clone());
    core::task::Poll::Pending
}

/// Set the foreground process.
pub fn set_foreground(pid: ProcessId) {
    FOREGROUND_PID.store(pid.as_u64(), Ordering::Relaxed);
    // Flush the input buffer on foreground change.
    flush_input();
}

/// Get the foreground process PID.
pub fn foreground_pid() -> ProcessId {
    ProcessId::from_raw(FOREGROUND_PID.load(Ordering::Relaxed))
}

/// Clear the input buffer.
pub fn flush_input() {
    CONSOLE_INPUT.lock().buf.clear();
}
