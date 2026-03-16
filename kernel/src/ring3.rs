//! Ring-3 test helpers invoked by the shell `test` command.

use libkernel::memory::with_memory;
use x86_64::structures::paging::PageTableFlags;
use x86_64::VirtAddr;

const USER_CODE_VIRT: u64  = 0x0040_0000;
const USER_STACK_VIRT: u64 = 0x0050_0000;
const USER_STACK_TOP: u64  = USER_STACK_VIRT + 0x1000;

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
// Core helper

/// Allocate a fresh isolated PML4 (kernel high-half shared, user half empty),
/// map code and stack into it, switch CR3, then drop to ring 3 via `iretq`.
///
/// # Why CR3 switch works now
///
/// The kernel is linked at PML4 entry 510 (`0xFFFF_FF00_0000_0000`).
/// `create_user_page_table` copies entries 256–510 from the active PML4,
/// so the new PML4 has the full kernel (code, data, BSS, heap, SYSCALL stack)
/// at high-half addresses.  After `mov cr3`, the very next instruction is
/// still reachable because it is in entry 510.
///
/// Never returns: the user code calls `sys_exit` or triggers a fault, both
/// of which halt via `hlt_loop`.
fn launch_isolated(code: &[u8]) -> ! {
    assert!(code.len() <= 0x1000, "ring3 code blob exceeds one page");

    let user_pml4 = with_memory(|mem| {
        // Allocate code frame and copy the blob into it.
        let code_phys = mem.alloc_dma_pages(1).expect("ring3: out of frames (code)");
        let dst = mem.phys_mem_offset() + code_phys.as_u64();
        unsafe {
            core::ptr::copy_nonoverlapping(code.as_ptr(), dst.as_mut_ptr::<u8>(), code.len());
        }

        // Allocate stack frame.
        let stack_phys = mem.alloc_dma_pages(1).expect("ring3: out of frames (stack)");

        // Create the isolated PML4: kernel half copied, user half empty.
        let pml4_phys = mem.create_user_page_table();

        // Map user code (RX) and stack (RW, NX) into the new address space.
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

    let user_cs = libkernel::gdt::user_code_selector().0 as u64;
    let user_ss = libkernel::gdt::user_data_selector().0 as u64;

    log::info!(
        "ring3: iretq to {:#x}  pml4={:#x}  cs={:#x}  ss={:#x}  rsp={:#x}",
        USER_CODE_VIRT, user_pml4.as_u64(), user_cs, user_ss, USER_STACK_TOP,
    );

    // Tell the scheduler about the new CR3 so preempt_tick switches address
    // spaces correctly when context-switching away from this thread.
    libkernel::task::scheduler::set_current_cr3(user_pml4.as_u64());

    // Use the SYSCALL stack (BSS, PML4 entry 510) for the iretq frame.
    // The boot stack (thread 0) lives in the bootloader's lower-half mapping
    // which is not present in the user PML4, so we must switch RSP to a
    // high-half stack *before* changing CR3.
    let ksp = libkernel::syscall::kernel_stack_top().as_u64();

    unsafe {
        core::arch::asm!(
            "cli",
            "swapgs",
            // Move to a stack that survives the CR3 switch (BSS, entry 510).
            "mov rsp, {ksp}",
            // Switch to the isolated address space.  The kernel (entry 510)
            // remains mapped, so the next instruction fetch succeeds.
            "mov cr3, {pml4}",
            // Build the iretq frame (SS, RSP, RFLAGS, CS, RIP) and return.
            "push {ss}",
            "push {usp}",
            "push {rf}",
            "push {cs}",
            "push {ip}",
            "iretq",
            ksp   = in(reg) ksp,
            pml4  = in(reg) user_pml4.as_u64(),
            ss    = in(reg) user_ss,
            usp   = in(reg) USER_STACK_TOP,
            rf    = in(reg) 0x0202u64,
            cs    = in(reg) user_cs,
            ip    = in(reg) USER_CODE_VIRT,
            options(noreturn),
        );
    }
}

// ---------------------------------------------------------------------------
// Public test entry points

/// Run hello-world via `write` + `exit` syscalls in ring 3.
/// `sys_exit` calls `hlt_loop`; never returns.
pub fn run_hello_isolated() -> ! {
    let code = unsafe {
        let start = &raw const ring3_hello_start as *const u8;
        let end   = &raw const ring3_hello_end   as *const u8;
        core::slice::from_raw_parts(start, end.offset_from(start) as usize)
    };
    launch_isolated(code)
}

/// Touch unmapped 0x700000 in ring 3; page fault handler logs and halts.
/// Never returns.
pub fn run_pagefault_isolated() -> ! {
    let code = unsafe {
        let start = &raw const ring3_fault_start as *const u8;
        let end   = &raw const ring3_fault_end   as *const u8;
        core::slice::from_raw_parts(start, end.offset_from(start) as usize)
    };
    launch_isolated(code)
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
