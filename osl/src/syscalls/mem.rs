//! Memory management syscalls: mmap, munmap, mprotect, brk.

use x86_64::structures::paging::PageTableFlags;

use crate::errno;
use crate::elf_loader::USER_DATA_FLAGS;
use libkernel::consts::{PAGE_SIZE, PAGE_MASK};
use libkernel::process;

pub(crate) fn sys_brk(addr: u64) -> i64 {
    use libkernel::memory::with_memory;

    let pid = process::current_pid();
    if pid == process::ProcessId::KERNEL {
        return 0;
    }

    let (brk_base, brk_current, pml4_phys) = match process::with_process_ref(pid, |p| {
        (p.brk_base, p.brk_current, p.pml4_phys)
    }) {
        Some(v) => v,
        None => return 0,
    };

    if addr == 0 || addr < brk_base {
        return brk_current as i64;
    }

    let new_brk = (addr + PAGE_MASK) & !PAGE_MASK;
    if new_brk <= brk_current {
        process::with_process(pid, |p| p.brk_current = new_brk);
        return new_brk as i64;
    }

    let pages_needed = ((new_brk - brk_current) / PAGE_SIZE) as usize;
    let ok = with_memory(|mem| {
        mem.alloc_and_map_user_pages(pages_needed, brk_current, pml4_phys, USER_DATA_FLAGS)
            .is_ok()
    });

    if ok {
        process::with_process(pid, |p| p.brk_current = new_brk);
        new_brk as i64
    } else {
        brk_current as i64
    }
}

pub(crate) fn sys_mmap(addr: u64, length: u64, prot: u64, flags: u64, a5: u64) -> i64 {
    use libkernel::process::{Vma, MAP_ANONYMOUS, MAP_FIXED};
    use libkernel::memory::with_memory;

    let pid = process::current_pid();
    if pid == process::ProcessId::KERNEL {
        return -errno::ENOMEM;
    }

    let aligned_len = (length + PAGE_MASK) & !PAGE_MASK;
    if aligned_len == 0 {
        return -errno::EINVAL;
    }
    let num_pages = (aligned_len / PAGE_SIZE) as usize;

    let anonymous = flags & MAP_ANONYMOUS as u64 != 0;
    let fixed = flags & MAP_FIXED as u64 != 0;

    // For file-backed mappings: extract fd, offset, and grab file content.
    let file_info: Option<(i32, u64, alloc::vec::Vec<u8>)> = if !anonymous {
        let fd = a5 as i32;
        let offset = libkernel::syscall::get_user_r9();
        if offset & PAGE_MASK != 0 {
            return -errno::EINVAL;
        }

        // Get the file content from the fd's buffer.
        let content = match process::with_process_ref(pid, |p| {
            let obj = p.get_fd(fd as usize).ok()?;
            let handle = obj.as_file()?.clone();
            Some(handle)
        }) {
            Some(Some(handle)) => {
                match handle.content_bytes() {
                    Some(bytes) => alloc::vec::Vec::from(bytes),
                    None => return -errno::ENODEV,
                }
            }
            _ => return -errno::EBADF,
        };

        Some((fd, offset, content))
    } else {
        None
    };

    let (vma_fd, vma_offset) = match &file_info {
        Some((fd, offset, _)) => (Some(*fd as usize), *offset),
        None => (None, 0),
    };

    if fixed {
        // MAP_FIXED: addr must be page-aligned and non-zero.
        if addr == 0 || addr & PAGE_MASK != 0 {
            return -errno::EINVAL;
        }

        let pml4_phys = match process::with_process_ref(pid, |p| p.pml4_phys) {
            Some(v) => v,
            None => return -errno::ENOMEM,
        };

        // Implicit munmap: remove any overlapping VMAs and free their pages.
        let pages_to_free = process::with_process(pid, |p| {
            p.munmap_vmas(addr, aligned_len)
        }).unwrap_or_default();

        if !pages_to_free.is_empty() {
            with_memory(|mem| {
                for (base, count) in &pages_to_free {
                    for i in 0..*count {
                        let vaddr = x86_64::VirtAddr::new(base + (i as u64) * PAGE_SIZE);
                        mem.unmap_and_free_user_page(pml4_phys, vaddr, true);
                    }
                }
            });
        }

        let vma = Vma {
            start: addr,
            len: aligned_len,
            prot: prot as u32,
            flags: flags as u32,
            fd: vma_fd,
            offset: vma_offset,
        };
        let pt_flags = vma.page_table_flags();

        let ok = mmap_alloc_pages(num_pages, addr, pml4_phys, pt_flags, &file_info);
        if ok {
            process::with_process(pid, |p| {
                p.vma_map.insert(addr, vma);
            });
            addr as i64
        } else {
            -errno::ENOMEM
        }
    } else {
        // Non-fixed: find a gap using the top-down gap finder.
        let (region_base, pml4_phys) = match process::with_process(pid, |p| {
            p.find_mmap_gap(aligned_len).map(|base| (base, p.pml4_phys))
        }) {
            Some(Some(v)) => v,
            _ => return -errno::ENOMEM,
        };

        let vma = Vma {
            start: region_base,
            len: aligned_len,
            prot: prot as u32,
            flags: flags as u32,
            fd: vma_fd,
            offset: vma_offset,
        };
        let pt_flags = vma.page_table_flags();

        let ok = mmap_alloc_pages(num_pages, region_base, pml4_phys, pt_flags, &file_info);
        if ok {
            process::with_process(pid, |p| {
                p.vma_map.insert(region_base, vma);
            });
            region_base as i64
        } else {
            -errno::ENOMEM
        }
    }
}

