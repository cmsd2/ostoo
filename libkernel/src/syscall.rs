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
// Dispatch

/// Called from the assembly stub with the SysV64 calling convention.
#[no_mangle]
extern "sysv64" fn syscall_dispatch(
    nr: u64,
    a1: u64, a2: u64, a3: u64,
    a4: u64, a5: u64,
) -> i64 {
    match nr {
        0        => sys_read(a1, a2, a3),
        1        => sys_write(a1, a2, a3),
        2        => sys_open(),
        3        => 0, // close — no-op
        5        => sys_fstat(a1, a2),
        9        => sys_mmap(a1, a2, a3, a4, a5),
        10       => 0, // mprotect — no-op
        11       => 0, // munmap — stub (leak frames)
        12       => sys_brk(a1),
        8        => -(29i64), // lseek — ESPIPE (stdout is not seekable)
        16       => -25i64, // ioctl — ENOTTY
        20       => sys_writev(a1, a2, a3),
        60 | 231 => sys_exit(a1 as i32),
        158      => sys_arch_prctl(a1, a2),
        202      => 0, // futex — stub (single-threaded, lock never contended)
        218      => sys_set_tid_address(),
        273      => 0, // set_robust_list — no-op
        other    => {
            log::warn!("unhandled syscall nr={} a1={:#x} a2={:#x} a3={:#x}",
                other, a1, a2, a3);
            -(38i64) // ENOSYS
        }
    }
}

