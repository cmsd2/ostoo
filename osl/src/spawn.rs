//! ELF process spawning with argv and parent PID support.

use libkernel::consts::{PAGE_SIZE, PAGE_MASK};
use libkernel::memory::with_memory;
use libkernel::process::{Process, ProcessId};
use x86_64::structures::paging::PageTableFlags;
use x86_64::VirtAddr;

/// 8-page (32 KiB) user stack for ELF processes, placed at a high user address.
const ELF_STACK_PAGES: usize = 8;
const ELF_STACK_SIZE: u64 = (ELF_STACK_PAGES as u64) * PAGE_SIZE;
const ELF_STACK_VIRT: u64 = 0x0000_7FFF_F000_0000;

/// Spawn with argv and explicit parent.
/// Used by the spawn syscall.
pub fn spawn_process_full(
    elf_data: &[u8],
    argv: &[&[u8]],
    parent_pid: ProcessId,
) -> Result<ProcessId, &'static str> {
    use libkernel::elf::{self, PF_W, PF_X};

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
            let page_start = seg.vaddr & !PAGE_MASK;
            let page_end = (seg.vaddr + seg.memsz + PAGE_MASK) & !PAGE_MASK;
            let num_pages = ((page_end - page_start) / PAGE_SIZE) as usize;

            for p in 0..num_pages {
                let page_vaddr = page_start + (p as u64) * PAGE_SIZE;
                let frame_phys = mem.alloc_dma_pages(1)
                    .expect("spawn_process: out of frames");

                let dst_base = phys_off + frame_phys.as_u64();
                unsafe {
                    libkernel::consts::clear_page(dst_base.as_mut_ptr::<u8>());
                }

                let page_off_in_seg = page_vaddr.wrapping_sub(seg.vaddr);
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
                    let room = PAGE_SIZE as usize - copy_start_in_page;
                    let count = avail.min(room);
                    let src = &elf_data[(seg.offset + seg_offset_for_page) as usize..][..count];
                    unsafe {
                        let dst = (dst_base + copy_start_in_page as u64).as_mut_ptr::<u8>();
                        core::ptr::copy_nonoverlapping(src.as_ptr(), dst, count);
                    }
                }

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

        // Map 8-page user stack (RW, NX) using contiguous physical pages.
        let stack_phys = mem.alloc_dma_pages(ELF_STACK_PAGES)
            .expect("spawn_process: out of frames (stack)");
        let stack_kernel_base = phys_off + stack_phys.as_u64();
        unsafe {
            core::ptr::write_bytes(
                stack_kernel_base.as_mut_ptr::<u8>(), 0,
                ELF_STACK_SIZE as usize,
            );
        }

        let stack_flags = crate::dispatch::USER_DATA_FLAGS;
        for i in 0..ELF_STACK_PAGES {
            let page_phys = x86_64::PhysAddr::new(stack_phys.as_u64() + (i as u64) * PAGE_SIZE);
            let page_virt = VirtAddr::new(ELF_STACK_VIRT + (i as u64) * PAGE_SIZE);
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
        (max_end + PAGE_MASK) & !PAGE_MASK
    };

    // Build the initial user stack: argc/argv/envp/auxv.
    let user_rsp = build_initial_stack(
        stack_kernel_base,
        ELF_STACK_VIRT,
        ELF_STACK_SIZE,
        &info,
        argv,
    );

    // Create the process and insert it into the process table.
    let mut proc = Process::new(pml4_phys, info.entry, user_rsp, brk_base);
    proc.parent_pid = parent_pid;
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
///   argv string data (null-terminated strings)
///   16 bytes of zeros (AT_RANDOM target)
///   auxv pairs (AT_NULL terminator)
///   NULL                    <- envp terminator
///   argv[argc-1] ptr
///   ...
///   argv[0] ptr
///   NULL                    <- argv terminator
///   argc
/// [RSP points here, 16-byte aligned]
/// ```
fn build_initial_stack(
    kernel_base: VirtAddr,
    user_virt_base: u64,
    stack_size: u64,
    info: &libkernel::elf::ElfInfo,
    argv: &[&[u8]],
) -> u64 {
    let kernel_top = kernel_base.as_u64() + stack_size;
    let user_top = user_virt_base + stack_size;

    let mut cursor = kernel_top;

    let push = |cursor: &mut u64, val: u64| {
        *cursor -= 8;
        unsafe { *(*cursor as *mut u64) = val; }
    };

    let k2u = |kaddr: u64| -> u64 {
        user_top - (kernel_top - kaddr)
    };

    // 1. Write argv string data at the top of the stack.
    let mut argv_user_addrs: alloc::vec::Vec<u64> = alloc::vec::Vec::new();
    for arg in argv {
        let len = arg.len() + 1; // +1 for null terminator
        cursor -= len as u64;
        let str_user_addr = k2u(cursor);
        unsafe {
            let p = cursor as *mut u8;
            core::ptr::copy_nonoverlapping(arg.as_ptr(), p, arg.len());
            *p.add(arg.len()) = 0; // null-terminate
        }
        argv_user_addrs.push(str_user_addr);
    }

    // 2. AT_RANDOM data: 16 bytes of "random" data.
    cursor -= 16;
    let random_user_addr = k2u(cursor);
    unsafe {
        let p = cursor as *mut u8;
        for i in 0..16u8 {
            *p.add(i as usize) = i.wrapping_mul(7).wrapping_add(0x42);
        }
    }

    // Align cursor to 8 bytes.
    cursor &= !7;

    // 3. Auxiliary vector.
    const AT_NULL: u64 = 0;
    const AT_PHDR: u64 = 3;
    const AT_PHENT: u64 = 4;
    const AT_PHNUM: u64 = 5;
    const AT_PAGESZ: u64 = 6;
    const AT_ENTRY: u64 = 9;
    const AT_UID: u64 = 11;
    const AT_RANDOM: u64 = 25;

    push(&mut cursor, 0); push(&mut cursor, AT_NULL);
    push(&mut cursor, random_user_addr); push(&mut cursor, AT_RANDOM);
    push(&mut cursor, info.entry); push(&mut cursor, AT_ENTRY);
    push(&mut cursor, info.phnum as u64); push(&mut cursor, AT_PHNUM);
    push(&mut cursor, info.phentsize as u64); push(&mut cursor, AT_PHENT);
    push(&mut cursor, info.phdr_vaddr); push(&mut cursor, AT_PHDR);
    push(&mut cursor, PAGE_SIZE); push(&mut cursor, AT_PAGESZ);
    push(&mut cursor, 0); push(&mut cursor, AT_UID);

    // 4. envp: NULL terminator (no environment variables).
    push(&mut cursor, 0);

    // 5. argv pointers: NULL terminator, then pointers in reverse order.
    push(&mut cursor, 0); // argv NULL terminator
    for addr in argv_user_addrs.iter().rev() {
        push(&mut cursor, *addr);
    }

    // 6. Alignment padding + argc.
    // After pushing argc, RSP must be 16-byte aligned. Compute prospective
    // user_rsp and insert padding BEFORE argc if needed, so the stack is:
    //   RSP → argc, argv[0], argv[1], …, NULL, envp…, NULL, auxv…
    let prospective_offset = kernel_top - cursor + 8; // +8 for the argc push
    let prospective_rsp = user_top - prospective_offset;
    if prospective_rsp % 16 != 0 {
        push(&mut cursor, 0); // alignment padding (below argc)
    }
    push(&mut cursor, argv.len() as u64); // argc

    let offset_from_top = kernel_top - cursor;
    let user_rsp = user_top - offset_from_top;

    debug_assert!(user_rsp % 16 == 0, "user RSP must be 16-byte aligned, got {:#x}", user_rsp);

    user_rsp
}
