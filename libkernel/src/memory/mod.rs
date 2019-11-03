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
