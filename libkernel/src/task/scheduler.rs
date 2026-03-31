use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use crate::spin_mutex::SpinMutex as Mutex;

use crate::process::ProcessId;
use crate::serial_println;

pub const QUANTUM_TICKS: u32 = 10; // 10 ms at 1000 Hz

static NEXT_THREAD_ID: AtomicU64 = AtomicU64::new(0);

/// Total number of context switches since boot.
pub static CONTEXT_SWITCHES: AtomicU64 = AtomicU64::new(0);
/// Index of the currently running thread (into the `threads` Vec).
static CURRENT_THREAD_IDX_ATOMIC: AtomicUsize = AtomicUsize::new(0);

pub fn context_switches() -> u64 {
    CONTEXT_SWITCHES.load(Ordering::Relaxed)
}

pub fn current_thread_idx() -> usize {
    CURRENT_THREAD_IDX_ATOMIC.load(Ordering::Relaxed)
}

/// Update the PML4 physical address recorded for the currently running thread.
///
/// Call this before switching CR3 outside the scheduler (e.g. before `iretq`
/// to a user process) so that `preempt_tick` will restore the correct
/// address space on context switch.
pub fn set_current_cr3(pml4_phys: u64) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut sched = SCHEDULER.lock();
        if sched.initialized {
            let idx = sched.current_idx;
            sched.threads[idx].pml4_phys = pml4_phys;
        }
    });
}

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
    Blocked,
    Dead,
}

impl ThreadState {
    /// Returns `true` if the thread can be scheduled (not Dead or Blocked).
    fn is_runnable(self) -> bool {
        self != ThreadState::Dead && self != ThreadState::Blocked
    }
}

/// What kind of schedulable entity this thread represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulableKind {
    Kernel,
    UserProcess(ProcessId),
}

struct Thread {
    #[allow(dead_code)]
    id: ThreadId,
    state: ThreadState,
    saved_rsp: u64,
    /// Physical address of this thread's PML4 (CR3 value).
    /// All kernel threads share the boot PML4; user processes each have their own.
    pml4_phys: u64,
    ticks_remaining: u32,
    /// Owned stack backing. None for thread 0 (permanent, forgotten) and for
    /// user process threads (kernel stack owned by Process).
    _stack: Option<crate::stack_arena::StackSlot>,
    /// What this thread represents.
    kind: SchedulableKind,
    /// For user process threads: the top of the process's kernel stack.
    /// Used to update TSS.rsp0 and PER_CPU on context switch.
    kernel_stack_top: u64,
    /// Saved user RSP from PER_CPU.  The SYSCALL entry stub writes the user
    /// RSP to a single per-CPU slot; we must save/restore it per-thread so
    /// that a blocked syscall resumes with the correct user stack.
    user_rsp: u64,
    /// Saved FS_BASE (IA32_FS_BASE MSR).  musl uses FS-relative addressing
    /// for TLS (errno, etc.), so each user process needs its own FS_BASE.
    fs_base: u64,
}

struct Scheduler {
    initialized: bool,
    threads: Vec<Thread>,
    current_idx: usize,
    /// Queue of thread indices (into `threads`) that are ready to run.
    ready_queue: VecDeque<usize>,
    /// Index of the idle thread.  Never on the ready queue — used as fallback
    /// when the ready queue is empty.
    idle_thread_idx: usize,
}

impl Scheduler {
    const fn new() -> Self {
        Scheduler {
            initialized: false,
            threads: Vec::new(),
            current_idx: 0,
            ready_queue: VecDeque::new(),
            idle_thread_idx: 0,
        }
    }

    /// Find a Dead slot to reuse, or push a new entry. Returns the index.
    fn alloc_thread_slot(&mut self, thread: Thread) -> usize {
        // Skip slot 0 (boot thread) — always kept.
        if let Some(idx) = self.threads.iter().position(|t| t.state == ThreadState::Dead) {
            self.threads[idx] = thread;
            idx
        } else {
            let idx = self.threads.len();
            self.threads.push(thread);
            idx
        }
    }
}

static SCHEDULER: Mutex<Scheduler> = Mutex::new(Scheduler::new());

// ---------------------------------------------------------------------------
// Switch frame — matches the layout pushed by `lapic_timer_stub`

/// Saved register frame written to the stack by the timer stub and read back
/// on context switch.  Must match the push/pop order in `lapic_timer_stub`
/// exactly: 15 GPRs followed by the hardware iretq frame.
///
/// `Thread.saved_rsp` points to the base of the 512-byte FXSAVE area, which
/// sits immediately below this frame in memory.  The SwitchFrame itself is at
/// `saved_rsp + FXSAVE_SIZE`.
#[repr(C)]
#[derive(Clone, Copy)]
struct SwitchFrame {
    // GPRs (push order in the stub: rax, rbx, rcx, rdx, rsi, rdi, rbp,
    //       r8, r9, r10, r11, r12, r13, r14, r15)
    // Pops happen in reverse, so r15 is at the lowest address.
    r15: u64, r14: u64, r13: u64, r12: u64,
    r11: u64, r10: u64, r9: u64,  r8: u64,
    rbp: u64, rdi: u64, rsi: u64,
    rdx: u64, rcx: u64, rbx: u64, rax: u64,
    // iretq frame
    rip: u64, cs: u64, rflags: u64, rsp: u64, ss: u64,
}

