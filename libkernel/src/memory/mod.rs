use alloc::collections::BTreeMap;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;
use x86_64::{VirtAddr, PhysAddr};
use x86_64::structures::paging::{
    OffsetPageTable,
    Page,
    PageTable,
    PageTableFlags,
    PageTableIndex,
    PhysFrame,
    Mapper,
    Size4KiB,
    Size2MiB,
    FrameAllocator,
    RecursivePageTable,
    mapper::MapToError,
};
use bootloader::bootinfo::{MemoryMap, MemoryRegion};

pub mod frame_allocator;
pub mod vmem_allocator;

pub use frame_allocator::BootInfoFrameAllocator;
pub use vmem_allocator::VmemAllocator;
pub use vmem_allocator::DumbVmemAllocator;

// ---------------------------------------------------------------------------
// Constants

/// PML4 index used for the recursive self-mapping.
const RECURSIVE_INDEX: u16 = 511;

/// Virtual address of the PML4 when accessed through the recursive mapping.
/// With RECURSIVE_INDEX=511: P4=511, P3=511, P2=511, P1=511, offset=0.
const PML4_RECURSIVE_VIRT: u64 = 0xFFFF_FFFF_FFFF_F000;

/// Base virtual address for the kernel's physical-memory direct map.
/// PML4 entry 257 → 0xFFFF_8080_0000_0000 (512 GiB window).
pub const PHYS_MAP_BASE: u64 = 0xFFFF_8080_0000_0000;

/// Base of the high-half MMIO virtual window.
pub const MMIO_VIRT_BASE: u64 = 0xFFFF_8002_0000_0000;

/// Size of the MMIO virtual window (512 GiB).
const MMIO_VIRT_SIZE: u64 = 0x0000_0080_0000_0000;

// ---------------------------------------------------------------------------
// Global physical memory offset (set once during init, readable from anywhere)

static PHYS_MEM_OFFSET: AtomicU64 = AtomicU64::new(0);

