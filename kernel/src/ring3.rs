//! Ring-3 execution: test helpers and ELF process spawning.

use libkernel::consts::PAGE_SIZE;
use libkernel::memory::with_memory;
use libkernel::process::ProcessId;
use x86_64::structures::paging::PageTableFlags;
use x86_64::VirtAddr;

const USER_CODE_VIRT: u64  = 0x0040_0000;
const USER_STACK_VIRT: u64 = 0x0050_0000;
const USER_STACK_TOP: u64  = USER_STACK_VIRT + PAGE_SIZE;


// ---------------------------------------------------------------------------
// Assembly blobs

core::arch::global_asm!(r#"
.global ring3_hello_start
.global ring3_hello_end
ring3_hello_start:
    mov  rax, 1
    mov  rdi, 1
    lea  rsi, [rip + ring3_hello_msg]
    mov  rdx, 19
    syscall
    mov  rax, 60
    xor  edi, edi
    syscall
ring3_hello_msg:
    .ascii "Hello from ring 3!\n"
ring3_hello_end:
"#);

// Touch unmapped 0x700000 to trigger a ring-3 page fault.
core::arch::global_asm!(r#"
.global ring3_fault_start
.global ring3_fault_end
ring3_fault_start:
    mov  rax, 0x700000
    mov  rax, [rax]
    mov  rax, 60
    xor  edi, edi
    syscall
ring3_fault_end:
"#);

extern "C" {
    static ring3_hello_start: u8;
    static ring3_hello_end:   u8;
    static ring3_fault_start: u8;
    static ring3_fault_end:   u8;
}

// ---------------------------------------------------------------------------
// Raw code blob spawning (used by test commands)

/// Spawn a user process from a raw code blob (not ELF).
///
/// Maps the blob at `USER_CODE_VIRT` (RX) and a stack at `USER_STACK_VIRT`
/// (RW, NX), creates a Process, and places it on the scheduler.  Returns
/// immediately — the process runs when scheduled.
fn spawn_blob(code: &[u8]) -> ProcessId {
    // Free kernel stacks of previously exited processes so the heap doesn't run out.
    libkernel::process::reap_zombies();

    assert!(code.len() <= PAGE_SIZE as usize, "ring3 code blob exceeds one page");

    let pml4_phys = with_memory(|mem| {
        let code_phys = mem.alloc_dma_pages(1).expect("ring3: out of frames (code)");
        let dst = mem.phys_mem_offset() + code_phys.as_u64();
        unsafe {
            libkernel::consts::clear_page(dst.as_mut_ptr::<u8>());
            core::ptr::copy_nonoverlapping(code.as_ptr(), dst.as_mut_ptr::<u8>(), code.len());
        }

        let stack_phys = mem.alloc_dma_pages(1).expect("ring3: out of frames (stack)");
        let stack_dst = mem.phys_mem_offset() + stack_phys.as_u64();
        unsafe { libkernel::consts::clear_page(stack_dst.as_mut_ptr::<u8>()); }

        let pml4_phys = mem.create_user_page_table();

        mem.map_user_page(
            pml4_phys,
            VirtAddr::new(USER_CODE_VIRT),
            code_phys,
            PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE,
        ).expect("ring3: failed to map code page");

        mem.map_user_page(
            pml4_phys,
            VirtAddr::new(USER_STACK_VIRT),
            stack_phys,
            PageTableFlags::PRESENT
                | PageTableFlags::WRITABLE
                | PageTableFlags::USER_ACCESSIBLE
                | PageTableFlags::NO_EXECUTE,
        ).expect("ring3: failed to map stack page");

        pml4_phys
    });

    let proc = libkernel::process::Process::new(pml4_phys, USER_CODE_VIRT, USER_STACK_TOP, 0);
    let pid = proc.pid;
    libkernel::process::insert(proc);

    let thread_idx = libkernel::task::scheduler::spawn_user_thread(pid, pml4_phys);
    libkernel::process::with_process(pid, |p| {
        p.thread_idx = Some(thread_idx);
    });

    pid
}

// ---------------------------------------------------------------------------
// Public test entry points

/// Spawn the hello-world ring-3 test as a proper process.
/// Returns the PID; the shell continues running.
pub fn run_hello_isolated() -> ProcessId {
    let code = unsafe {
        let start = &raw const ring3_hello_start as *const u8;
        let end   = &raw const ring3_hello_end   as *const u8;
        core::slice::from_raw_parts(start, end.offset_from(start) as usize)
    };
    spawn_blob(code)
}

/// Spawn the page-fault ring-3 test as a proper process.
/// Returns the PID; the shell continues running.
pub fn run_pagefault_isolated() -> ProcessId {
    let code = unsafe {
        let start = &raw const ring3_fault_start as *const u8;
        let end   = &raw const ring3_fault_end   as *const u8;
        core::slice::from_raw_parts(start, end.offset_from(start) as usize)
    };
    spawn_blob(code)
}

// ---------------------------------------------------------------------------
// ELF process spawning

/// Parse an ELF binary, create a user address space with its segments mapped,
/// and spawn a scheduler thread for it.  Returns the new process's PID.
///
/// Legacy entry point (no argv/envp, kernel as parent).
pub fn spawn_process(elf_data: &[u8]) -> Result<ProcessId, &'static str> {
    spawn_process_with_env(elf_data, &[])
}

/// Spawn with initial environment variables (kernel as parent).
pub fn spawn_process_with_env(elf_data: &[u8], envp: &[&[u8]]) -> Result<ProcessId, &'static str> {
    osl::spawn::spawn_process_full(elf_data, &[], envp, libkernel::process::ProcessId::KERNEL)
}