const KERNEL_CS: u64 = 0x08;
const KERNEL_SS: u64 = 0;
const RFLAGS_IF: u64 = 0x202; // IF + reserved bit 1
const FXSAVE_SIZE: usize = 512;

impl SwitchFrame {
    /// Build a frame for a new kernel thread.  All GPRs are zeroed.
    fn new_kernel(entry: fn() -> !, stack_top: u64) -> Self {
        SwitchFrame {
            r15: 0, r14: 0, r13: 0, r12: 0,
            r11: 0, r10: 0, r9: 0,  r8: 0,
            rbp: 0, rdi: 0, rsi: 0,
            rdx: 0, rcx: 0, rbx: 0, rax: 0,
            rip: entry as usize as u64,
            cs: KERNEL_CS,
            rflags: RFLAGS_IF,
            rsp: stack_top - 8, // 8 mod 16, simulates `call` push
            ss: KERNEL_SS,
        }
    }

    /// Build a frame that enters `process_trampoline` in kernel mode.
    /// `pid` is passed via the `rdi` slot (first SysV64 argument).
    fn new_user_trampoline(pid: ProcessId, stack_top: u64) -> Self {
        SwitchFrame {
            r15: 0, r14: 0, r13: 0, r12: 0,
            r11: 0, r10: 0, r9: 0,  r8: 0,
            rbp: 0, rdi: pid.as_u64(), rsi: 0,
            rdx: 0, rcx: 0, rbx: 0, rax: 0,
            rip: process_trampoline as *const () as usize as u64,
            cs: KERNEL_CS,
            rflags: RFLAGS_IF,
            rsp: stack_top - 8,
            ss: KERNEL_SS,
        }
    }
}

// ---------------------------------------------------------------------------
// Assembly context-switch stub
//
// The stub conditionally executes `swapgs` on entry and exit so that kernel
// code always runs with GS.BASE = per-CPU, regardless of whether the timer
// fired from ring 0 or ring 3.  This mirrors the Linux interrupt entry
// convention.
//
// On entry, the hardware iretq frame is at RSP.  [RSP+8] = CS: if RPL bits
// (0–1) are non-zero the interrupt came from ring 3 and we need swapgs.
// On exit, [RSP+8] is the CS of the (possibly different) thread we're
// returning to — a context switch may have changed the stack.
//
// Stack alignment analysis at the point of `call preempt_tick`:
//   CPU pushes SS/RSP/RFLAGS/CS/RIP  → 5 × 8 = 40 bytes
//   stub pushes 15 GPRs              → 15 × 8 = 120 bytes
//   stub subtracts 512 for FXSAVE    → 512 bytes
//   total pushed = 672 bytes = 42 × 16.
//   Before interrupt RSP = 16n  →  after pushes RSP = 16n − 672 = 16(n−42)
//   `call` subtracts 8            →  RSP = 16(n−42) − 8 = 16m − 8
//   SysV ABI: at function entry RSP + 8 must be 16-byte aligned  ✓
//   fxsave requires 16-byte aligned operand — RSP is 16-aligned after sub  ✓
// ---------------------------------------------------------------------------
// LLVM defaults to Intel (noprefix) syntax for inline assembly, so no
// .intel_syntax directive is needed.
core::arch::global_asm!(
    ".globl lapic_timer_stub",
    "lapic_timer_stub:",
    // --- entry swapgs: if interrupted from ring 3, swap to kernel GS ---
    "test qword ptr [rsp+8], 3",   // check RPL bits of saved CS
    "jz .Ltimer_from_ring0",
    "swapgs",
    ".Ltimer_from_ring0:",
    "push rax", "push rbx", "push rcx", "push rdx",
    "push rsi", "push rdi", "push rbp",
    "push r8",  "push r9",  "push r10", "push r11",
    "push r12", "push r13", "push r14", "push r15",
    "sub  rsp, 512",        // allocate FXSAVE area
    "fxsave [rsp]",         // save x87/MMX/SSE state
    "mov  rdi, rsp",        // current_rsp → first argument (SysV)
    "call preempt_tick",    // returns new rsp in rax
    "mov  rsp, rax",        // switch to (possibly new) thread's stack
    "fxrstor [rsp]",        // restore x87/MMX/SSE state
    "add  rsp, 512",        // deallocate FXSAVE area
    "pop r15", "pop r14", "pop r13", "pop r12",
    "pop r11", "pop r10", "pop r9",  "pop r8",
    "pop rbp", "pop rdi", "pop rsi",
    "pop rdx", "pop rcx", "pop rbx", "pop rax",
    // --- exit swapgs: if returning to ring 3, swap back to user GS ---
    "test qword ptr [rsp+8], 3",   // check RPL bits of target CS
    "jz .Ltimer_to_ring0",
    "swapgs",
    ".Ltimer_to_ring0:",
    "iretq",
);