/// Returns the virtual address at which all physical memory is linearly mapped.
/// Returns 0 if called before `init_services`.
pub fn phys_mem_offset() -> u64 {
    PHYS_MEM_OFFSET.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Global memory services

/// Combined mapper and frame allocator, accessible via [`with_memory`].
pub struct MemoryServices {
    mapper:          RecursivePageTable<'static>,
    frame_allocator: BootInfoFrameAllocator,
    phys_mem_offset: VirtAddr,
    memory_map:      &'static MemoryMap,
    /// Bump pointer for the MMIO virtual window.
    mmio_next:       u64,
    /// Cache: page-aligned physical base → virtual base of the mapped region.
    mmio_cache:      BTreeMap<u64, u64>,
}

impl MemoryServices {
    /// Map a single 4 KiB page at `page` to the physical address `addr`.
    pub fn map_page(&mut self, page: Page, addr: PhysAddr, flags: PageTableFlags) -> VirtAddr {
        map_page(page, addr, &mut self.mapper, &mut self.frame_allocator, flags)
    }

    /// Virtual address at which all physical memory is linearly mapped.
    pub fn phys_mem_offset(&self) -> VirtAddr {
        self.phys_mem_offset
    }

    /// Walk the active page tables and return the physical address that `virt`
    /// maps to, regardless of whether it is a 4 KiB, 2 MiB, or 1 GiB page.
    /// Returns `None` for unmapped or invalid addresses.
    pub fn translate_virt(&self, virt: VirtAddr) -> Option<PhysAddr> {
        use x86_64::structures::paging::mapper::{MappedFrame, TranslateResult, Translate};
        match self.mapper.translate(virt) {
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
    }

    /// Iterate over every region in the bootloader memory map.
    pub fn iter_memory_regions(&self) -> impl Iterator<Item = &MemoryRegion> {
        self.memory_map.iter()
    }

    /// Map a range of physical MMIO addresses into a fresh high-half virtual
    /// region and return the corresponding virtual address.
    ///
    /// A `BTreeMap` cache keyed on the page-aligned physical base ensures that
    /// calling this function twice for the same physical base always returns the
    /// same virtual address, regardless of the requested size.
    ///
    /// New pages are mapped with PRESENT | WRITABLE | NO_CACHE flags.
    pub fn map_mmio_region(&mut self, phys_start: PhysAddr, size: usize) -> VirtAddr {
        // Work in page-aligned units.
        let phys_base  = phys_start.as_u64() & !crate::consts::PAGE_MASK; // round down to page
        let page_off   = (phys_start.as_u64() - phys_base) as usize;
        let num_pages  = (size + page_off + crate::consts::PAGE_MASK as usize) / crate::consts::PAGE_SIZE as usize;

        // Return the cached virtual address if this physical region was already
        // mapped.  The cache key is the page-aligned physical base.
        if let Some(&virt_base) = self.mmio_cache.get(&phys_base) {
            return VirtAddr::new(virt_base + page_off as u64);
        }

        // Allocate virtual pages from the MMIO window.
        let virt_base = self.mmio_next;
        self.mmio_next += num_pages as u64 * crate::consts::PAGE_SIZE;
        assert!(
            self.mmio_next <= MMIO_VIRT_BASE + MMIO_VIRT_SIZE,
            "MMIO virtual window exhausted"
        );

        let flags = PageTableFlags::PRESENT
                  | PageTableFlags::WRITABLE
                  | PageTableFlags::NO_CACHE;

        for i in 0..num_pages as u64 {
            let page = Page::<Size4KiB>::from_start_address(
                VirtAddr::new(virt_base + i * crate::consts::PAGE_SIZE)
            ).expect("map_mmio_region: unaligned virt page");
            let phys  = PhysAddr::new(phys_base + i * crate::consts::PAGE_SIZE);
            let frame = PhysFrame::<Size4KiB>::containing_address(phys);
            unsafe {
                self.mapper
                    .map_to(page, frame, flags, &mut self.frame_allocator)
                    .expect("map_mmio_region: map_to failed")
                    .flush();
            }
        }

        self.mmio_cache.insert(phys_base, virt_base);
        VirtAddr::new(virt_base + page_off as u64)
    }

    /// Allocate `pages` physically-contiguous 4 KiB frames for DMA.
    ///
    /// Returns the base physical address, or `None` if frames are exhausted.
    /// For `pages > 1`, bypasses the free list to ensure contiguity.
    /// Panics if the allocated frames are not physically contiguous.
    pub fn alloc_dma_pages(&mut self, pages: usize) -> Option<PhysAddr> {
        let mut base: Option<PhysAddr> = None;
        for i in 0..pages {
            // For single-page allocs the free list is fine (trivially contiguous).
            // For multi-page allocs use the sequential allocator to guarantee contiguity.
            let frame = if pages > 1 {
                self.frame_allocator.allocate_frame_sequential()
            } else {
                self.frame_allocator.allocate_frame()
            }?;
            let paddr = frame.start_address();
            if i == 0 {
                base = Some(paddr);
            } else {
                let expected = PhysAddr::new(base.unwrap().as_u64() + (i as u64) * crate::consts::PAGE_SIZE);
                assert_eq!(paddr, expected, "alloc_dma_pages: non-contiguous frames");
            }
        }
        base
    }

    /// Allocate, zero, and map `count` user pages starting at `vaddr_base`.
    pub fn alloc_and_map_user_pages(
        &mut self,
        count: usize,
        vaddr_base: u64,
        pml4_phys: PhysAddr,
        flags: PageTableFlags,
    ) -> Result<(), ()> {
        let phys_off = self.phys_mem_offset;
        for i in 0..count {
            let vaddr = vaddr_base + (i as u64) * crate::consts::PAGE_SIZE;
            let frame = self.alloc_dma_pages(1).ok_or(())?;
            let dst = phys_off + frame.as_u64();
            unsafe { crate::consts::clear_page(dst.as_mut_ptr::<u8>()); }
            self.map_user_page(
                pml4_phys,
                VirtAddr::new(vaddr),
                frame,
                flags,
            ).map_err(|_| ())?;
        }
        Ok(())
    }

    /// `(frames_allocated, total_usable_frames, free_list_len)` since boot.
    pub fn frame_stats(&self) -> (usize, usize, usize) {
        (
            self.frame_allocator.frames_allocated(),
            self.frame_allocator.total_usable_frames(),
            self.frame_allocator.free_list_len(),
        )
    }

    // -----------------------------------------------------------------------
    // Per-process page table management

    /// Allocate a fresh PML4 for a new user process.
    ///
    /// The new table is initialised as follows:
    /// - Entries 0–255 zeroed (user-private lower half; populated by the ELF loader).
    /// - Entries 256–510 copied verbatim from the active PML4 so every process
    ///   shares the kernel's high-half mappings without `USER_ACCESSIBLE`.
    /// - Entry 511 set to a recursive self-mapping pointing at the new frame,
    ///   so the `RecursivePageTable` mechanism works after `switch_address_space`.
    ///
    /// Returns the physical address of the new PML4 frame (use as CR3 value).
    pub fn create_user_page_table(&mut self) -> PhysAddr {
        use x86_64::registers::control::Cr3;

        // Allocate and zero a fresh PML4 frame.
        let pml4_frame = self.frame_allocator
            .allocate_frame()
            .expect("create_user_page_table: out of frames");
        let pml4_phys = pml4_frame.start_address();
        let pml4_virt = self.phys_mem_offset + pml4_phys.as_u64();
        let new_pml4: &mut PageTable = unsafe { &mut *pml4_virt.as_mut_ptr() };
        new_pml4.zero();

        // Copy kernel-half entries (256–510) from the active PML4 so the new
        // address space inherits all kernel mappings.  Use raw u64 copies to
        // avoid touching the entry API (flags, frame, etc.).
        let (active_frame, _) = Cr3::read();
        let active_virt = self.phys_mem_offset + active_frame.start_address().as_u64();
        unsafe {
            let src = active_virt.as_ptr::<u64>();
            let dst = pml4_virt.as_mut_ptr::<u64>();
            for i in 256..511usize {
                dst.add(i).write(src.add(i).read());
            }
        }

        // Entry 511: recursive self-mapping — points at this PML4's own frame.
        new_pml4[PageTableIndex::new(511)].set_frame(
            pml4_frame,
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
        );

        pml4_phys
    }

    /// Map a single 4 KiB page into a *non-active* page table rooted at
    /// `pml4_phys`.
    ///
    /// Uses an `OffsetPageTable` view over the target PML4 so intermediate
    /// page-table frames are allocated automatically.  The flush token is
    /// discarded (`.ignore()`) because the target address space is not active;
    /// the TLB is implicitly clean after `switch_address_space`.
    pub fn map_user_page(
        &mut self,
        pml4_phys: PhysAddr,
        virt: VirtAddr,
        phys: PhysAddr,
        flags: PageTableFlags,
    ) -> Result<(), MapToError<Size4KiB>> {
        let pml4_virt = self.phys_mem_offset + pml4_phys.as_u64();
        let pml4: &mut PageTable = unsafe { &mut *pml4_virt.as_mut_ptr() };
        // Safety: phys_mem_offset correctly maps all physical memory, and
        // pml4_phys is a valid PML4 frame allocated by this MemoryServices.
        let mut table = unsafe { OffsetPageTable::new(pml4, self.phys_mem_offset) };
        let page  = Page::<Size4KiB>::containing_address(virt);
        let frame = PhysFrame::<Size4KiB>::containing_address(phys);
        unsafe {
            table.map_to(page, frame, flags, &mut self.frame_allocator)?.ignore();
        }
        Ok(())
    }

    /// Unmap a single 4 KiB page from a page table rooted at `pml4_phys`.
    ///
    /// Returns the physical frame that was mapped, or `None` if the page was
    /// not mapped. If `flush_tlb` is true the TLB entry is invalidated (use
    /// when the target PML4 is the active address space).
    pub fn unmap_user_page(
        &mut self,
        pml4_phys: PhysAddr,
        vaddr: VirtAddr,
        flush_tlb: bool,
    ) -> Option<PhysFrame> {
        let pml4_virt = self.phys_mem_offset + pml4_phys.as_u64();
        let pml4: &mut PageTable = unsafe { &mut *pml4_virt.as_mut_ptr() };
        let mut table = unsafe { OffsetPageTable::new(pml4, self.phys_mem_offset) };
        let page = Page::<Size4KiB>::containing_address(vaddr);
        match table.unmap(page) {
            Ok((frame, flush_token)) => {
                if flush_tlb {
                    flush_token.flush();
                } else {
                    flush_token.ignore();
                }
                Some(frame)
            }
            Err(_) => None,
        }
    }

    /// Unmap a single user page and return its frame to the free list.
    ///
    /// Returns `true` if a frame was freed, `false` if the page was not mapped.
    pub fn unmap_and_free_user_page(
        &mut self,
        pml4_phys: PhysAddr,
        vaddr: VirtAddr,
        flush_tlb: bool,
    ) -> bool {
        if let Some(frame) = self.unmap_user_page(pml4_phys, vaddr, flush_tlb) {
            self.frame_allocator.deallocate_frame(frame);
            true
        } else {
            false
        }
    }
}

// Safety: RecursivePageTable contains raw pointers (*mut PageTable) which are
// !Send by default.  Access is always serialised through the Mutex, and the
// lock is never held across interrupt boundaries (see with_memory docs).
unsafe impl Send for MemoryServices {}

static MEMORY: Mutex<Option<MemoryServices>> = Mutex::new(None);

/// Store the mapper and frame allocator in global state.
///
/// Call exactly once, after the heap is initialised and before any driver
/// needs to map memory.
pub fn init_services(
    mut mapper: RecursivePageTable<'static>,
    mut frame_allocator: BootInfoFrameAllocator,
    phys_mem_offset: VirtAddr,
    memory_map: &'static MemoryMap,
) {
    PHYS_MEM_OFFSET.store(phys_mem_offset.as_u64(), Ordering::Relaxed);

    // Copy the MemoryMap from its bootloader-provided location (which may be
    // in the lower-half virtual address space) onto the kernel heap (high-half).
    // This ensures the frame allocator works from any address space, including
    // user process page tables that don't map the bootloader's lower half.
    let heap_map: &'static MemoryMap = unsafe {
        let layout = core::alloc::Layout::new::<MemoryMap>();
        let ptr = alloc::alloc::alloc(layout) as *mut MemoryMap;
        assert!(!ptr.is_null(), "init_services: failed to allocate MemoryMap on heap");
        core::ptr::copy_nonoverlapping(memory_map as *const MemoryMap, ptr, 1);
        &*ptr
    };
    frame_allocator.set_memory_map(heap_map);

    // -----------------------------------------------------------------------
    // Remap all physical memory into the high half using 2 MiB huge pages.
    //
    // The bootloader placed the physical memory map in the lower half, which
    // is zeroed in user page tables.  By remapping at PHYS_MAP_BASE (PML4
    // entry 257) the mapping is automatically inherited by every user address
    // space via create_user_page_table's entry 256–510 copy.
    // -----------------------------------------------------------------------

    let max_phys: u64 = heap_map.iter()
        .map(|r| r.range.end_addr())
        .max()
        .unwrap_or(0);
    let two_mib = 2u64 * 1024 * 1024;
    let max_phys_aligned = (max_phys + two_mib - 1) & !(two_mib - 1);

    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
    for phys_addr in (0..max_phys_aligned).step_by(two_mib as usize) {
        let page = Page::<Size2MiB>::containing_address(
            VirtAddr::new(PHYS_MAP_BASE + phys_addr),
        );
        let frame = PhysFrame::<Size2MiB>::containing_address(
            PhysAddr::new(phys_addr),
        );
        unsafe {
            mapper
                .map_to(page, frame, flags, &mut frame_allocator)
                .expect("remap_physical: map_to failed")
                .ignore(); // flush all at the end
        }
    }
    x86_64::instructions::tlb::flush_all();

    let new_offset = VirtAddr::new(PHYS_MAP_BASE);
    PHYS_MEM_OFFSET.store(new_offset.as_u64(), Ordering::Relaxed);

    log::info!(
        "memory::init_services: bootloader phys_mem_offset={:#x} (PML4 idx {}), \
         remapped to {:#x} (PML4 idx {}), max_phys={:#x} ({} MiB, {} 2MiB pages)",
        phys_mem_offset.as_u64(),
        (phys_mem_offset.as_u64() >> 39) & 0x1FF,
        PHYS_MAP_BASE,
        (PHYS_MAP_BASE >> 39) & 0x1FF,
        max_phys_aligned,
        max_phys_aligned / (1024 * 1024),
        max_phys_aligned / two_mib,
    );

    // Tell the frame allocator where the physical identity map lives so it
    // can read/write the intrusive free-list pointers in freed pages.
    frame_allocator.set_phys_mem_offset(PHYS_MAP_BASE);

    let mut m = MEMORY.lock();
    assert!(m.is_none(), "memory::init_services called more than once");
    *m = Some(MemoryServices {
        mapper,
        frame_allocator,
        phys_mem_offset: new_offset,
        memory_map: heap_map,
        mmio_next:  MMIO_VIRT_BASE,
        mmio_cache: BTreeMap::new(),
    });
}

/// Run `f` with exclusive access to the kernel memory services.
///
/// Must not be called from interrupt context — the lock is a plain spinlock
/// and is not ISR-safe.
pub fn with_memory<F, R>(f: F) -> R
where
    F: FnOnce(&mut MemoryServices) -> R,
{
    let mut guard = MEMORY.lock();
    let svc = guard.as_mut().expect("memory::init_services not yet called");
    f(svc)
}

/// Switch the active address space by writing a new PML4 physical address to CR3.
///
/// # Safety
/// - Must be called with interrupts disabled (a context switch between the CR3
///   write and the next instruction could land in an inconsistent state).
/// - The new PML4 must have correct kernel entries (indices 256–510) and a
///   valid recursive self-mapping at index 511.
pub unsafe fn switch_address_space(pml4_phys: PhysAddr) {
    use x86_64::registers::control::{Cr3, Cr3Flags};
    let frame = PhysFrame::containing_address(pml4_phys);
    Cr3::write(frame, Cr3Flags::empty());
}

/// Initialise a `RecursivePageTable` for the active PML4.
///
/// # What this does
///
/// 1. Reads CR3 to find the PML4's physical frame.
/// 2. Accesses the PML4 via the bootloader's identity map and writes a
///    self-referential entry at slot `RECURSIVE_INDEX` (511).
/// 3. Flushes the TLB so the new entry is immediately active.
/// 4. Re-accesses the PML4 through its recursive virtual address and wraps it
///    in `RecursivePageTable`.
///
/// After this call the identity map is still alive (the bootloader's PML4
/// entries are never touched), but page-table walks no longer need it.
///
/// # Safety
///
/// * `physical_memory_offset` must be the exact offset at which all physical
///   memory is linearly mapped (supplied by the bootloader).
/// * Must be called exactly once to avoid aliasing `&mut PageTable` references.
pub unsafe fn init(physical_memory_offset: VirtAddr) -> RecursivePageTable<'static> {
    use x86_64::registers::control::Cr3;

    // 1. Find the PML4's physical frame.
    let (pml4_frame, _) = Cr3::read();

    // 2. Access the PML4 via the identity map and install the recursive entry.
    {
        let pml4_virt = physical_memory_offset + pml4_frame.start_address().as_u64();
        let pml4: &mut PageTable = &mut *pml4_virt.as_mut_ptr();
        pml4[PageTableIndex::new(RECURSIVE_INDEX)].set_frame(
            pml4_frame,
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
        );
    } // pml4 reference dropped here — no aliasing with the recursive reference below

    // 3. Flush all TLB entries so the new recursive entry takes effect.
    x86_64::instructions::tlb::flush_all();

    // 4. Access the PML4 through its recursive virtual address.
    //    With RECURSIVE_INDEX=511, this is 0xFFFF_FFFF_FFFF_F000.
    let pml4_recursive: &'static mut PageTable =
        &mut *VirtAddr::new(PML4_RECURSIVE_VIRT).as_mut_ptr();

    RecursivePageTable::new(pml4_recursive)
        .expect("recursive page table setup failed")
}

pub fn map_page(
    page: Page,
    addr: PhysAddr,
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
    flags: PageTableFlags,
) -> VirtAddr {
    let frame = PhysFrame::containing_address(addr);

    let map_to_result = unsafe {
        mapper.map_to(page, frame, flags, frame_allocator)
    };
    map_to_result.expect("map_to failed").flush();

    let offset = addr.as_u64() - frame.start_address().as_u64();
    VirtAddr::new(page.start_address().as_u64() + offset)
}
