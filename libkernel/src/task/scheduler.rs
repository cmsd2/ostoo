use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use spin::Mutex;

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
    Dead,
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
    /// Owned stack backing. None for thread 0 (uses the boot stack) and for
    /// user process threads (kernel stack owned by Process).
    _stack: Option<Vec<u8>>,
    /// What this thread represents.
    kind: SchedulableKind,
    /// For user process threads: the top of the process's kernel stack.
    /// Used to update TSS.rsp0 and PER_CPU on context switch.
    kernel_stack_top: u64,
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
// Switch frame — matches the layout pushed by `lapic_timer_stub`

/// Saved register frame written to the stack by the timer stub and read back
/// on context switch.  Must match the push/pop order in `lapic_timer_stub`
/// exactly: 15 GPRs followed by the hardware iretq frame.
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

/// Allocate a heap stack and switch RSP to it, then call `continuation`.
///
/// This moves the boot thread off the bootloader's lower-half stack onto a
/// heap-backed stack (PML4 entry 256, high canonical half).  The old stack is
/// abandoned — `continuation` must never return.
///
/// Call once, after the heap allocator is initialised.
pub fn migrate_to_heap_stack(continuation: fn() -> !) -> ! {
    const STACK_SIZE: usize = 64 * 1024;
    let mut stack: Vec<u8> = Vec::with_capacity(STACK_SIZE);
    stack.resize(STACK_SIZE, 0u8);
    let stack_top = (stack.as_ptr() as u64 + stack.len() as u64) & !0xF;
    core::mem::forget(stack); // leaked — permanent stack for thread 0
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

    let frame = SwitchFrame::new_kernel(entry, stack_top);
    let saved_rsp = stack_top - core::mem::size_of::<SwitchFrame>() as u64;
    // Safety: saved_rsp points into our just-allocated, zeroed stack.
    unsafe { core::ptr::write(saved_rsp as *mut SwitchFrame, frame); }

    let (cr3_frame, _) = x86_64::registers::control::Cr3::read();
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut sched = SCHEDULER.lock();
        let id = ThreadId::new();
        let idx = sched.threads.len();
        sched.threads.push(Thread {
            id,
            state: ThreadState::Ready,
            saved_rsp,
            pml4_phys: cr3_frame.start_address().as_u64(),
            ticks_remaining: QUANTUM_TICKS,
            _stack: Some(stack),
            kind: SchedulableKind::Kernel,
            kernel_stack_top: 0,
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
    let saved_rsp = stack_top - core::mem::size_of::<SwitchFrame>() as u64;
    // Safety: saved_rsp points into the process's kernel stack.
    unsafe { core::ptr::write(saved_rsp as *mut SwitchFrame, frame); }

    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut sched = SCHEDULER.lock();
        let id = ThreadId::new();
        let idx = sched.threads.len();
        sched.threads.push(Thread {
            id,
            state: ThreadState::Ready,
            saved_rsp,
            pml4_phys: pml4_phys.as_u64(),
            ticks_remaining: QUANTUM_TICKS,
            _stack: None, // kernel stack owned by Process, not Thread
            kind: SchedulableKind::UserProcess(pid),
            kernel_stack_top: stack_top,
        });
        sched.ready_queue.push_back(idx);
        idx
    })
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
        drop_to_ring3(entry_point, user_stack_top, pml4_phys, user_cs, user_ss, per_cpu);
    }
}

