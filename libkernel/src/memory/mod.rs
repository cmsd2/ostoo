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

pub mod frame_allocator;
pub mod vmem_allocator;

pub use frame_allocator::BootInfoFrameAllocator;
pub use vmem_allocator::VmemAllocator;
pub use vmem_allocator::DumbVmemAllocator;

// ---------------------------------------------------------------------------
// Global memory services

/// Combined mapper and frame allocator, accessible via [`with_memory`].
///
/// For libraries that require the two components separately (e.g. `apic::init`)
/// destructure with `mem.mapper` / `mem.frame_allocator`.
pub struct MemoryServices {
    pub mapper: OffsetPageTable<'static>,
    pub frame_allocator: BootInfoFrameAllocator,
}

impl MemoryServices {
    /// Map a single 4 KiB page at `page` to the physical address `addr`.
    pub fn map_page(&mut self, page: Page, addr: PhysAddr, flags: PageTableFlags) -> VirtAddr {
        map_page(page, addr, &mut self.mapper, &mut self.frame_allocator, flags)
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
) {
    let mut m = MEMORY.lock();
    assert!(m.is_none(), "memory::init_services called more than once");
    *m = Some(MemoryServices { mapper, frame_allocator });
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