// ---------------------------------------------------------------------------
// Voluntary yield stub — identical to the timer stub but calls `yield_tick`
// instead of `preempt_tick`.  Triggered via `int 0x50` from syscall context.
core::arch::global_asm!(
    ".globl ipc_yield_stub",
    "ipc_yield_stub:",
    "test qword ptr [rsp+8], 3",
    "jz .Lyield_from_ring0",
    "swapgs",
    ".Lyield_from_ring0:",
    "push rax", "push rbx", "push rcx", "push rdx",
    "push rsi", "push rdi", "push rbp",
    "push r8",  "push r9",  "push r10", "push r11",
    "push r12", "push r13", "push r14", "push r15",
    "sub  rsp, 512",
    "fxsave [rsp]",
    "mov  rdi, rsp",
    "call yield_tick",
    "mov  rsp, rax",
    "fxrstor [rsp]",
    "add  rsp, 512",
    "pop r15", "pop r14", "pop r13", "pop r12",
    "pop r11", "pop r10", "pop r9",  "pop r8",
    "pop rbp", "pop rdi", "pop rsi",
    "pop rdx", "pop rcx", "pop rbx", "pop rax",
    "test qword ptr [rsp+8], 3",
    "jz .Lyield_to_ring0",
    "swapgs",
    ".Lyield_to_ring0:",
    "iretq",
);

/// Allocate an arena stack and switch RSP to it, then call `continuation`.
///
/// This moves the boot thread off the bootloader's lower-half stack onto an
/// arena-backed stack (PML4 entry 256, high canonical half).  The old stack is
/// abandoned — `continuation` must never return.
///
/// Call once, after the stack arena is initialised.
pub fn migrate_to_heap_stack(continuation: fn() -> !) -> ! {
    let stack = crate::stack_arena::alloc().expect("stack arena exhausted");
    let stack_top = stack.top();
    core::mem::forget(stack); // permanent — never freed
    unsafe {
        core::arch::asm!(
            "mov rsp, {top}",
            "call {entry}",
            top = in(reg) stack_top,
            entry = in(reg) continuation,
            options(noreturn),
        );
    }
}

/// The idle thread body.  Runs when no other thread is ready.
fn idle_thread_main() -> ! {
    loop {
        x86_64::instructions::interrupts::enable_and_hlt();
    }
}

/// Register the current execution context as thread 0.
/// Call once, after the heap is initialised and before starting the executor.
pub fn init() {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let (cr3_frame, _) = x86_64::registers::control::Cr3::read();
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
            pml4_phys: cr3_frame.start_address().as_u64(),
            ticks_remaining: QUANTUM_TICKS,
            _stack: None,
            kind: SchedulableKind::Kernel,
            kernel_stack_top: 0,
            user_rsp: 0,
            fs_base: 0,
        });
        sched.current_idx = 0;
        sched.initialized = true;
    });

    // Spawn the idle thread outside the scheduler lock (spawn_thread locks internally).
    spawn_thread(idle_thread_main);

    // Remove the idle thread from the ready queue and record its index.
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut sched = SCHEDULER.lock();
        let idle_idx = sched.ready_queue.pop_back()
            .expect("idle thread should be on the ready queue");
        sched.idle_thread_idx = idle_idx;
        // Set idle thread to Ready (not on the queue — scheduler picks it as fallback).
        sched.threads[idle_idx].state = ThreadState::Ready;
    });
}

/// Spawn a new kernel thread.  `entry` must never return.
/// The new thread is placed on the ready queue and will run at the next
/// context switch.
pub fn spawn_thread(entry: fn() -> !) {
    let stack = crate::stack_arena::alloc().expect("stack arena exhausted");
    let stack_top = stack.top();

    let frame = SwitchFrame::new_kernel(entry, stack_top);
    // saved_rsp points to the base of the FXSAVE area; the SwitchFrame sits above it.
    let saved_rsp = stack_top - core::mem::size_of::<SwitchFrame>() as u64 - FXSAVE_SIZE as u64;
    unsafe {
        let frame_ptr = (saved_rsp + FXSAVE_SIZE as u64) as *mut SwitchFrame;
        core::ptr::write(frame_ptr, frame);
        // MXCSR default at FXSAVE offset 24: all SSE exceptions masked, round-to-nearest.
        core::ptr::write((saved_rsp + 24) as *mut u32, 0x1F80);
    }

    let (cr3_frame, _) = x86_64::registers::control::Cr3::read();
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut sched = SCHEDULER.lock();
        let id = ThreadId::new();
        let idx = sched.alloc_thread_slot(Thread {
            id,
            state: ThreadState::Ready,
            saved_rsp,
            pml4_phys: cr3_frame.start_address().as_u64(),
            ticks_remaining: QUANTUM_TICKS,
            _stack: Some(stack),
            kind: SchedulableKind::Kernel,
            kernel_stack_top: 0,
            user_rsp: 0,
            fs_base: 0,
        });
        sched.ready_queue.push_back(idx);
    });
}