/// Switch to ring 3: set GS MSRs, load CR3, and execute iretq.
///
/// # Safety
/// - Interrupts must be disabled.
/// - `pml4_phys` must be a valid PML4 physical address.
/// - `entry` and `user_rsp` must be valid user-space addresses.
/// - `user_cs` and `user_ss` must be valid ring-3 segment selectors.
/// - `per_cpu` must point to the kernel's `PerCpuData` block.
unsafe fn drop_to_ring3(
    entry: u64, user_rsp: u64, pml4_phys: u64,
    user_cs: u64, user_ss: u64, per_cpu: u64,
) -> ! {
    // Explicitly set GS MSRs instead of using `swapgs`, because the GS
    // polarity is unpredictable: a timer preemption of a previous user process
    // leaves user GS active, while a kernel thread leaves kernel GS active.
    // Writing both MSRs directly avoids this ambiguity.
    //   IA32_GS_BASE (0xC000_0101) = 0         → user GS for ring 3
    //   IA32_KERNEL_GS_BASE (0xC000_0102) = per_cpu → restored by syscall swapgs
    core::arch::asm!("cli", options(nostack, nomem));
    x86_64::registers::model_specific::Msr::new(0xC000_0101).write(0);
    x86_64::registers::model_specific::Msr::new(0xC000_0102).write(per_cpu);

    core::arch::asm!(
        "mov cr3, {pml4}",
        "push {ss}",
        "push {usp}",
        "push {rf}",
        "push {cs}",
        "push {ip}",
        "iretq",
        pml4  = in(reg) pml4_phys,
        ss    = in(reg) user_ss,
        usp   = in(reg) user_rsp,
        rf    = in(reg) RFLAGS_IF,
        cs    = in(reg) user_cs,
        ip    = in(reg) entry,
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
/// The timer ISR will see `Dead` and not re-queue the thread.  This function
/// never returns — it enables interrupts and spins until preempted.
pub fn kill_current_thread() -> ! {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut sched = SCHEDULER.lock();
        let idx = sched.current_idx;
        sched.threads[idx].state = ThreadState::Dead;
    });
    // Enable interrupts and spin; the timer will preempt us and we won't be
    // re-queued because our state is Dead.
    loop {
        x86_64::instructions::interrupts::enable_and_hlt();
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
    // Advance the global tick counter and wake any sleeping tasks.
    crate::task::timer::tick();
    // Acknowledge the interrupt so the LAPIC can deliver the next one.
    crate::interrupts::lapic_eoi();

    let mut sched = SCHEDULER.lock();

    if !sched.initialized {
        return current_rsp;
    }

    let current_idx = sched.current_idx;

    // Decrement the running thread's quantum; keep running if ticks remain
    // (unless the thread is Dead, in which case we must switch away).
    if sched.threads[current_idx].state != ThreadState::Dead {
        sched.threads[current_idx].ticks_remaining -= 1;
        if sched.threads[current_idx].ticks_remaining > 0 {
            return current_rsp;
        }
    }

    // Save context of the current thread.
    sched.threads[current_idx].saved_rsp = current_rsp;

    // Only re-queue the thread if it's not Dead.
    if sched.threads[current_idx].state != ThreadState::Dead {
        sched.threads[current_idx].state = ThreadState::Ready;
        sched.ready_queue.push_back(current_idx);
    }

    // Round-robin: pop from the front of the ready queue.
    let next_idx = match sched.ready_queue.pop_front() {
        Some(idx) => idx,
        None => return current_rsp, // no other thread ready; stay on current
    };

    sched.current_idx = next_idx;
    sched.threads[next_idx].state = ThreadState::Running;
    sched.threads[next_idx].ticks_remaining = QUANTUM_TICKS;

    let next_rsp         = sched.threads[next_idx].saved_rsp;
    let cur_pml4         = sched.threads[current_idx].pml4_phys;
    let next_pml4        = sched.threads[next_idx].pml4_phys;
    let next_kind        = sched.threads[next_idx].kind;
    let next_kstack_top  = sched.threads[next_idx].kernel_stack_top;
    drop(sched); // release before touching other subsystems

    // Update TSS.rsp0 and PER_CPU for user process threads; reset PID for kernel threads.
    match next_kind {
        SchedulableKind::UserProcess(pid) => {
            crate::gdt::set_kernel_stack(x86_64::VirtAddr::new(next_kstack_top));
            crate::syscall::set_kernel_rsp(next_kstack_top);
            crate::process::set_current_pid(pid);
        }
        SchedulableKind::Kernel => {
            crate::process::set_current_pid(ProcessId::KERNEL);
        }
    }

    // Switch address space when the new thread lives in a different PML4.
    // Kernel threads all share the boot PML4 so this is a no-op for them.
    if next_pml4 != cur_pml4 {
        unsafe {
            crate::memory::switch_address_space(x86_64::PhysAddr::new(next_pml4));
        }
    }

    CONTEXT_SWITCHES.fetch_add(1, Ordering::Relaxed);
    CURRENT_THREAD_IDX_ATOMIC.store(next_idx, Ordering::Relaxed);
    crate::vga_buffer::timeline_append(next_idx);

    // Sanity-check: the stub will pop 15 GPRs then iretq.  For a brand-new
    // thread the iretq RSP should be 8 mod 16 (SysV ABI entry alignment).
    // We check the saved RIP: if it matches a known entry function, this is a
    // first dispatch and we validate RSP alignment.
    let frame = unsafe { &*(next_rsp as *const SwitchFrame) };
    if frame.rip == (process_trampoline as *const () as u64)
        || frame.rsp == next_rsp  // old pattern where RSP == saved_rsp
    {
        if frame.rsp & 0xF != 8 {
            serial_println!("[preempt_tick] WARNING: thread {} initial iretq RSP={:#x} (mod16={}) — MISALIGNED!",
                next_idx, frame.rsp, frame.rsp & 0xF);
        }
    }

    next_rsp
}
