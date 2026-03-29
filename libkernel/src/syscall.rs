use core::arch::global_asm;
use core::cell::UnsafeCell;
use x86_64::VirtAddr;

// ---------------------------------------------------------------------------
// Per-CPU data

/// Per-CPU block accessed by the syscall entry stub via the GS segment.
///
/// Field offsets are hard-coded in the assembly stub — keep in sync.
#[repr(C)]
pub struct PerCpuData {
    /// Kernel RSP loaded on SYSCALL entry. Offset 0.
    pub kernel_rsp: u64,
    /// User RSP saved by the entry stub. Offset 8.
    pub user_rsp: u64,
    /// User RIP (RCX on SYSCALL entry) saved by the entry stub. Offset 16.
    pub user_rip: u64,
    /// User RFLAGS (R11 on SYSCALL entry) saved by the entry stub. Offset 24.
    pub user_rflags: u64,
    /// User R9 saved by the entry stub before the arg shuffle. Offset 32.
    /// Needed by clone: musl's __clone stores the child fn pointer in R9.
    pub user_r9: u64,
    /// Pointer to the SyscallSavedFrame on the kernel stack. Offset 40.
    /// Set by the entry stub after pushing all registers; used by signal delivery.
    pub saved_frame_ptr: u64,
}

/// Layout of the 8 registers pushed on the kernel stack by the SYSCALL entry stub.
/// Matches the push order: rcx, r11, rdi, rsi, rdx, r10, r8, r9.
/// RSP points to the r9 slot (lowest address) after all pushes.
#[repr(C)]
pub struct SyscallSavedFrame {
    pub r9: u64,      // offset 0  (top of stack after pushes)
    pub r8: u64,      // offset 8
    pub r10: u64,     // offset 16
    pub rdx: u64,     // offset 24
    pub rsi: u64,     // offset 32
    pub rdi: u64,     // offset 40
    pub r11: u64,     // offset 48 (user RFLAGS)
    pub rcx: u64,     // offset 56 (user RIP)
}

/// Wrapper for the per-CPU data block, replacing `static mut`.
///
/// Safety invariant: only accessed with interrupts disabled (from the SYSCALL
/// entry stub, from `init`, or from context-switch code that runs with IF=0).
/// Single-CPU, so no concurrent access is possible when IF is clear.
#[repr(transparent)]
struct PerCpuCell(UnsafeCell<PerCpuData>);
unsafe impl Sync for PerCpuCell {}

impl PerCpuCell {
    const fn new() -> Self {
        PerCpuCell(UnsafeCell::new(PerCpuData {
            kernel_rsp: 0, user_rsp: 0, user_rip: 0, user_rflags: 0,
            user_r9: 0, saved_frame_ptr: 0,
        }))
    }
    fn get(&self) -> *mut PerCpuData {
        self.0.get()
    }
}

static PER_CPU: PerCpuCell = PerCpuCell::new();

/// Dedicated kernel stack for SYSCALL entry.
const SYSCALL_STACK_SIZE: usize = crate::consts::KERNEL_STACK_SIZE;
#[repr(align(16))]
struct SyscallStack([u8; SYSCALL_STACK_SIZE]);
static SYSCALL_STACK: SyscallStack = SyscallStack([0; SYSCALL_STACK_SIZE]);

// ---------------------------------------------------------------------------
// Initialisation