// ---------------------------------------------------------------------------
// User-process thread support

/// Spawn a scheduler thread for a user process.
///
/// The new thread starts in kernel mode at `process_trampoline`, which reads
/// the PID from r15, looks up the process, and drops to ring 3.
///
/// Returns the thread index in the scheduler's thread vec.
pub fn spawn_user_thread(pid: ProcessId, pml4_phys: x86_64::PhysAddr) -> usize {
    // Read process info under the process table lock (not from ISR context).
    let (kernel_stack_top, entry_point, user_stack_top) =
        crate::process::with_process_ref(pid, |p| {
            (p.kernel_stack_top, p.entry_point, p.user_stack_top)
        }).expect("spawn_user_thread: process not found");

    let _ = entry_point;
    let _ = user_stack_top;

    // Build a SwitchFrame on the process's kernel stack.
    // The frame targets `process_trampoline` in kernel mode, with the PID
    // passed via the RDI slot so the trampoline receives it as its first arg.
    let stack_top = kernel_stack_top;
    let frame = SwitchFrame::new_user_trampoline(pid, stack_top);
    // saved_rsp points to the base of the FXSAVE area; the SwitchFrame sits above it.
    let saved_rsp = stack_top - core::mem::size_of::<SwitchFrame>() as u64 - FXSAVE_SIZE as u64;
    unsafe {
        let frame_ptr = (saved_rsp + FXSAVE_SIZE as u64) as *mut SwitchFrame;
        core::ptr::write(frame_ptr, frame);
        // MXCSR default at FXSAVE offset 24: all SSE exceptions masked, round-to-nearest.
        core::ptr::write((saved_rsp + 24) as *mut u32, 0x1F80);
    }

    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut sched = SCHEDULER.lock();
        let id = ThreadId::new();
        let idx = sched.alloc_thread_slot(Thread {
            id,
            state: ThreadState::Ready,
            saved_rsp,
            pml4_phys: pml4_phys.as_u64(),
            ticks_remaining: QUANTUM_TICKS,
            _stack: None, // kernel stack owned by Process, not Thread
            kind: SchedulableKind::UserProcess(pid),
            kernel_stack_top: stack_top,
            user_rsp: 0,
            fs_base: 0,
        });
        sched.ready_queue.push_back(idx);
        idx
    })
}

/// Spawn a scheduler thread for a clone(CLONE_VM|CLONE_VFORK) child.
///
/// The child "returns from syscall" at `user_rip` with RAX=0, on `child_stack`.
/// Returns the thread index.
pub fn spawn_clone_thread(
    pid: ProcessId,
    pml4_phys: x86_64::PhysAddr,
    user_rip: u64,
    child_stack: u64,
    user_rflags: u64,
    user_r9: u64,
    fs_base: u64,
) -> usize {
    let kernel_stack_top = crate::process::with_process_ref(pid, |p| {
        p.kernel_stack_top
    }).expect("spawn_clone_thread: process not found");

    // Build a SwitchFrame that enters `clone_trampoline` in kernel mode.
    // Pass the PID via RDI (first SysV64 argument).
    let stack_top = kernel_stack_top;
    let frame = SwitchFrame {
        r15: 0, r14: 0, r13: 0, r12: 0,
        r11: user_rflags, r10: 0, r9: 0, r8: 0,
        rbp: 0, rdi: pid.as_u64(), rsi: user_rip,
        rdx: child_stack, rcx: user_r9, rbx: 0, rax: 0,
        rip: clone_trampoline as *const () as usize as u64,
        cs: KERNEL_CS,
        rflags: RFLAGS_IF,
        rsp: stack_top - 8,
        ss: KERNEL_SS,
    };
    let saved_rsp = stack_top - core::mem::size_of::<SwitchFrame>() as u64 - FXSAVE_SIZE as u64;
    unsafe {
        let frame_ptr = (saved_rsp + FXSAVE_SIZE as u64) as *mut SwitchFrame;
        core::ptr::write(frame_ptr, frame);
        core::ptr::write((saved_rsp + 24) as *mut u32, 0x1F80);
    }

    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut sched = SCHEDULER.lock();
        let id = ThreadId::new();
        let idx = sched.alloc_thread_slot(Thread {
            id,
            state: ThreadState::Ready,
            saved_rsp,
            pml4_phys: pml4_phys.as_u64(),
            ticks_remaining: QUANTUM_TICKS,
            _stack: None,
            kind: SchedulableKind::UserProcess(pid),
            kernel_stack_top: stack_top,
            user_rsp: child_stack,
            fs_base,
        });
        sched.ready_queue.push_back(idx);
        idx
    })
}

