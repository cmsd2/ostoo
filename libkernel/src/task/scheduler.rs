use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

pub const QUANTUM_TICKS: u32 = 10; // 10 ms at 1000 Hz

static NEXT_THREAD_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ThreadId(u64);

impl ThreadId {
    fn new() -> Self {
        ThreadId(NEXT_THREAD_ID.fetch_add(1, Ordering::Relaxed))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThreadState {
    Ready,
    Running,
}

struct Thread {
    #[allow(dead_code)]
    id: ThreadId,
    state: ThreadState,
    saved_rsp: u64,
    ticks_remaining: u32,
    /// Owned stack backing. None for thread 0 (uses the boot stack).
    _stack: Option<Vec<u8>>,
}

struct Scheduler {
    initialized: bool,
    threads: Vec<Thread>,
    current_idx: usize,
    /// Queue of thread indices (into `threads`) that are ready to run.
    ready_queue: VecDeque<usize>,
}

impl Scheduler {
    const fn new() -> Self {
        Scheduler {
            initialized: false,
            threads: Vec::new(),
            current_idx: 0,
            ready_queue: VecDeque::new(),
        }
    }
}

static SCHEDULER: Mutex<Scheduler> = Mutex::new(Scheduler::new());

// ---------------------------------------------------------------------------
// Assembly context-switch stub
//
// Stack alignment analysis at the point of `call preempt_tick`:
//   CPU pushes SS/RSP/RFLAGS/CS/RIP  → 5 × 8 = 40 bytes
//   stub pushes 15 GPRs              → 15 × 8 = 120 bytes
//   total pushed = 160 bytes.
//   Before interrupt RSP = 16n  →  after 15 pushes RSP = 16n − 160 = 16(n−10)
//   `call` subtracts 8            →  RSP = 16(n−10) − 8 = 16m − 8
//   SysV ABI: at function entry RSP + 8 must be 16-byte aligned  ✓
// ---------------------------------------------------------------------------
// LLVM defaults to Intel (noprefix) syntax for inline assembly, so no
// .intel_syntax directive is needed.
core::arch::global_asm!(
    ".globl lapic_timer_stub",
    "lapic_timer_stub:",
    "push rax", "push rbx", "push rcx", "push rdx",
    "push rsi", "push rdi", "push rbp",
    "push r8",  "push r9",  "push r10", "push r11",
    "push r12", "push r13", "push r14", "push r15",
    "mov  rdi, rsp",        // current_rsp → first argument (SysV)
    "call preempt_tick",    // returns new rsp in rax
    "mov  rsp, rax",        // switch to (possibly new) thread's stack
    "pop r15", "pop r14", "pop r13", "pop r12",
    "pop r11", "pop r10", "pop r9",  "pop r8",
    "pop rbp", "pop rdi", "pop rsi",
    "pop rdx", "pop rcx", "pop rbx", "pop rax",
    "iretq",
);

/// Register the current execution context as thread 0.
/// Call once, after the heap is initialised and before starting the executor.
pub fn init() {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut sched = SCHEDULER.lock();
        // Pre-allocate so that preempt_tick's push_back never allocates from
        // ISR context (where the heap allocator may already be locked).
        sched.ready_queue.reserve(16);
        sched.threads.reserve(16);
        let id = ThreadId::new();
        sched.threads.push(Thread {
            id,
            state: ThreadState::Running,
            saved_rsp: 0, // filled in on first preemption
            ticks_remaining: QUANTUM_TICKS,
            _stack: None,
        });
        sched.current_idx = 0;
        sched.initialized = true;
    });
}