/// Initialise the SYSCALL/SYSRET mechanism.
///
/// `kernel_cs` is the kernel code selector (e.g. 0x08).
/// `user_cs` is the user 64-bit code selector (e.g. 0x20).
///
/// Must be called after the GDT has been loaded and after the heap is ready.
pub fn init(kernel_cs: u16, user_cs: u16) {
    use x86_64::registers::model_specific::Msr;

    let per_cpu_ptr = PER_CPU.get();
    let stack_top = SYSCALL_STACK.0.as_ptr_range().end as u64;

    // Safety: called once during boot, single CPU, interrupts disabled.
    unsafe { (*per_cpu_ptr).kernel_rsp = stack_top; }

    unsafe {
        // IA32_GS_BASE: GS.BASE in ring 0 = kernel per-CPU.
        Msr::new(crate::msr::IA32_GS_BASE).write(per_cpu_ptr as u64);
        // IA32_KERNEL_GS_BASE: restored after swapgs = user GS.
        // Initially 0; will be set by arch_prctl(ARCH_SET_GS) when musl TLS
        // is initialised.  The syscall stub swaps on entry and exit.
        Msr::new(crate::msr::IA32_KERNEL_GS_BASE).write(0);

        // IA32_STAR:
        //   bits[47:32] = kernel CS  → SYSCALL sets CS=kernel_cs, SS=kernel_cs+8
        //   bits[63:48] = user CS-16 → SYSRETQ sets CS=(+16)|3, SS=(+8)|3
        let star = ((kernel_cs as u64) << 32)
            | (((user_cs as u64).wrapping_sub(16)) << 48);
        Msr::new(crate::msr::IA32_STAR).write(star);

        // IA32_LSTAR: 64-bit SYSCALL entry point.
        Msr::new(crate::msr::IA32_LSTAR).write(syscall_entry as *const () as u64);

        // IA32_FMASK: bits to clear in RFLAGS on SYSCALL.
        // Clear IF (bit 9) to prevent interrupts in the entry stub, and
        // DF (bit 10) for the C ABI string direction convention.
        Msr::new(crate::msr::IA32_FMASK).write(0x0000_0300);

        // Enable SCE (Syscall Enable) in IA32_EFER (bit 0).
        let efer = Msr::new(crate::msr::IA32_EFER).read();
        Msr::new(crate::msr::IA32_EFER).write(efer | 1);
    }
}

// ---------------------------------------------------------------------------
// Entry stub

extern "C" {
    fn syscall_entry();
}