/// Trampoline for clone() child threads.
///
/// Receives PID in RDI, user_rip in RSI, child_stack in RDX, user_r9 in RCX.
/// Drops to ring 3 at user_rip with RAX=0, R9=user_r9, RSP=child_stack.
extern "C" fn clone_trampoline(pid_raw: u64, user_rip: u64, child_stack: u64, user_r9: u64) -> ! {
    let pid = ProcessId::from_raw(pid_raw);

    let (pml4_phys, kernel_stack_top) =
        crate::process::with_process_ref(pid, |p| {
            (p.pml4_phys.as_u64(), p.kernel_stack_top)
        }).expect("clone_trampoline: process not found");

    serial_println!("[clone_trampoline] pid={} rip={:#x} stack={:#x} pml4={:#x} r9={:#x}",
        pid.as_u64(), user_rip, child_stack, pml4_phys, user_r9);

    x86_64::instructions::interrupts::disable();

    crate::gdt::set_kernel_stack(x86_64::VirtAddr::new(kernel_stack_top));
    crate::syscall::set_kernel_rsp(kernel_stack_top);
    crate::process::set_current_pid(pid);
    set_current_cr3(pml4_phys);

    let user_cs = crate::gdt::user_code_selector().0 as u64;
    let user_ss = crate::gdt::user_data_selector().0 as u64;
    let per_cpu = crate::syscall::per_cpu_addr();

    // RAX=0 tells musl's __clone that this is the child.
    // R9=user_r9 restores the child function pointer that musl stored in R9.
    unsafe {
        jump_to_userspace(user_rip, child_stack, pml4_phys, user_cs, user_ss, per_cpu, 0, user_r9);
    }
}

/// Trampoline for newly-spawned user process threads.
///
/// The scheduler's iretq lands here in kernel mode with the PID passed in RDI
/// (first argument, SysV64 ABI).  We look up the process, set up TSS/PER_CPU,
/// switch CR3, and drop to ring 3.
extern "C" fn process_trampoline(pid_raw: u64) -> ! {
    let pid = ProcessId::from_raw(pid_raw);

    let (entry_point, user_stack_top, pml4_phys, kernel_stack_top) =
        crate::process::with_process_ref(pid, |p| {
            (p.entry_point, p.user_stack_top, p.pml4_phys.as_u64(), p.kernel_stack_top)
        }).expect("process_trampoline: process not found");

    let rsp: u64;
    unsafe { core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nostack, nomem)); }
    serial_println!("[trampoline] pid={} entry={:#x} usp={:#x} pml4={:#x} kstack={:#x} RSP={:#x} (mod16={})",
        pid.as_u64(), entry_point, user_stack_top, pml4_phys, kernel_stack_top,
        rsp, rsp & 0xF);

    // Disable interrupts early to close the preemption window between
    // set_current_cr3 (which re-enables IF) and the GS MSR writes.
    // Without this, a timer interrupt in that gap could context-switch away
    // with partially-configured GS polarity, corrupting kernel state.
    x86_64::instructions::interrupts::disable();

    // Set up TSS.rsp0 and PER_CPU kernel_rsp for this process.
    crate::gdt::set_kernel_stack(x86_64::VirtAddr::new(kernel_stack_top));
    crate::syscall::set_kernel_rsp(kernel_stack_top);
    crate::process::set_current_pid(pid);

    // Tell the scheduler about our CR3 for context-switch address space restore.
    // Note: with interrupts already disabled, the without_interrupts() inside
    // set_current_cr3 is a no-op (IF stays cleared).
    set_current_cr3(pml4_phys);

    let user_cs = crate::gdt::user_code_selector().0 as u64;
    let user_ss = crate::gdt::user_data_selector().0 as u64;
    let per_cpu = crate::syscall::per_cpu_addr();

    // Safety: interrupts are disabled, we have set up TSS/PER_CPU/CR3,
    // and the entry/stack/selector values come from a validated Process.
    unsafe {
        jump_to_userspace(entry_point, user_stack_top, pml4_phys, user_cs, user_ss, per_cpu, 0, 0);
    }
}