/// Spawn a new kernel thread.  `entry` must never return.
/// The new thread is placed on the ready queue and will run at the next
/// context switch.
pub fn spawn_thread(entry: fn() -> !) {
    const STACK_SIZE: usize = 64 * 1024; // 64 KiB

    let mut stack: Vec<u8> = Vec::with_capacity(STACK_SIZE);
    // Safety: we immediately zero-initialise via resize.
    stack.resize(STACK_SIZE, 0u8);

    // Round stack_top down to 16-byte boundary so saved_rsp is aligned.
    let stack_top_raw = stack.as_ptr() as u64 + stack.len() as u64;
    let stack_top = stack_top_raw & !0xF;

    // saved_rsp is the address of the r15 slot — bottom of the 160-byte frame.
    // The assembly stub restores all registers starting from this address then
    // executes iretq to start the new thread.
    //
    // Frame layout from saved_rsp (each slot is 8 bytes, stack grows upward
    // from saved_rsp):
    //   offset  0..112  :  r15, r14, r13, r12, r11, r10, r9, r8,
    //                      rbp, rdi, rsi, rdx, rcx, rbx, rax  (all 0)
    //   offset 120      :  RIP  = entry
    //   offset 128      :  CS   = 0x08 (kernel code segment)
    //   offset 136      :  RFLAGS = 0x202 (IF + reserved bit 1)
    //   offset 144      :  RSP  = stack_top − 160  (= saved_rsp, 16-byte aligned)
    //   offset 152      :  SS   = 0  (null selector, valid for ring-0 iretq)
    let saved_rsp = stack_top - (20 * 8); // 20 × u64 = 160 bytes

    unsafe {
        let frame = saved_rsp as *mut u64;
        for i in 0..15usize {
            frame.add(i).write(0); // 15 GPRs
        }
        frame.add(15).write(entry as usize as u64); // RIP
        frame.add(16).write(0x08);                  // CS
        frame.add(17).write(0x202);                 // RFLAGS
        frame.add(18).write(stack_top - 160);       // RSP after iretq
        frame.add(19).write(0);                     // SS
    }

    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut sched = SCHEDULER.lock();
        let id = ThreadId::new();
        let idx = sched.threads.len();
        sched.threads.push(Thread {
            id,
            state: ThreadState::Ready,
            saved_rsp,
            ticks_remaining: QUANTUM_TICKS,
            _stack: Some(stack),
        });
        sched.ready_queue.push_back(idx);
    });
}

/// Called by the LAPIC timer assembly stub on every tick.
///
/// Receives the current thread's RSP (pointing to the saved-register area on
/// its stack) and returns the RSP that should be active after the stub
/// finishes — either the same value (no switch) or the saved_rsp of the next
/// thread (context switch).
///
/// Safety: must only be called from `lapic_timer_stub` with interrupts already
/// disabled (the CPU clears IF on IDT dispatch).
#[no_mangle]
unsafe extern "C" fn preempt_tick(current_rsp: u64) -> u64 {
    // Advance the global tick counter and wake any sleeping tasks.
    crate::task::timer::tick();
    // Acknowledge the interrupt so the LAPIC can deliver the next one.
    crate::interrupts::lapic_eoi();

    let mut sched = SCHEDULER.lock();

    if !sched.initialized {
        return current_rsp;
    }

    let current_idx = sched.current_idx;

    // Decrement the running thread's quantum; keep running if ticks remain.
    sched.threads[current_idx].ticks_remaining -= 1;
    if sched.threads[current_idx].ticks_remaining > 0 {
        return current_rsp;
    }

    // Quantum expired — save context and pick the next thread.
    sched.threads[current_idx].saved_rsp = current_rsp;
    sched.threads[current_idx].state = ThreadState::Ready;
    sched.ready_queue.push_back(current_idx);

    // Round-robin: pop from the front of the ready queue.
    // If no other thread is ready we re-schedule the current one.
    let next_idx = sched.ready_queue.pop_front().unwrap_or(current_idx);

    sched.current_idx = next_idx;
    sched.threads[next_idx].state = ThreadState::Running;
    sched.threads[next_idx].ticks_remaining = QUANTUM_TICKS;

    sched.threads[next_idx].saved_rsp
}