fn sys_open() -> i64 {
    -(2i64) // ENOENT — no filesystem paths accessible yet
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

fn sys_read(fd: u64, _buf: u64, _count: u64) -> i64 {
    if fd == 0 {
        0 // EOF on stdin
    } else {
        -(9i64) // EBADF
    }
}

fn sys_fstat(_fd: u64, buf: u64) -> i64 {
    // Zero-fill the 144-byte stat struct, then set st_mode = S_IFCHR|0o666
    // at offset 24 (mode field in x86_64 Linux stat).
    const STAT_SIZE: usize = 144;
    const S_IFCHR: u32 = 0o020000;
    let stat_ptr = buf as *mut u8;
    unsafe {
        core::ptr::write_bytes(stat_ptr, 0, STAT_SIZE);
        let mode_ptr = stat_ptr.add(24) as *mut u32;
        mode_ptr.write(S_IFCHR | 0o666);
    }
    0
}

fn sys_set_tid_address() -> i64 {
    crate::process::current_pid().as_u64() as i64
}

fn sys_brk(addr: u64) -> i64 {
    use crate::process;
    use crate::memory::with_memory;
    use x86_64::structures::paging::PageTableFlags;

    let pid = process::current_pid();
    if pid == process::ProcessId::KERNEL {
        return 0;
    }

    // Read current brk state (drop lock immediately).
    let (brk_base, brk_current, pml4_phys) = match process::with_process_ref(pid, |p| {
        (p.brk_base, p.brk_current, p.pml4_phys)
    }) {
        Some(v) => v,
        None => return 0,
    };

    // brk(0) or addr below base: return current break.
    if addr == 0 || addr < brk_base {
        return brk_current as i64;
    }

    let new_brk = (addr + 0xFFF) & !0xFFF; // page-align up
    if new_brk <= brk_current {
        // Shrinking — just update (don't unmap).
        process::with_process(pid, |p| p.brk_current = new_brk);
        return new_brk as i64;
    }

    // Grow: allocate and map new pages.
    let pages_needed = ((new_brk - brk_current) / 0x1000) as usize;
    let ok = with_memory(|mem| {
        let phys_off = mem.phys_mem_offset();
        for i in 0..pages_needed {
            let vaddr = brk_current + (i as u64) * 0x1000;
            let frame = match mem.alloc_dma_pages(1) {
                Some(f) => f,
                None => return false,
            };
            // Zero the frame.
            let dst = phys_off + frame.as_u64();
            unsafe { core::ptr::write_bytes(dst.as_mut_ptr::<u8>(), 0, 0x1000); }
            if mem.map_user_page(
                pml4_phys,
                VirtAddr::new(vaddr),
                frame,
                PageTableFlags::PRESENT
                    | PageTableFlags::WRITABLE
                    | PageTableFlags::USER_ACCESSIBLE
                    | PageTableFlags::NO_EXECUTE,
            ).is_err() {
                return false;
            }
        }
        true
    });

    if ok {
        process::with_process(pid, |p| p.brk_current = new_brk);
        new_brk as i64
    } else {
        brk_current as i64 // return old brk on failure
    }
}

fn sys_writev(fd: u64, iov_ptr: u64, iovcnt: u64) -> i64 {
    if fd != 1 && fd != 2 {
        return -(9i64); // EBADF
    }
    let mut total: usize = 0;
    for i in 0..iovcnt as usize {
        let iov_addr = iov_ptr + (i * 16) as u64;
        let iov_base = unsafe { *(iov_addr as *const u64) };
        let iov_len = unsafe { *((iov_addr + 8) as *const u64) } as usize;
        if iov_len == 0 {
            continue;
        }
        let bytes = unsafe { core::slice::from_raw_parts(iov_base as *const u8, iov_len) };
        if let Ok(s) = core::str::from_utf8(bytes) {
            crate::print!("{}", s);
        } else {
            // Fallback: print printable ASCII chars only.
            for &b in bytes {
                if (b >= 0x20 && b < 0x7F) || b == b'\n' || b == b'\r' || b == b'\t' {
                    crate::print!("{}", b as char);
                }
            }
        }
        total += iov_len;
    }
    total as i64
}

fn sys_mmap(addr: u64, length: u64, _prot: u64, flags: u64, _a5: u64) -> i64 {
    use crate::process;
    use crate::memory::with_memory;
    use x86_64::structures::paging::PageTableFlags;

    const MAP_ANONYMOUS: u64 = 0x20;
    const MAP_PRIVATE: u64 = 0x02;
    const MAP_FIXED: u64 = 0x10;

    // Only support MAP_PRIVATE|MAP_ANONYMOUS (with optional MAP_FIXED rejected).
    if flags & MAP_ANONYMOUS == 0 {
        return -(38i64); // ENOSYS — file-backed not supported
    }
    if flags & MAP_FIXED != 0 && addr != 0 {
        return -(38i64); // ENOSYS — MAP_FIXED not supported
    }
    let _ = MAP_PRIVATE; // always implied

    let pid = process::current_pid();
    if pid == process::ProcessId::KERNEL {
        return -(12i64); // ENOMEM
    }

    let aligned_len = (length + 0xFFF) & !0xFFF;
    let num_pages = (aligned_len / 0x1000) as usize;

    // Read process state (drop lock before memory alloc).
    let (mmap_next, pml4_phys) = match process::with_process_ref(pid, |p| {
        (p.mmap_next, p.pml4_phys)
    }) {
        Some(v) => v,
        None => return -(12i64),
    };

    let region_base = mmap_next - aligned_len;

    let ok = with_memory(|mem| {
        let phys_off = mem.phys_mem_offset();
        for i in 0..num_pages {
            let vaddr = region_base + (i as u64) * 0x1000;
            let frame = match mem.alloc_dma_pages(1) {
                Some(f) => f,
                None => return false,
            };
            let dst = phys_off + frame.as_u64();
            unsafe { core::ptr::write_bytes(dst.as_mut_ptr::<u8>(), 0, 0x1000); }
            if mem.map_user_page(
                pml4_phys,
                VirtAddr::new(vaddr),
                frame,
                PageTableFlags::PRESENT
                    | PageTableFlags::WRITABLE
                    | PageTableFlags::USER_ACCESSIBLE
                    | PageTableFlags::NO_EXECUTE,
            ).is_err() {
                return false;
            }
        }
        true
    });

    if ok {
        process::with_process(pid, |p| {
            p.mmap_next = region_base;
            p.mmap_regions.push((region_base, aligned_len));
        });
        region_base as i64
    } else {
        -(12i64) // ENOMEM
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