/// Kernel-mode test: verify that two independently-created PML4s have
/// genuinely independent mappings at the same user virtual address.
/// Does not modify the active address space; returns to the caller.
pub fn test_isolation() -> bool {
    use x86_64::structures::paging::{
        OffsetPageTable, PageTable,
        mapper::{MappedFrame, Translate, TranslateResult},
    };
    use x86_64::PhysAddr;

    with_memory(|mem| {
        let pml4_a = mem.create_user_page_table();
        let pml4_b = mem.create_user_page_table();

        let frame_a = match mem.alloc_dma_pages(1) { Some(p) => p, None => return false };
        let frame_b = match mem.alloc_dma_pages(1) { Some(p) => p, None => return false };

        let flags   = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
        let test_va = VirtAddr::new(USER_CODE_VIRT);

        if mem.map_user_page(pml4_a, test_va, frame_a, flags).is_err() { return false; }
        if mem.map_user_page(pml4_b, test_va, frame_b, flags).is_err() { return false; }

        let phys_off = mem.phys_mem_offset();

        let translate = |pml4_phys: PhysAddr| -> Option<PhysAddr> {
            let virt = phys_off + pml4_phys.as_u64();
            let pml4: &mut PageTable = unsafe { &mut *virt.as_mut_ptr() };
            let table = unsafe { OffsetPageTable::new(pml4, phys_off) };
            match table.translate(test_va) {
                TranslateResult::Mapped { frame, offset, .. } => {
                    let base = match frame {
                        MappedFrame::Size4KiB(f) => f.start_address(),
                        MappedFrame::Size2MiB(f) => f.start_address(),
                        MappedFrame::Size1GiB(f) => f.start_address(),
                    };
                    Some(base + offset)
                }
                _ => None,
            }
        };

        let pa_in_a = match translate(pml4_a) { Some(p) => p, None => return false };
        let pa_in_b = match translate(pml4_b) { Some(p) => p, None => return false };

        pa_in_a == frame_a && pa_in_b == frame_b && pa_in_a != pa_in_b
    })
}
