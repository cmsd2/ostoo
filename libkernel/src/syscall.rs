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
        PerCpuCell(UnsafeCell::new(PerCpuData { kernel_rsp: 0, user_rsp: 0 }))
    }
    fn get(&self) -> *mut PerCpuData {
        self.0.get()
    }
}

static PER_CPU: PerCpuCell = PerCpuCell::new();

/// Dedicated kernel stack for SYSCALL entry (64 KiB).
const SYSCALL_STACK_SIZE: usize = 64 * 1024;
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
        // IA32_GS_BASE (0xC000_0101): GS.BASE in ring 0 = kernel per-CPU.
        Msr::new(0xC000_0101).write(per_cpu_ptr as u64);
        // IA32_KERNEL_GS_BASE (0xC000_0102): restored after swapgs = user GS.
        // Initially 0; will be set by arch_prctl(ARCH_SET_GS) when musl TLS
        // is initialised.  The syscall stub swaps on entry and exit.
        Msr::new(0xC000_0102).write(0);

        // IA32_STAR (0xC000_0081):
        //   bits[47:32] = kernel CS  → SYSCALL sets CS=kernel_cs, SS=kernel_cs+8
        //   bits[63:48] = user CS-16 → SYSRETQ sets CS=(+16)|3, SS=(+8)|3
        let star = ((kernel_cs as u64) << 32)
            | (((user_cs as u64).wrapping_sub(16)) << 48);
        Msr::new(0xC000_0081).write(star);

        // IA32_LSTAR (0xC000_0082): 64-bit SYSCALL entry point.
        Msr::new(0xC000_0082).write(syscall_entry as *const () as u64);

        // IA32_FMASK (0xC000_0084): bits to clear in RFLAGS on SYSCALL.
        // Clear IF (bit 9) to prevent interrupts in the entry stub, and
        // DF (bit 10) for the C ABI string direction convention.
        Msr::new(0xC000_0084).write(0x0000_0300);

        // Enable SCE (Syscall Enable) in IA32_EFER (bit 0).
        let efer = Msr::new(0xC000_0080).read();
        Msr::new(0xC000_0080).write(efer | 1);
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
    mov  rsp, gs:0              /* load kernel RSP from per_cpu.kernel_rsp (offset 0) */

    push rcx                    /* save user RIP   */
    push r11                    /* save user RFLAGS */

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

    sub  rsp, 8                 /* 16-byte align for call */
    call syscall_dispatch        /* returns i64 in rax */
    add  rsp, 8

    pop  r11                    /* restore user RFLAGS */
    pop  rcx                    /* restore user RIP    */

    mov  rsp, gs:8              /* restore user RSP    */
    swapgs                      /* restore user GS     */
    sysretq
"#);

// ---------------------------------------------------------------------------
// Dispatch

/// Called from the assembly stub with the SysV64 calling convention.
#[no_mangle]
extern "sysv64" fn syscall_dispatch(
    nr: u64,
    a1: u64, a2: u64, a3: u64,
    _a4: u64, _a5: u64,
) -> i64 {
    match nr {
        1       => sys_write(a1, a2, a3),
        60 | 231 => sys_exit(a1 as i32),
        158     => sys_arch_prctl(a1, a2),
        _       => -(38i64), // ENOSYS
    }
}

fn sys_write(fd: u64, buf: u64, count: u64) -> i64 {
    if fd != 1 && fd != 2 {
        return -(9i64); // EBADF
    }
    // Validate that the entire buffer falls within user address space.
    const USER_LIMIT: u64 = 0x0000_8000_0000_0000;
    if buf == 0 || count > USER_LIMIT || buf.checked_add(count).map_or(true, |end| end > USER_LIMIT) {
        return -(14i64); // EFAULT
    }
    // Safety: we have validated that buf..buf+count is within user space.
    let bytes = unsafe { core::slice::from_raw_parts(buf as *const u8, count as usize) };
    if let Ok(s) = core::str::from_utf8(bytes) {
        crate::print!("{}", s);
    }
    count as i64
}

fn sys_exit(code: i32) -> i64 {
    let pid = crate::process::current_pid();
    if pid != crate::process::ProcessId::KERNEL {
        crate::println!("\n[kernel] pid {} exited with code {}", pid.as_u64(), code);
        crate::process::mark_zombie(pid, code);
        // Don't reap here — we're still running on the process's kernel stack.
        // The Process (and its kernel stack) leaks as a zombie until a future
        // wait()/reaper is implemented.
    } else {
        crate::println!("\n[kernel] kernel sys_exit({}) — halting", code);
    }
    crate::task::scheduler::kill_current_thread();
}

fn sys_arch_prctl(code: u64, addr: u64) -> i64 {
    const ARCH_SET_FS: u64 = 0x1002;
    match code {
        ARCH_SET_FS => {
            // Set FS_BASE MSR for musl TLS.
            unsafe { x86_64::registers::model_specific::Msr::new(0xC000_0100).write(addr); }
            0
        }
        _ => -(22i64), // EINVAL
    }
}

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

/// Returns the top of the dedicated kernel syscall stack, suitable for
/// storing in TSS.rsp0 so hardware interrupts from ring 3 land on it.
pub fn kernel_stack_top() -> VirtAddr {
    VirtAddr::new(SYSCALL_STACK.0.as_ptr_range().end as u64)
}
