//! Console input buffer for raw keypress delivery to userspace.

use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicU64, Ordering};
use crate::spin_mutex::SpinMutex as Mutex;

use crate::process::ProcessId;
use crate::task::scheduler;
use crate::wait_condition::WaitCondition;

const INPUT_BUF_CAP: usize = 256;

struct ConsoleInner {
    buf: VecDeque<u8>,
    /// Thread index of a reader blocked waiting for input.
    blocked_reader: Option<usize>,
    /// PID of the process whose thread is blocked in `read_input`.
    blocked_reader_pid: Option<ProcessId>,
    /// Waker for async readers (completion port OP_READ).
    blocked_waker: Option<core::task::Waker>,
}

static CONSOLE_INPUT: Mutex<ConsoleInner> = Mutex::new(ConsoleInner {
    buf: VecDeque::new(),
    blocked_reader: None,
    blocked_reader_pid: None,
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

/// Result of a console read attempt.
pub enum ReadResult {
    /// Successfully read `n` bytes.
    Data(usize),
    /// Interrupted by a pending signal before any data was read.
    Interrupted,
}

/// Read bytes from the console input buffer into `buf`.
/// If the buffer is empty, blocks the current thread until input arrives.
/// Returns `Data(n)` on success or `Interrupted` if a signal is pending.
pub fn read_input(buf: &mut [u8]) -> ReadResult {
    let pid = crate::process::current_pid();
    loop {
        let mut inner = CONSOLE_INPUT.lock();
        if !inner.buf.is_empty() {
            let count = buf.len().min(inner.buf.len());
            for i in 0..count {
                buf[i] = inner.buf.pop_front().unwrap();
            }
            return ReadResult::Data(count);
        }
        // Before blocking, check for pending deliverable signals.
        if pid != ProcessId::KERNEL {
            let has_signal = crate::process::with_process_ref(pid, |p| {
                let deliverable = p.signal.pending & !p.signal.blocked;
                deliverable != 0
            }).unwrap_or(false);
            if has_signal {
                return ReadResult::Interrupted;
            }
        }
        // Buffer empty — register + mark blocked under the console lock.
        // [spec: completion_port.tla CheckAndAct — WaitCondition]
        WaitCondition::wait_while(Some(inner), |inner, thread_idx| {
            inner.blocked_reader = Some(thread_idx);
            inner.blocked_reader_pid = Some(pid);
        });
        // Loop back to retry after being woken.
    }
}

/// Wake any thread blocked in `read_input` so it can re-check for
/// pending signals and return `Interrupted`.
///
/// Does NOT queue a signal — the caller is responsible for that.
pub fn wake_blocked_reader() {
    let mut inner = CONSOLE_INPUT.lock();
    inner.blocked_reader_pid.take();
    if let Some(thread_idx) = inner.blocked_reader.take() {
        scheduler::unblock(thread_idx);
    }
    if let Some(waker) = inner.blocked_waker.take() {
        waker.wake();
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