/// Switch to ring 3: set GS MSRs, load CR3, restore GPRs, and execute iretq.
///
/// `user_rax` and `user_r9` are loaded into RAX/R9 before the iretq.
/// For a fresh exec, pass 0 for both.  For a clone child, pass RAX=0
/// and R9 = the saved user R9 (musl's `__clone` needs R9 = fn pointer).
///
/// # Safety
/// - Interrupts must be disabled.
/// - `pml4_phys` must be a valid PML4 physical address.
/// - `entry` and `user_rsp` must be valid user-space addresses.
/// - `user_cs` and `user_ss` must be valid ring-3 segment selectors.
/// - `per_cpu` must point to the kernel's `PerCpuData` block.
pub unsafe fn jump_to_userspace(
    entry: u64, user_rsp: u64, pml4_phys: u64,
    user_cs: u64, user_ss: u64, per_cpu: u64,
    user_rax: u64, user_r9: u64,
) -> ! {
    // Explicitly set GS MSRs instead of using `swapgs`, because the GS
    // polarity is unpredictable: a timer preemption of a previous user process
    // leaves user GS active, while a kernel thread leaves kernel GS active.
    // Writing both MSRs directly avoids this ambiguity.
    //   IA32_GS_BASE = 0         → user GS for ring 3
    //   IA32_KERNEL_GS_BASE = per_cpu → restored by syscall swapgs
    core::arch::asm!("cli", options(nostack, nomem));
    x86_64::registers::model_specific::Msr::new(crate::msr::IA32_GS_BASE).write(0);
    x86_64::registers::model_specific::Msr::new(crate::msr::IA32_KERNEL_GS_BASE).write(per_cpu);

    core::arch::asm!(
        "mov cr3, {pml4}",
        "push {ss}",
        "push {usp}",
        "push {rf}",
        "push {cs}",
        "push {ip}",
        "mov rax, {rax_val}",
        "mov r9,  {r9_val}",
        "iretq",
        pml4    = in(reg) pml4_phys,
        ss      = in(reg) user_ss,
        usp     = in(reg) user_rsp,
        rf      = in(reg) RFLAGS_IF,
        cs      = in(reg) user_cs,
        ip      = in(reg) entry,
        rax_val = in(reg) user_rax,
        r9_val  = in(reg) user_r9,
        options(noreturn),
    );
}

/// Returns `true` if the thread at `idx` is in the `Dead` state.
pub fn is_thread_dead(idx: usize) -> bool {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let sched = SCHEDULER.lock();
        sched.threads.get(idx).map_or(true, |t| t.state == ThreadState::Dead)
    })
}

/// Mark the current thread as dead and yield.
///
/// The scheduler will see `Dead` and switch to the next ready thread (or the
/// idle thread).  This function never returns.
pub fn kill_current_thread() -> ! {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut sched = SCHEDULER.lock();
        let idx = sched.current_idx;
        sched.threads[idx].state = ThreadState::Dead;
    });
    yield_now();
    unreachable!("dead thread was rescheduled");
}

/// Mark the current thread as blocked WITHOUT yielding.
///
/// Call this while still holding the caller's lock (e.g. port, pipe, channel)
/// so that `unblock()` is guaranteed to find `ThreadState::Blocked`.
/// Follow with `yield_now()` after releasing the lock.
///
/// # Protocol
/// ```text
/// {
///     let mut guard = resource.lock();
///     guard.set_waiter(thread_idx);
///     scheduler::mark_blocked();   // under the lock
/// }                                // lock released
/// scheduler::yield_now();          // switch away
/// ```
// [spec: completion_port_fixed.tla MarkBlocked — sets thread_state := "blocked"
//  while the port lock is still held, closing the lost-wakeup window.]
pub fn mark_blocked() {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut sched = SCHEDULER.lock();
        let idx = sched.current_idx;
        sched.threads[idx].state = ThreadState::Blocked;
    });
}

/// Mark the current thread as blocked and yield.
///
/// Convenience wrapper: `mark_blocked()` + `yield_now()`.
/// Prefer calling `mark_blocked()` under the caller's lock and `yield_now()`
/// after releasing it for new code — see [`mark_blocked`] doc comment.
/// This function exists for backward compatibility with sites that cannot
/// easily restructure their locking (e.g. `sys_wait4`, `blocking()`).
// [spec: completion_port.tla Block — the split mark_blocked + yield_now avoids
//  the lost-wakeup race documented in completion_port_fixed.tla.]
pub fn block_current_thread() {
    mark_blocked();
    yield_now();
}

