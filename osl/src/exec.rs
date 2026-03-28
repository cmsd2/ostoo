//! execve(2) syscall implementation.

use alloc::string::String;
use alloc::vec::Vec;

use crate::errno;
use crate::dispatch::{read_user_string, resolve_user_path, vfs_read_file, USER_DATA_FLAGS};
use libkernel::consts::{PAGE_SIZE, PAGE_MASK};
use libkernel::elf::{self, PF_W, PF_X};
use libkernel::memory::with_memory;
use libkernel::process;
use libkernel::task::scheduler;
use x86_64::structures::paging::PageTableFlags;
use x86_64::VirtAddr;

/// 8-page (32 KiB) user stack, same as spawn.rs.
const ELF_STACK_PAGES: usize = 8;
const ELF_STACK_SIZE: u64 = (ELF_STACK_PAGES as u64) * PAGE_SIZE;
const ELF_STACK_VIRT: u64 = 0x0000_7FFF_F000_0000;

pub fn sys_execve(path_ptr: u64, argv_ptr: u64, envp_ptr: u64) -> i64 {
    // 1. Copy all arguments from userspace before we destroy the address space.
    let path = match read_user_string(path_ptr, 4096) {
        Some(p) => p,
        None => return -errno::EFAULT,
    };

    let argv = match read_string_array(argv_ptr) {
        Ok(v) => v,
        Err(e) => return e,
    };

    let envp = match read_string_array(envp_ptr) {
        Ok(v) => v,
        Err(e) => return e,
    };

    let resolved = resolve_user_path(&path);

    // 2. Read ELF from VFS.
    let pid = libkernel::process::current_pid();
    let elf_data = match vfs_read_file(&resolved, pid) {
        Ok(data) => data,
        Err(_) => return -errno::ENOENT,
    };

    // 3. Parse ELF.
    let info = match elf::parse(&elf_data) {
        Ok(info) => info,
        Err(_) => return -errno::ENOEXEC,
    };

    if info.segments.is_empty() {
        return -errno::ENOEXEC;
    }

    // 4a. Save old address space info before creating new one.
    let (old_pml4_phys, old_pml4_shared) = process::with_process_ref(pid, |p| {
        (p.pml4_phys, p.pml4_shared)
    }).unwrap_or((x86_64::PhysAddr::new(0), false));

    // 4. Create fresh PML4 and map segments + stack.
    let (new_pml4_phys, stack_kernel_base) = with_memory(|mem| {
        let pml4_phys = mem.create_user_page_table();
        let phys_off = mem.phys_mem_offset();

        for seg in &info.segments {
            let page_start = seg.vaddr & !PAGE_MASK;
            let page_end = (seg.vaddr + seg.memsz + PAGE_MASK) & !PAGE_MASK;
            let num_pages = ((page_end - page_start) / PAGE_SIZE) as usize;

            for p in 0..num_pages {
                let page_vaddr = page_start + (p as u64) * PAGE_SIZE;
                let frame_phys = mem.alloc_dma_pages(1)
                    .expect("execve: out of frames");

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
                ).expect("execve: failed to map segment page");
            }
        }

        // Map user stack.
        let stack_phys = mem.alloc_dma_pages(ELF_STACK_PAGES)
            .expect("execve: out of frames (stack)");
        let stack_kernel_base = phys_off + stack_phys.as_u64();
        unsafe {
            core::ptr::write_bytes(
                stack_kernel_base.as_mut_ptr::<u8>(), 0,
                ELF_STACK_SIZE as usize,
            );
        }

        let stack_flags = USER_DATA_FLAGS;
        for i in 0..ELF_STACK_PAGES {
            let page_phys = x86_64::PhysAddr::new(stack_phys.as_u64() + (i as u64) * PAGE_SIZE);
            let page_virt = VirtAddr::new(ELF_STACK_VIRT + (i as u64) * PAGE_SIZE);
            mem.map_user_page(pml4_phys, page_virt, page_phys, stack_flags)
                .expect("execve: failed to map stack page");
        }

        (pml4_phys, stack_kernel_base)
    });

    // 5. Compute brk_base.
    let brk_base = {
        let max_end = info.segments.iter()
            .map(|s| s.vaddr + s.memsz)
            .max()
            .unwrap_or(0);
        (max_end + PAGE_MASK) & !PAGE_MASK
    };

    // 6. Build initial stack with argv, envp, auxv.
    let argv_slices: Vec<Vec<u8>> = argv.iter().map(|s| s.as_bytes().to_vec()).collect();
    let envp_slices: Vec<Vec<u8>> = envp.iter().map(|s| s.as_bytes().to_vec()).collect();
    let argv_refs: Vec<&[u8]> = argv_slices.iter().map(|v| v.as_slice()).collect();
    let envp_refs: Vec<&[u8]> = envp_slices.iter().map(|v| v.as_slice()).collect();

    let user_rsp = crate::spawn::build_initial_stack(
        stack_kernel_base,
        ELF_STACK_VIRT,
        ELF_STACK_SIZE,
        &info,
        &argv_refs,
        &envp_refs,
    );

    // 7. Update Process.
    let pid = process::current_pid();
    let vfork_parent_thread = process::with_process(pid, |p| {
        p.pml4_phys = new_pml4_phys;
        p.entry_point = info.entry;
        p.user_stack_top = user_rsp;
        p.brk_base = brk_base;
        p.brk_current = brk_base;
        p.vma_map.clear();
        p.pml4_shared = false;
        p.close_cloexec_fds();
        p.vfork_parent_thread.take()
    });

    // 8. Switch to new address space and free old one.
    scheduler::set_current_cr3(new_pml4_phys.as_u64());
    unsafe { libkernel::memory::switch_address_space(new_pml4_phys); }

    // Free old address space (CR3 already points to new PML4, so flush_tlb=false).
    // Skip if the old PML4 was shared with the parent (CLONE_VM/vfork).
    if !old_pml4_shared && old_pml4_phys.as_u64() != 0 {
        with_memory(|mem| {
            mem.cleanup_user_address_space(old_pml4_phys, false);
        });
    }

    // 9. If vfork child, unblock parent.
    if let Some(Some(thread_idx)) = vfork_parent_thread {
        scheduler::unblock(thread_idx);
    }

    libkernel::serial_println!("[execve] pid={} path={} entry={:#x} rsp={:#x}",
        pid.as_u64(), resolved, info.entry, user_rsp);

    // 10. Jump to new userspace — never returns.
    let user_cs = libkernel::gdt::user_code_selector().0 as u64;
    let user_ss = libkernel::gdt::user_data_selector().0 as u64;
    let per_cpu = libkernel::syscall::per_cpu_addr();
    let kernel_stack_top = process::with_process_ref(pid, |p| p.kernel_stack_top)
        .unwrap_or(0);

    // Reset FS_BASE (TLS) — the new program's libc will set it up.
    unsafe { libkernel::msr::write_fs_base(0); }

    // Set up TSS and PER_CPU for this process.
    libkernel::gdt::set_kernel_stack(VirtAddr::new(kernel_stack_top));
    libkernel::syscall::set_kernel_rsp(kernel_stack_top);

    unsafe {
        scheduler::jump_to_userspace(
            info.entry, user_rsp, new_pml4_phys.as_u64(),
            user_cs, user_ss, per_cpu, 0, 0,
        );
    }
}

/// Read a NULL-terminated array of char* pointers from userspace.
fn read_string_array(ptr: u64) -> Result<Vec<String>, i64> {
    let mut result = Vec::new();
    if ptr == 0 {
        return Ok(result);
    }
    const USER_LIMIT: u64 = 0x0000_8000_0000_0000;
    let mut i = 0usize;
    loop {
        let entry_addr = ptr + (i * 8) as u64;
        if entry_addr + 8 > USER_LIMIT {
            return Err(-errno::EFAULT);
        }
        let str_ptr = unsafe { *(entry_addr as *const u64) };
        if str_ptr == 0 {
            break; // NULL terminator
        }
        match read_user_string(str_ptr, 4096) {
            Some(s) => result.push(s),
            None => return Err(-errno::EFAULT),
        }
        i += 1;
        if i > 256 {
            return Err(-errno::EINVAL); // safety limit
        }
    }
    Ok(result)
}
