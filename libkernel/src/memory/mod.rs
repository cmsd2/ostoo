use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;
use x86_64::{VirtAddr, PhysAddr};
use x86_64::structures::paging::{
    Page,
    PageTable,
    PageTableFlags,
    PhysFrame,
    Mapper,
    Size4KiB,
    FrameAllocator,
    OffsetPageTable
};
use bootloader::bootinfo::{MemoryMap, MemoryRegion};

pub mod frame_allocator;
pub mod vmem_allocator;

pub use frame_allocator::BootInfoFrameAllocator;
pub use vmem_allocator::VmemAllocator;
pub use vmem_allocator::DumbVmemAllocator;

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
    mapper: OffsetPageTable<'static>,
    frame_allocator: BootInfoFrameAllocator,
    phys_mem_offset: VirtAddr,
    memory_map: &'static MemoryMap,
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

    /// Map a range of physical MMIO (or any physical) addresses into the linear
    /// physical memory window and return the corresponding virtual address.
    ///
    /// Pages that are already present in the page table are skipped silently.
    /// New pages are mapped with PRESENT | WRITABLE | NO_CACHE flags.
    pub fn map_mmio_region(&mut self, phys_start: PhysAddr, size: usize) -> VirtAddr {
        use x86_64::structures::paging::{
            Mapper, Page, PageTableFlags, PhysFrame, Size4KiB,
        };
        use x86_64::structures::paging::mapper::TranslateError;
        let offset = self.phys_mem_offset;
        let virt_start = offset + phys_start.as_u64();
        let start_page = Page::<Size4KiB>::containing_address(virt_start);
        let num_pages = (size + 4095) / 4096;
        let flags = PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::NO_CACHE;

        for i in 0..num_pages as u64 {
            let page = start_page + i;
            match self.mapper.translate_page(page) {
                Ok(_) => continue, // already mapped as a 4 KiB page — skip
                Err(TranslateError::ParentEntryHugePage) => continue, // inside a huge page — already accessible
                Err(_) => {
                    // Not mapped yet; create a 4 KiB mapping.
                    let phys = PhysAddr::new(phys_start.as_u64() + i * 4096);
                    let frame = PhysFrame::<Size4KiB>::containing_address(phys);
                    unsafe {
                        self.mapper
                            .map_to(page, frame, flags, &mut self.frame_allocator)
                            .expect("map_mmio_region: map_to failed")
                            .flush();
                    }
                }
            }
        }
        virt_start
    }

    /// Allocate `pages` physically-contiguous 4 KiB frames for DMA.
    ///
    /// Returns the base physical address, or `None` if frames are exhausted.
    /// Panics if the allocated frames are not physically contiguous (very
    /// unlikely with the sequential bootloader frame allocator).
    pub fn alloc_dma_pages(&mut self, pages: usize) -> Option<PhysAddr> {
        let mut base: Option<PhysAddr> = None;
        for i in 0..pages {
            let frame = self.frame_allocator.allocate_frame()?;
            let paddr = frame.start_address();
            if i == 0 {
                base = Some(paddr);
            } else {
                let expected = PhysAddr::new(base.unwrap().as_u64() + (i as u64) * 4096);
                assert_eq!(paddr, expected, "alloc_dma_pages: non-contiguous frames");
            }
        }
        base
    }

    /// `(frames_allocated, total_usable_frames)` since boot.
    pub fn frame_stats(&self) -> (usize, usize) {
        (
            self.frame_allocator.frames_allocated(),
            self.frame_allocator.total_usable_frames(),
        )
    }
}

// Safety: OffsetPageTable contains raw pointers (*mut PageTable) which are
// !Send by default.  Access is always serialised through the Mutex, and the
// lock is never held across interrupt boundaries (see with_memory docs).
unsafe impl Send for MemoryServices {}

static MEMORY: Mutex<Option<MemoryServices>> = Mutex::new(None);

/// Store the mapper and frame allocator in global state.
///
/// Call exactly once, after the heap is initialised and before any driver
/// needs to map memory.
pub fn init_services(
    mapper: OffsetPageTable<'static>,
    frame_allocator: BootInfoFrameAllocator,
    phys_mem_offset: VirtAddr,
    memory_map: &'static MemoryMap,
) {
    PHYS_MEM_OFFSET.store(phys_mem_offset.as_u64(), Ordering::Relaxed);
    let mut m = MEMORY.lock();
    assert!(m.is_none(), "memory::init_services called more than once");
    *m = Some(MemoryServices { mapper, frame_allocator, phys_mem_offset, memory_map });
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

/// Initialize a new OffsetPageTable.
///
/// This function is unsafe because the caller must guarantee that the
/// complete physical memory is mapped to virtual memory at the passed
/// `physical_memory_offset`. Also, this function must be only called once
/// to avoid aliasing `&mut` references (which is undefined behavior).
pub unsafe fn init(physical_memory_offset: VirtAddr) -> OffsetPageTable<'static> {
    let level_4_table = active_level_4_table(physical_memory_offset);
    OffsetPageTable::new(level_4_table, physical_memory_offset)
}

/// Returns a mutable reference to the active level 4 table.
///
/// This function is unsafe because the caller must guarantee that the
/// complete physical memory is mapped to virtual memory at the passed
/// `physical_memory_offset`. Also, this function must be only called once
/// to avoid aliasing `&mut` references (which is undefined behavior).
unsafe fn active_level_4_table(physical_memory_offset: VirtAddr)
    -> &'static mut PageTable
{
    use x86_64::registers::control::Cr3;

    let (level_4_table_frame, _) = Cr3::read();

    let phys = level_4_table_frame.start_address();
    let virt = physical_memory_offset + phys.as_u64();
    let page_table_ptr: *mut PageTable = virt.as_mut_ptr();

    &mut *page_table_ptr // unsafe
}

pub fn map_page(
    page: Page,
    addr: PhysAddr,
    mapper: &mut OffsetPageTable,
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