/// Allocate, zero (and optionally fill with file data), and map pages for mmap.
fn mmap_alloc_pages(
    num_pages: usize,
    vaddr_base: u64,
    pml4_phys: x86_64::PhysAddr,
    flags: PageTableFlags,
    file_info: &Option<(i32, u64, alloc::vec::Vec<u8>)>,
) -> bool {
    use libkernel::memory::with_memory;

    match file_info {
        None => {
            with_memory(|mem| {
                mem.alloc_and_map_user_pages(num_pages, vaddr_base, pml4_phys, flags)
                    .is_ok()
            })
        }
        Some((_fd, offset, content)) => {
            with_memory(|mem| {
                let phys_off = mem.phys_mem_offset();
                for i in 0..num_pages {
                    let page_vaddr = vaddr_base + (i as u64) * PAGE_SIZE;
                    let frame_phys = match mem.alloc_dma_pages(1) {
                        Some(f) => f,
                        None => return false,
                    };

                    let dst_base = phys_off + frame_phys.as_u64();
                    unsafe {
                        libkernel::consts::clear_page(dst_base.as_mut_ptr::<u8>());
                    }

                    let file_offset = *offset + (i as u64) * PAGE_SIZE;
                    if (file_offset as usize) < content.len() {
                        let src_start = file_offset as usize;
                        let src_end = content.len().min(src_start + PAGE_SIZE as usize);
                        let count = src_end - src_start;
                        unsafe {
                            let dst = dst_base.as_mut_ptr::<u8>();
                            core::ptr::copy_nonoverlapping(
                                content[src_start..src_end].as_ptr(),
                                dst,
                                count,
                            );
                        }
                    }

                    if mem.map_user_page(
                        pml4_phys,
                        x86_64::VirtAddr::new(page_vaddr),
                        frame_phys,
                        flags,
                    ).is_err() {
                        return false;
                    }
                }
                true
            })
        }
    }
}

pub(crate) fn sys_munmap(addr: u64, length: u64) -> i64 {
    use libkernel::memory::with_memory;

    if addr & PAGE_MASK != 0 || length == 0 {
        return -errno::EINVAL;
    }

    let pid = process::current_pid();
    if pid == process::ProcessId::KERNEL {
        return -errno::EINVAL;
    }

    let aligned_len = (length + PAGE_MASK) & !PAGE_MASK;

    let result = process::with_process(pid, |p| {
        (p.pml4_phys, p.munmap_vmas(addr, aligned_len))
    });
    let (pml4_phys, pages_to_free) = match result {
        Some(v) => v,
        None => return -errno::EINVAL,
    };

    if pages_to_free.is_empty() {
        return 0;
    }

    with_memory(|mem| {
        for (base, count) in &pages_to_free {
            for i in 0..*count {
                let vaddr = x86_64::VirtAddr::new(base + (i as u64) * PAGE_SIZE);
                mem.unmap_and_free_user_page(pml4_phys, vaddr, true);
            }
        }
    });

    0
}

pub(crate) fn sys_mprotect(addr: u64, length: u64, prot: u64) -> i64 {
    use libkernel::memory::with_memory;

    if addr & PAGE_MASK != 0 || length == 0 {
        return -errno::EINVAL;
    }

    let pid = process::current_pid();
    if pid == process::ProcessId::KERNEL {
        return -errno::EINVAL;
    }

    let aligned_len = (length + PAGE_MASK) & !PAGE_MASK;
    let prot32 = prot as u32;

    let result = process::with_process(pid, |p| {
        (p.pml4_phys, p.mprotect_vmas(addr, aligned_len, prot32))
    });
    let (pml4_phys, pages_to_update) = match result {
        Some(v) => v,
        None => return -errno::EINVAL,
    };

    if pages_to_update.is_empty() {
        return 0;
    }

    let flags = process::prot_to_page_flags(prot32);

    with_memory(|mem| {
        for (base, count) in &pages_to_update {
            for i in 0..*count {
                let vaddr = x86_64::VirtAddr::new(base + (i as u64) * PAGE_SIZE);
                mem.update_user_page_flags(pml4_phys, vaddr, flags, true);
            }
        }
    });

    0
}
