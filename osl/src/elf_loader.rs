//! Shared ELF segment and stack mapping for process creation.
//!
//! Used by both `spawn::spawn_process_full` and `exec::sys_execve` to avoid
//! duplicating the page-allocation / segment-copy / stack-mapping loops.

use libkernel::consts::{PAGE_SIZE, PAGE_MASK};
use libkernel::elf::{ElfInfo, PF_W, PF_X};
use libkernel::memory::with_memory;
use x86_64::structures::paging::PageTableFlags;
use x86_64::{PhysAddr, VirtAddr};

/// 8-page (32 KiB) user stack for ELF processes.
pub const ELF_STACK_PAGES: usize = 8;
pub const ELF_STACK_SIZE: u64 = (ELF_STACK_PAGES as u64) * PAGE_SIZE;
/// Virtual address where the user stack is placed.
pub const ELF_STACK_VIRT: u64 = 0x0000_7FFF_F000_0000;

/// Standard user-data page flags: present, writable, user-accessible, no-execute.
pub const USER_DATA_FLAGS: PageTableFlags = PageTableFlags::PRESENT
    .union(PageTableFlags::WRITABLE)
    .union(PageTableFlags::USER_ACCESSIBLE)
    .union(PageTableFlags::NO_EXECUTE);

/// Create a fresh user PML4, map all ELF PT_LOAD segments and a user stack.
///
/// Returns `(pml4_phys, stack_kernel_base)` where `stack_kernel_base` is the
/// kernel-virtual address of the stack memory (for writing argv/envp/auxv).
pub fn load_elf_address_space(
    elf_data: &[u8],
    info: &ElfInfo,
) -> Result<(PhysAddr, VirtAddr), &'static str> {
    Ok(with_memory(|mem| {
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
                    .expect("load_elf: out of frames");

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
                ).expect("load_elf: failed to map segment page");
            }
        }

        // Map user stack (RW, NX).
        let stack_phys = mem.alloc_dma_pages(ELF_STACK_PAGES)
            .expect("load_elf: out of frames (stack)");
        let stack_kernel_base = phys_off + stack_phys.as_u64();
        unsafe {
            core::ptr::write_bytes(
                stack_kernel_base.as_mut_ptr::<u8>(), 0,
                ELF_STACK_SIZE as usize,
            );
        }

        for i in 0..ELF_STACK_PAGES {
            let page_phys = PhysAddr::new(stack_phys.as_u64() + (i as u64) * PAGE_SIZE);
            let page_virt = VirtAddr::new(ELF_STACK_VIRT + (i as u64) * PAGE_SIZE);
            mem.map_user_page(pml4_phys, page_virt, page_phys, USER_DATA_FLAGS)
                .expect("load_elf: failed to map stack page");
        }

        (pml4_phys, stack_kernel_base)
    }))
}

/// Compute brk_base: page-aligned end of the highest PT_LOAD segment.
pub fn compute_brk_base(info: &ElfInfo) -> u64 {
    let max_end = info.segments.iter()
        .map(|s| s.vaddr + s.memsz)
        .max()
        .unwrap_or(0);
    (max_end + PAGE_MASK) & !PAGE_MASK
}