/// Unblock a previously blocked thread, placing it back on the ready queue.
///
/// Safe to call from any context including ISR.
// [spec: completion_port.tla Post — "if thread_state = blocked then
//  thread_state := running".  The conditional is the root cause of the
//  lost-wakeup bug: if called before block_current_thread(), state is
//  Running and this is a no-op.]
pub fn unblock(thread_idx: usize) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut sched = SCHEDULER.lock();
        if let Some(t) = sched.threads.get_mut(thread_idx) {
            if t.state == ThreadState::Blocked {
                t.state = ThreadState::Ready;
                sched.ready_queue.push_back(thread_idx);
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Voluntary yield / scheduler donate

/// Direct-switch target for `yield_tick`.  `usize::MAX` means "no target".
static DONATE_TARGET: AtomicUsize = AtomicUsize::new(usize::MAX);

/// Trigger a voluntary context switch via `int 0x50`.
///
/// If a donate target has been set via [`set_donate_target`], the current
/// thread switches directly to that thread.  Otherwise the current thread is
/// re-queued and the scheduler picks the next ready thread.
///
/// Must NOT be called from ISR context (the scheduler lock could deadlock).
pub fn yield_now() {
    unsafe { core::arch::asm!("int 0x50"); }
}

/// Set the direct-switch target for the next [`yield_now`].
///
/// The target thread should already be in the Ready state (e.g. via
/// [`unblock`]).  Cleared atomically by `yield_tick` after use.
pub fn set_donate_target(thread_idx: usize) {
    DONATE_TARGET.store(thread_idx, Ordering::Release);
}

/// Unblock a thread and immediately switch to it.
///
/// Combines [`unblock`], [`set_donate_target`], and [`yield_now`] — the waker
/// thread switches directly to the woken thread without waiting for the
/// scheduler's round-robin.
///
/// Must only be called from syscall/kernel thread context, NOT from ISR.
pub fn unblock_yield(thread_idx: usize) {
    unblock(thread_idx);
    set_donate_target(thread_idx);
    yield_now();
}

// ---------------------------------------------------------------------------
// preempt_tick helpers — extracted to shrink the unsafe surface

/// Save the current thread's RSP, user RSP, and FS_BASE.
fn save_current_context(thread: &mut Thread, current_rsp: u64) {
    thread.saved_rsp = current_rsp;
    thread.user_rsp = crate::syscall::get_user_rsp();
    thread.fs_base = crate::msr::read_fs_base();
}

/// Snapshot of the state needed to restore a thread after the scheduler lock
/// is released.
struct SwitchTarget {
    next_rsp: u64,
    next_user_rsp: u64,
    next_fs_base: u64,
    cur_pml4: u64,
    next_pml4: u64,
    next_kind: SchedulableKind,
    next_kstack_top: u64,
    next_idx: usize,
}

/// Restore CPU state for the incoming thread: user RSP, FS_BASE, TSS rsp0,
/// PER_CPU kernel RSP, current PID, and address space.
fn restore_thread_state(target: &SwitchTarget) {
    crate::syscall::set_user_rsp(target.next_user_rsp);
    // Safety: `next_fs_base` was saved from a valid thread's FS_BASE in
    // `save_current_context` and is being restored to the same thread.
    unsafe { crate::msr::write_fs_base(target.next_fs_base); }

    match target.next_kind {
        SchedulableKind::UserProcess(pid) => {
            crate::gdt::set_kernel_stack(x86_64::VirtAddr::new(target.next_kstack_top));
            crate::syscall::set_kernel_rsp(target.next_kstack_top);
            crate::process::set_current_pid(pid);
        }
        SchedulableKind::Kernel => {
            crate::process::set_current_pid(ProcessId::KERNEL);
        }
    }

    if target.next_pml4 != target.cur_pml4 {
        // Safety: `next_pml4` was read from the Thread struct and is a valid
        // PML4 physical address set up during process creation.
        unsafe {
            crate::memory::switch_address_space(x86_64::PhysAddr::new(target.next_pml4));
        }
    }

    CONTEXT_SWITCHES.fetch_add(1, Ordering::Relaxed);
    CURRENT_THREAD_IDX_ATOMIC.store(target.next_idx, Ordering::Relaxed);
    crate::vga_buffer::timeline_append(target.next_idx);
}

/// Sanity-check alignment for a brand-new thread's first dispatch.
///
/// The stub will pop 15 GPRs then iretq.  For a new thread the iretq RSP
/// should be 8 mod 16 (SysV ABI entry alignment).
fn debug_check_initial_alignment(next_rsp: u64, next_idx: usize) {
    // Safety: `next_rsp + FXSAVE_SIZE` points to a SwitchFrame that was
    // written by `spawn_thread` / `spawn_user_thread` on a valid kernel stack.
    let frame = unsafe { &*((next_rsp + FXSAVE_SIZE as u64) as *const SwitchFrame) };
    if frame.rip == (process_trampoline as *const () as u64)
        || frame.rsp == next_rsp
    {
        if frame.rsp & 0xF != 8 {
            serial_println!("[preempt_tick] WARNING: thread {} initial iretq RSP={:#x} (mod16={}) — MISALIGNED!",
                next_idx, frame.rsp, frame.rsp & 0xF);
        }
    }
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
    crate::task::timer::tick();
    crate::interrupts::lapic_eoi();

    let mut sched = SCHEDULER.lock();

    if !sched.initialized {
        return current_rsp;
    }

    let current_idx = sched.current_idx;

    // Decrement the running thread's quantum; keep running if ticks remain
    // (unless the thread is Dead or Blocked, in which case we must switch away).
    if sched.threads[current_idx].state.is_runnable() {
        sched.threads[current_idx].ticks_remaining -= 1;
        if sched.threads[current_idx].ticks_remaining > 0 {
            return current_rsp;
        }
    }

    save_current_context(&mut sched.threads[current_idx], current_rsp);

    // Only re-queue the thread if it's runnable (not Dead or Blocked).
    if sched.threads[current_idx].state.is_runnable() {
        sched.threads[current_idx].state = ThreadState::Ready;
        sched.ready_queue.push_back(current_idx);
    }

    // Round-robin: pop from the front of the ready queue, or fall back to
    // the idle thread when nothing else is runnable.
    let next_idx = match sched.ready_queue.pop_front() {
        Some(idx) => idx,
        None => sched.idle_thread_idx,
    };

    // Fast path: if we'd switch back to ourselves, just reset quantum.
    if next_idx == current_idx {
        sched.threads[current_idx].state = ThreadState::Running;
        sched.threads[current_idx].ticks_remaining = QUANTUM_TICKS;
        return current_rsp;
    }

    sched.current_idx = next_idx;
    sched.threads[next_idx].state = ThreadState::Running;
    sched.threads[next_idx].ticks_remaining = QUANTUM_TICKS;

    let target = SwitchTarget {
        next_rsp:       sched.threads[next_idx].saved_rsp,
        next_user_rsp:  sched.threads[next_idx].user_rsp,
        next_fs_base:   sched.threads[next_idx].fs_base,
        cur_pml4:       sched.threads[current_idx].pml4_phys,
        next_pml4:      sched.threads[next_idx].pml4_phys,
        next_kind:      sched.threads[next_idx].kind,
        next_kstack_top: sched.threads[next_idx].kernel_stack_top,
        next_idx,
    };
    drop(sched);

    restore_thread_state(&target);
    debug_check_initial_alignment(target.next_rsp, target.next_idx);

    target.next_rsp
}

/// Voluntary yield handler — called by `ipc_yield_stub` (vector 0x50).
///
/// Like [`preempt_tick`] but without timer bookkeeping (no tick, no EOI, no
/// quantum check).  If `DONATE_TARGET` is set and the target thread is Ready,
/// switches directly to it; otherwise picks the next ready thread.
///
/// # Safety
/// Must only be called from `ipc_yield_stub` with interrupts disabled.
#[no_mangle]
unsafe extern "C" fn yield_tick(current_rsp: u64) -> u64 {
    let mut sched = SCHEDULER.lock();

    if !sched.initialized {
        return current_rsp;
    }

    let current_idx = sched.current_idx;
    save_current_context(&mut sched.threads[current_idx], current_rsp);

    // Re-queue the current thread if it's still runnable.
    if sched.threads[current_idx].state.is_runnable() {
        sched.threads[current_idx].state = ThreadState::Ready;
        sched.ready_queue.push_back(current_idx);
    }

    // Check for a direct-switch (donate) target.
    let donate = DONATE_TARGET.swap(usize::MAX, Ordering::Acquire);
    let next_idx = if donate != usize::MAX
        && donate < sched.threads.len()
        && sched.threads[donate].state == ThreadState::Ready
    {
        // Remove donate target from the ready queue (it was pushed by unblock).
        if let Some(pos) = sched.ready_queue.iter().position(|&i| i == donate) {
            sched.ready_queue.remove(pos);
        }
        donate
    } else {
        match sched.ready_queue.pop_front() {
            Some(idx) => idx,
            None => sched.idle_thread_idx,
        }
    };

    // Fast path: if we'd switch back to ourselves, just reset quantum.
    if next_idx == current_idx {
        sched.threads[current_idx].state = ThreadState::Running;
        sched.threads[current_idx].ticks_remaining = QUANTUM_TICKS;
        return current_rsp;
    }

    sched.current_idx = next_idx;
    sched.threads[next_idx].state = ThreadState::Running;
    sched.threads[next_idx].ticks_remaining = QUANTUM_TICKS;

    let target = SwitchTarget {
        next_rsp:       sched.threads[next_idx].saved_rsp,
        next_user_rsp:  sched.threads[next_idx].user_rsp,
        next_fs_base:   sched.threads[next_idx].fs_base,
        cur_pml4:       sched.threads[current_idx].pml4_phys,
        next_pml4:      sched.threads[next_idx].pml4_phys,
        next_kind:      sched.threads[next_idx].kind,
        next_kstack_top: sched.threads[next_idx].kernel_stack_top,
        next_idx,
    };
    drop(sched);

    restore_thread_state(&target);
    debug_check_initial_alignment(target.next_rsp, target.next_idx);

    target.next_rsp
}
