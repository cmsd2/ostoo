//! Ring-3 execution: test helpers and ELF process spawning.

use libkernel::memory::with_memory;
use libkernel::process::ProcessId;
use x86_64::structures::paging::PageTableFlags;
use x86_64::VirtAddr;

const USER_CODE_VIRT: u64  = 0x0040_0000;
const USER_STACK_VIRT: u64 = 0x0050_0000;
const USER_STACK_TOP: u64  = USER_STACK_VIRT + 0x1000;

/// 8-page (32 KiB) user stack for ELF processes, placed at a high user address.
const ELF_STACK_PAGES: usize = 8;
const ELF_STACK_SIZE: u64 = (ELF_STACK_PAGES as u64) * 0x1000;
const ELF_STACK_VIRT: u64 = 0x0000_7FFF_F000_0000;
#[allow(dead_code)]
const ELF_STACK_TOP: u64  = ELF_STACK_VIRT + ELF_STACK_SIZE;

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

    assert!(code.len() <= 0x1000, "ring3 code blob exceeds one page");

    let pml4_phys = with_memory(|mem| {
        let code_phys = mem.alloc_dma_pages(1).expect("ring3: out of frames (code)");
        let dst = mem.phys_mem_offset() + code_phys.as_u64();
        unsafe {
            core::ptr::write_bytes(dst.as_mut_ptr::<u8>(), 0, 0x1000);
            core::ptr::copy_nonoverlapping(code.as_ptr(), dst.as_mut_ptr::<u8>(), code.len());
        }

        let stack_phys = mem.alloc_dma_pages(1).expect("ring3: out of frames (stack)");
        let stack_dst = mem.phys_mem_offset() + stack_phys.as_u64();
        unsafe { core::ptr::write_bytes(stack_dst.as_mut_ptr::<u8>(), 0, 0x1000); }

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
pub fn spawn_process(elf_data: &[u8]) -> Result<ProcessId, &'static str> {
    use libkernel::elf::{self, PF_W, PF_X};
    use libkernel::process::Process;

    // Free kernel stacks of previously exited processes so the heap doesn't run out.
    libkernel::process::reap_zombies();

    let info = elf::parse(elf_data).map_err(|e| {
        log::error!("ELF parse error: {:?} (data len={}, first 4 bytes={:02x?})",
            e, elf_data.len(), &elf_data[..elf_data.len().min(4)]);
        "invalid ELF binary"
    })?;

    if info.segments.is_empty() {
        return Err("ELF has no loadable segments");
    }

    let (pml4_phys, stack_kernel_base) = with_memory(|mem| {
        let pml4_phys = mem.create_user_page_table();
        let phys_off = mem.phys_mem_offset();

        // Map each PT_LOAD segment.
        for seg in &info.segments {
            // Calculate the page range needed.
            let page_start = seg.vaddr & !0xFFF;
            let page_end = (seg.vaddr + seg.memsz + 0xFFF) & !0xFFF;
            let num_pages = ((page_end - page_start) / 0x1000) as usize;

            for p in 0..num_pages {
                let page_vaddr = page_start + (p as u64) * 0x1000;
                let frame_phys = mem.alloc_dma_pages(1)
                    .expect("spawn_process: out of frames");

                // Zero the frame.
                let dst_base = phys_off + frame_phys.as_u64();
                unsafe {
                    core::ptr::write_bytes(dst_base.as_mut_ptr::<u8>(), 0, 0x1000);
                }

                // Copy file data into this page if it overlaps with filesz.
                let page_off_in_seg = page_vaddr.wrapping_sub(seg.vaddr);
                // The segment might not start at a page boundary.
                let copy_start_in_page = if page_vaddr < seg.vaddr {
                    (seg.vaddr - page_vaddr) as usize
                } else {
                    0
                };
                let seg_offset_for_page = if page_vaddr >= seg.vaddr {
                    page_off_in_seg
                } else {
                    0
                };

                if seg_offset_for_page < seg.filesz {
                    let avail = (seg.filesz - seg_offset_for_page) as usize;
                    let room = 0x1000 - copy_start_in_page;
                    let count = avail.min(room);
                    let src = &elf_data[(seg.offset + seg_offset_for_page) as usize..][..count];
                    unsafe {
                        let dst = (dst_base + copy_start_in_page as u64).as_mut_ptr::<u8>();
                        core::ptr::copy_nonoverlapping(src.as_ptr(), dst, count);
                    }
                }

                // Build page flags.
                let mut flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
                if seg.flags & PF_W != 0 {
                    flags |= PageTableFlags::WRITABLE;
                }
                if seg.flags & PF_X == 0 {
                    flags |= PageTableFlags::NO_EXECUTE;
                }

                mem.map_user_page(
                    pml4_phys,
                    VirtAddr::new(page_vaddr),
                    frame_phys,
                    flags,
                ).expect("spawn_process: failed to map segment page");
            }
        }

        // Map 8-page user stack (RW, NX) using contiguous physical pages
        // so we can write the initial auxv layout through the phys_mem_offset window.
        let stack_phys = mem.alloc_dma_pages(ELF_STACK_PAGES)
            .expect("spawn_process: out of frames (stack)");
        let stack_kernel_base = phys_off + stack_phys.as_u64();
        unsafe {
            core::ptr::write_bytes(
                stack_kernel_base.as_mut_ptr::<u8>(), 0,
                ELF_STACK_SIZE as usize,
            );
        }

        let stack_flags = PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::USER_ACCESSIBLE
            | PageTableFlags::NO_EXECUTE;
        for i in 0..ELF_STACK_PAGES {
            let page_phys = x86_64::PhysAddr::new(stack_phys.as_u64() + (i as u64) * 0x1000);
            let page_virt = VirtAddr::new(ELF_STACK_VIRT + (i as u64) * 0x1000);
            mem.map_user_page(pml4_phys, page_virt, page_phys, stack_flags)
                .expect("spawn_process: failed to map stack page");
        }

        (pml4_phys, stack_kernel_base)
    });

    // Compute brk_base: page-aligned end of highest PT_LOAD segment.
    let brk_base = {
        let max_end = info.segments.iter()
            .map(|s| s.vaddr + s.memsz)
            .max()
            .unwrap_or(0);
        (max_end + 0xFFF) & !0xFFF
    };

    // Build the initial user stack: argc/argv/envp/auxv.
    // We write through the kernel's physical memory window (stack_kernel_base).
    let user_rsp = build_initial_stack(
        stack_kernel_base,
        ELF_STACK_VIRT,
        ELF_STACK_SIZE,
        &info,
    );

    // Create the process and insert it into the process table.
    let proc = Process::new(pml4_phys, info.entry, user_rsp, brk_base);
    let pid = proc.pid;
    libkernel::process::insert(proc);

    // Spawn a scheduler thread and record the thread index on the process.
    let thread_idx = libkernel::task::scheduler::spawn_user_thread(pid, pml4_phys);
    libkernel::process::with_process(pid, |p| {
        p.thread_idx = Some(thread_idx);
    });

    log::info!("spawn_process: pid={} entry={:#x} pml4={:#x}",
        pid.as_u64(), info.entry, pml4_phys.as_u64());

    Ok(pid)
}

// ---------------------------------------------------------------------------
// Initial stack builder for ELF processes

/// Build the initial user stack layout that musl expects:
///
/// ```text
/// [stack_top]
///   16 bytes of zeros (AT_RANDOM target)
///   AT_NULL (0, 0)
///   AT_RANDOM (25, addr_of_random_bytes)
///   AT_ENTRY (9, entry_point)
///   AT_PHNUM (5, phnum)
///   AT_PHENT (4, phentsize)
///   AT_PHDR (3, phdr_vaddr)
///   AT_PAGESZ (6, 4096)
///   NULL                    <- envp terminator
///   NULL                    <- argv terminator
///   0u64                    <- argc = 0
/// [RSP points here, 16-byte aligned]
/// ```
///
/// `kernel_base` is the kernel-accessible (phys_mem_offset) virtual address of the
/// stack's physical memory. `user_virt_base` is where the stack is mapped in user space.
/// Returns the user-space RSP value.
fn build_initial_stack(
    kernel_base: VirtAddr,
    user_virt_base: u64,
    stack_size: u64,
    info: &libkernel::elf::ElfInfo,
) -> u64 {
    // We build the stack from the top down.
    let kernel_top = kernel_base.as_u64() + stack_size;
    let user_top = user_virt_base + stack_size;

    // Helper: write a u64 at a given offset from the top.
    let mut cursor = kernel_top;

    let push = |cursor: &mut u64, val: u64| {
        *cursor -= 8;
        unsafe { *(*cursor as *mut u64) = val; }
    };

    // 1. AT_RANDOM data: 16 bytes of "random" data at the top.
    //    (We use a simple counter-based pattern; real randomness not needed for boot.)
    cursor -= 16;
    let random_kernel_addr = cursor;
    let random_user_addr = user_top - 16;
    unsafe {
        let p = random_kernel_addr as *mut u8;
        for i in 0..16u8 {
            *p.add(i as usize) = i.wrapping_mul(7).wrapping_add(0x42);
        }
    }

    // 2. Auxiliary vector (pairs of u64: type, value), terminated by AT_NULL.
    const AT_NULL: u64 = 0;
    const AT_PHDR: u64 = 3;
    const AT_PHENT: u64 = 4;
    const AT_PHNUM: u64 = 5;
    const AT_PAGESZ: u64 = 6;
    const AT_ENTRY: u64 = 9;
    const AT_UID: u64 = 11;
    const AT_RANDOM: u64 = 25;

    // Push in reverse order (AT_NULL last pushed = lowest address = first read).
    // 8 auxv entries (16 u64s) + 3 u64s (envp/argv/argc) = 19 pushes.
    // Plus 16 bytes random data = 168 bytes from top. 168 % 16 = 8, so add
    // a padding word to keep RSP 16-byte aligned (20 pushes = 160, +16 = 176).
    push(&mut cursor, 0); // alignment padding
    push(&mut cursor, 0); push(&mut cursor, AT_NULL);
    push(&mut cursor, random_user_addr); push(&mut cursor, AT_RANDOM);
    push(&mut cursor, info.entry); push(&mut cursor, AT_ENTRY);
    push(&mut cursor, info.phnum as u64); push(&mut cursor, AT_PHNUM);
    push(&mut cursor, info.phentsize as u64); push(&mut cursor, AT_PHENT);
    push(&mut cursor, info.phdr_vaddr); push(&mut cursor, AT_PHDR);
    push(&mut cursor, 4096); push(&mut cursor, AT_PAGESZ);
    push(&mut cursor, 0); push(&mut cursor, AT_UID);  // AT_UID = 0 (root)

    // 3. envp: NULL terminator (no environment variables).
    push(&mut cursor, 0);

    // 4. argv: NULL terminator (no arguments).
    push(&mut cursor, 0);

    // 5. argc = 0
    push(&mut cursor, 0);

    // Compute user RSP: same offset from user_top as cursor from kernel_top.
    let offset_from_top = kernel_top - cursor;
    let user_rsp = user_top - offset_from_top;
    debug_assert!(user_rsp % 16 == 0, "user RSP must be 16-byte aligned");

    user_rsp
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