global_asm!(r#"
.global syscall_entry
syscall_entry:
    /* On entry (SYSCALL hardware state):
       rcx = user RIP, r11 = user RFLAGS, rsp = user RSP
       rax = syscall number, rdi/rsi/rdx/r10/r8/r9 = args 1-6
       IF is cleared by FMASK */

    swapgs                      /* GS.BASE <-> KERNEL_GS_BASE; now GS = per-CPU */
    mov  gs:8, rsp              /* save user RSP to per_cpu.user_rsp  (offset 8) */
    mov  gs:16, rcx             /* save user RIP to per_cpu.user_rip  (offset 16) */
    mov  gs:24, r11             /* save user RFLAGS to per_cpu.user_rflags (offset 24) */
    mov  gs:32, r9              /* save user R9 to per_cpu.user_r9 (offset 32) — needed by clone */
    mov  rsp, gs:0              /* load kernel RSP from per_cpu.kernel_rsp (offset 0) */

    /* Save all user registers that we clobber during the argument shuffle.
       The Linux syscall ABI preserves everything except rax (return value),
       rcx (clobbered by SYSCALL hw), and r11 (clobbered by SYSCALL hw).
       We must restore rdi, rsi, rdx, r8, r9, r10 after syscall_dispatch. */
    push rcx                    /* save user RIP   */
    push r11                    /* save user RFLAGS */
    push rdi                    /* save user rdi (a1) */
    push rsi                    /* save user rsi (a2) */
    push rdx                    /* save user rdx (a3) */
    push r10                    /* save user r10 (a4) */
    push r8                     /* save user r8  (a5) */
    push r9                     /* save user r9  (a6) */

    mov  gs:40, rsp              /* save frame ptr for signal delivery */

    /* Translate syscall ABI -> SysV64 for syscall_dispatch:
       syscall: nr=rax, a1=rdi, a2=rsi, a3=rdx, a4=r10, a5=r8
       SysV64:  rdi,    rsi,    rdx,    rcx,    r8,     r9
       Shuffle without clobbering unread sources: */
    mov  r9,  r8                /* a5 -> 6th SysV arg (r9)  */
    mov  r8,  r10               /* a4 -> 5th SysV arg (r8)  */
    mov  rcx, rdx               /* a3 -> 4th SysV arg (rcx) */
    mov  rdx, rsi               /* a2 -> 3rd SysV arg (rdx) */
    mov  rsi, rdi               /* a1 -> 2nd SysV arg (rsi) */
    mov  rdi, rax               /* nr -> 1st SysV arg (rdi) */

    call syscall_dispatch        /* returns i64 in rax */

    /* Re-save frame ptr: a blocking syscall (e.g. read) may have context-
       switched away, letting another thread's SYSCALL entry overwrite gs:40.
       After `call` returns RSP points to our pushed frame again. */
    mov  gs:40, rsp

    mov  rdi, rax               /* pass syscall return value as arg */
    call check_pending_signals  /* returns (possibly modified) rax */

    /* Restore user registers (rax has the return value from dispatch). */
    pop  r9
    pop  r8
    pop  r10
    pop  rdx
    pop  rsi
    pop  rdi
    pop  r11                    /* restore user RFLAGS */
    pop  rcx                    /* restore user RIP    */

    mov  rsp, gs:8              /* restore user RSP    */
    swapgs                      /* restore user GS     */
    sysretq
"#);


// ---------------------------------------------------------------------------
// Per-process kernel RSP

/// Update the kernel RSP in the per-CPU data block.
///
/// Call this on context switch to a user process so that SYSCALL entry and
/// hardware interrupts from ring 3 land on the correct kernel stack.
pub fn set_kernel_rsp(rsp: u64) {
    // Safety: called from context-switch code with interrupts disabled.
    unsafe { (*PER_CPU.get()).kernel_rsp = rsp; }
}

/// Read the saved user RSP from the per-CPU data block.
///
/// Called by the scheduler on context switch to save the outgoing thread's
/// user RSP (which was written by the SYSCALL entry stub).
pub fn get_user_rsp() -> u64 {
    unsafe { (*PER_CPU.get()).user_rsp }
}

/// Restore the user RSP in the per-CPU data block.
///
/// Called by the scheduler on context switch to restore the incoming thread's
/// user RSP, so the SYSCALL exit stub returns to the correct user stack.
pub fn set_user_rsp(rsp: u64) {
    unsafe { (*PER_CPU.get()).user_rsp = rsp; }
}

/// Address of the kernel per-CPU data block.
///
/// Used by `process_trampoline` to write IA32_KERNEL_GS_BASE explicitly
/// instead of relying on `swapgs` polarity.
pub fn per_cpu_addr() -> u64 {
    PER_CPU.get() as u64
}

// ---------------------------------------------------------------------------
// Helper: prepare to drop to ring 3

/// Set GS.BASE to the user value and KERNEL_GS_BASE to the kernel per-CPU
/// area, ready for the `swapgs` inside the SYSCALL entry stub.
///
/// Call once, just before the first `iretq` to ring 3.
pub fn prepare_swapgs() {
    // After this swapgs:
    //   GS.BASE          = 0    (user GS, initially nothing)
    //   KERNEL_GS_BASE   = &PER_CPU  (kernel per-CPU, restored by entry swapgs)
    unsafe { core::arch::asm!("swapgs", options(nostack, nomem)); }
}

/// Read the saved user RIP from the per-CPU data block.
///
/// During a SYSCALL, RCX holds the return address (user RIP).
/// The entry stub saves it to gs:16. Used by `sys_clone` to create a child
/// that "returns from syscall" at the same instruction.
pub fn get_user_rip() -> u64 {
    unsafe { (*PER_CPU.get()).user_rip }
}

/// Read the saved user RFLAGS from the per-CPU data block.
///
/// During a SYSCALL, R11 holds the user RFLAGS.
/// The entry stub saves it to gs:24. Used by `sys_clone`.
pub fn get_user_rflags() -> u64 {
    unsafe { (*PER_CPU.get()).user_rflags }
}

/// Read the saved user R9 from the per-CPU data block.
///
/// musl's `__clone` stores the child function pointer in R9 before the
/// SYSCALL.  After the clone child "returns from syscall", it does
/// `call *%r9`.  Used by `sys_clone` to capture R9 for the child thread.
pub fn get_user_r9() -> u64 {
    unsafe { (*PER_CPU.get()).user_r9 }
}

/// Read the saved frame pointer from the per-CPU data block.
///
/// Points to the `SyscallSavedFrame` on the kernel stack, written by the
/// SYSCALL entry stub. Used by signal delivery to rewrite the return context.
pub fn get_saved_frame_ptr() -> *mut SyscallSavedFrame {
    unsafe { (*PER_CPU.get()).saved_frame_ptr as *mut SyscallSavedFrame }
}

/// Read the user RSP that was saved by the SYSCALL entry stub into per-CPU.
///
/// This is the user-space RSP at the point of the SYSCALL instruction,
/// needed by signal delivery to construct the signal frame below it.
pub fn get_saved_user_rsp() -> u64 {
    unsafe { (*PER_CPU.get()).user_rsp }
}

/// Set the user RSP in the per-CPU data block (used by rt_sigreturn
/// to restore original user RSP after signal handling).
pub fn set_saved_user_rsp(rsp: u64) {
    unsafe { (*PER_CPU.get()).user_rsp = rsp; }
}

/// Returns the top of the dedicated kernel syscall stack, suitable for
/// storing in TSS.rsp0 so hardware interrupts from ring 3 land on it.
pub fn kernel_stack_top() -> VirtAddr {
    VirtAddr::new(SYSCALL_STACK.0.as_ptr_range().end as u64)
}

// ---------------------------------------------------------------------------
// Signal delivery on SYSCALL return path

/// Called from the assembly stub after `syscall_dispatch` returns.
///
/// Takes the syscall return value (passed in rdi) and returns it in rax.
/// If a signal is pending, delivers it by rewriting the saved frame so
/// that `sysretq` "returns" to the handler instead.
#[no_mangle]
extern "C" fn check_pending_signals(syscall_ret: i64) -> i64 {
    let pid = crate::process::current_pid();
    if pid == crate::process::ProcessId::KERNEL {
        return syscall_ret;
    }

    // Peek at pending & !blocked — avoid locking if nothing to do.
    let deliverable = match crate::process::with_process_ref(pid, |p| {
        p.signal.pending & !p.signal.blocked
    }) {
        Some(d) if d != 0 => d,
        _ => return syscall_ret,
    };

    // Dequeue the lowest signal and get its action.
    let (signum, action) = match crate::process::with_process(pid, |p| {
        if let Some(sig) = p.signal.dequeue() {
            let idx = (sig - 1) as usize;
            Some((sig, p.signal.actions[idx]))
        } else {
            None
        }
    }) {
        Some(Some(v)) => v,
        _ => return syscall_ret,
    };

    let _ = deliverable; // used only for early-exit check above

    use crate::signal::*;

    if action.handler == SIG_IGN {
        return syscall_ret;
    }

    if action.handler == SIG_DFL {
        if SignalState::is_default_ignore(signum) {
            return syscall_ret;
        }
        if SignalState::is_default_terminate(signum) {
            crate::serial_println!("[signal] pid={} killed by signal {}", pid.as_u64(), signum);
            crate::process::terminate_process(pid, 128 + signum as i32);
        }
        return syscall_ret;
    }

    // Deliver signal: construct rt_sigframe on user stack, rewrite saved frame.
    deliver_signal(pid, signum, &action, syscall_ret);
    syscall_ret
}

/// Construct an rt_sigframe on the user stack and rewrite the SYSCALL saved
/// frame so that sysretq "returns" into the signal handler.
fn deliver_signal(
    pid: crate::process::ProcessId,
    signum: u8,
    action: &crate::signal::SigAction,
    syscall_ret: i64,
) {
    use crate::signal::*;

    let frame_ptr = get_saved_frame_ptr();
    let user_rsp = get_saved_user_rsp();
    let orig_rax = syscall_ret as u64;

    let old_blocked = crate::process::with_process_ref(pid, |p| p.signal.blocked)
        .unwrap_or(0);

    // Block sa_mask + the delivered signal during handler execution.
    crate::process::with_process(pid, |p| {
        p.signal.blocked |= action.mask | (1u64 << (signum - 1));
        let unblockable = (1u64 << (SIGKILL - 1)) | (1u64 << (SIGSTOP - 1));
        p.signal.blocked &= !unblockable;
    });

    // rt_sigframe layout (Linux x86_64):
    //   +0x000: pretcode     (8 bytes, sa_restorer address)
    //   +0x008: ucontext_t   (uc_flags 8, uc_link 8, uc_stack 24,
    //                          sigcontext 256, fpstate_ptr 8+reserved 64,
    //                          uc_sigmask 8) = 376 bytes
    //   +0x180: siginfo_t    (128 bytes)
    //   Total: 512 bytes
    //
    // sigcontext (32 u64s): r8 r9 r10 r11 r12 r13 r14 r15
    //   rdi rsi rbp rbx rdx rax rcx rsp rip eflags
    //   {cs,gs,fs,ss} err trapno oldmask cr2
    //   fpstate reserved1[8]
    const PRETCODE_SIZE: u64 = 8;
    const UC_HEADER: u64 = 8 + 8 + 24;           // uc_flags + uc_link + uc_stack
    const SIGCONTEXT_SIZE: u64 = 32 * 8;          // 256 bytes
    const UC_TAIL: u64 = 8;                       // uc_sigmask
    const UCONTEXT_SIZE: u64 = UC_HEADER + SIGCONTEXT_SIZE + UC_TAIL; // 376
    const SIGINFO_SIZE: u64 = 128;
    const FRAME_SIZE: u64 = PRETCODE_SIZE + UCONTEXT_SIZE + SIGINFO_SIZE; // 512

    // ABI: at function entry, RSP % 16 == 8 (as if `call` pushed the
    // return address).  Match Linux's align_sigframe: ((sp-8) & ~15) + 8.
    let sp = user_rsp - FRAME_SIZE;
    let frame_base = ((sp - 8) & !0xF) + 8;

    unsafe { core::ptr::write_bytes(frame_base as *mut u8, 0, FRAME_SIZE as usize); }

    // pretcode
    unsafe { *(frame_base as *mut u64) = action.restorer; }

    // ucontext_t
    let uc_base = frame_base + PRETCODE_SIZE;
    let sc_base = uc_base + UC_HEADER;

    // sigcontext registers (Linux x86_64 order)
    let saved = unsafe { &*frame_ptr };
    unsafe {
        let sc = sc_base as *mut u64;
        sc.add(0).write(saved.r8);
        sc.add(1).write(saved.r9);
        sc.add(2).write(saved.r10);
        sc.add(3).write(saved.r11);     // r11 (user RFLAGS from SYSCALL)
        // r12-r15, rbp, rbx = 0 (not saved by SYSCALL stub; zeroed above)
        sc.add(8).write(saved.rdi);
        sc.add(9).write(saved.rsi);
        sc.add(12).write(saved.rdx);
        sc.add(13).write(orig_rax);     // rax (syscall return value)
        sc.add(14).write(saved.rcx);    // rcx (user RIP from SYSCALL)
        sc.add(15).write(user_rsp);     // rsp
        sc.add(16).write(saved.rcx);    // rip (same as rcx)
        sc.add(17).write(saved.r11);    // eflags (same as r11)
    }

    // uc_sigmask — old blocked mask, restored by rt_sigreturn
    unsafe {
        *((sc_base + SIGCONTEXT_SIZE) as *mut u64) = old_blocked;
    }

    // siginfo_t: just si_signo
    let siginfo_base = uc_base + UCONTEXT_SIZE;
    unsafe { *(siginfo_base as *mut i32) = signum as i32; }

    // Rewrite saved frame so sysretq enters the handler.
    unsafe {
        let frame = &mut *frame_ptr;
        frame.rcx = action.handler;
        frame.rdi = signum as u64;
        if action.flags & SA_SIGINFO != 0 {
            frame.rsi = siginfo_base;
            frame.rdx = uc_base;
        } else {
            frame.rsi = 0;
            frame.rdx = 0;
        }
        frame.r11 = saved.r11;
    }

    set_saved_user_rsp(frame_base);

    crate::serial_println!(
        "[signal] pid={} delivering sig={} handler={:#x} restorer={:#x} frame={:#x} user_rsp={:#x}",
        pid.as_u64(), signum, action.handler, action.restorer, frame_base, user_rsp
    );
}

// ---------------------------------------------------------------------------
// Signal delivery from hardware interrupt context (page fault, #UD, etc.)
//
// Unlike the SYSCALL path, in an interrupt we have an InterruptStackFrame
// (managed by the CPU) rather than a SyscallSavedFrame.  We construct the
// rt_sigframe on the user stack and rewrite the interrupt stack frame so
// that IRETQ enters the handler.

/// Attempt to deliver a signal to the current process from an interrupt handler.
///
/// `stack_frame` is the mutable interrupt stack frame pushed by the CPU.
/// `signum` is the signal number (e.g. SIGSEGV, SIGILL).
/// `fault_addr` is the CR2 value for page faults, or 0 for others.
///
/// Returns `true` if the signal was delivered (caller should return from the
/// handler, letting IRETQ jump to the signal handler).  Returns `false` if
/// no user handler is installed (caller should kill the process).
///
/// # Safety
/// Must only be called from a ring-3 exception handler with the faulting
/// process's page tables still active.  The CPU entered from ring 3 without
/// swapgs, so GS is still user polarity — this function does not use GS.
pub fn deliver_signal_from_interrupt(
    pid: crate::process::ProcessId,
    signum: u8,
    stack_frame: &mut x86_64::structures::idt::InterruptStackFrame,
    fault_addr: u64,
) -> bool {
    use crate::signal::*;

    let action = match crate::process::with_process(pid, |p| {
        let idx = (signum - 1) as usize;
        let act = p.signal.actions[idx];
        // Clear pending bit for this signal since we're delivering it.
        p.signal.pending &= !(1u64 << (signum - 1));
        act
    }) {
        Some(a) => a,
        None => return false,
    };

    // SIG_DFL or SIG_IGN → caller does default action (kill).
    if action.handler == SIG_DFL || action.handler == SIG_IGN {
        return false;
    }

    let user_rsp = stack_frame.stack_pointer.as_u64();
    let user_rip = stack_frame.instruction_pointer.as_u64();
    let user_rflags = stack_frame.cpu_flags.bits();

    let old_blocked = crate::process::with_process_ref(pid, |p| p.signal.blocked)
        .unwrap_or(0);

    // Block sa_mask + the delivered signal during handler execution.
    crate::process::with_process(pid, |p| {
        p.signal.blocked |= action.mask | (1u64 << (signum - 1));
        let unblockable = (1u64 << (SIGKILL - 1)) | (1u64 << (SIGSTOP - 1));
        p.signal.blocked &= !unblockable;
    });

    // Same frame layout as deliver_signal (must match rt_sigreturn).
    const PRETCODE_SIZE: u64 = 8;
    const UC_HEADER: u64 = 8 + 8 + 24;
    const SIGCONTEXT_SIZE: u64 = 32 * 8;
    const UC_TAIL: u64 = 8;
    const UCONTEXT_SIZE: u64 = UC_HEADER + SIGCONTEXT_SIZE + UC_TAIL;
    const SIGINFO_SIZE: u64 = 128;
    const FRAME_SIZE: u64 = PRETCODE_SIZE + UCONTEXT_SIZE + SIGINFO_SIZE;

    let sp = user_rsp - FRAME_SIZE;
    let frame_base = ((sp - 8) & !0xF) + 8;

    // Validate frame_base is in user space.
    if !(0x1000..0x0000_8000_0000_0000).contains(&frame_base) {
        return false;
    }

    unsafe { core::ptr::write_bytes(frame_base as *mut u8, 0, FRAME_SIZE as usize); }

    // pretcode
    unsafe { *(frame_base as *mut u64) = action.restorer; }

    // sigcontext registers
    let uc_base = frame_base + PRETCODE_SIZE;
    let sc_base = uc_base + UC_HEADER;

    unsafe {
        let sc = sc_base as *mut u64;
        // GPRs: zero for most (not available from interrupt frame).
        // Key registers we do have:
        sc.add(14).write(0);              // rcx (was user rip on SYSCALL, 0 here)
        sc.add(15).write(user_rsp);       // rsp
        sc.add(16).write(user_rip);       // rip
        sc.add(17).write(user_rflags);    // eflags
        sc.add(26).write(fault_addr);     // cr2
    }

    // uc_sigmask
    unsafe {
        *((sc_base + SIGCONTEXT_SIZE) as *mut u64) = old_blocked;
    }

    // siginfo_t
    let siginfo_base = uc_base + UCONTEXT_SIZE;
    unsafe {
        *(siginfo_base as *mut i32) = signum as i32;
        // si_addr at offset 16 in siginfo_t (for SIGSEGV/SIGBUS).
        *((siginfo_base + 16) as *mut u64) = fault_addr;
    }

    // Rewrite interrupt stack frame so IRETQ enters the handler.
    unsafe {
        stack_frame.as_mut().update(|f| {
            f.instruction_pointer = x86_64::VirtAddr::new(action.handler);
            f.stack_pointer = x86_64::VirtAddr::new(frame_base);
        });
    }

    crate::serial_println!(
        "[signal] pid={} delivering sig={} (interrupt) handler={:#x} frame={:#x}",
        pid.as_u64(), signum, action.handler, frame_base
    );

    true
}
