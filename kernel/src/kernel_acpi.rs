use core::ptr::NonNull;
use libkernel::memory::{VmemAllocator, DumbVmemAllocator};
use x86_64::{PhysAddr, VirtAddr, align_up};
use x86_64::structures::paging::{Page, PageSize, PageTableFlags, PhysFrame, UnusedPhysFrame, FrameAllocator, Size4KiB, Mapper};
use acpi::handler::{PhysicalMapping, AcpiHandler};
use acpi::{Acpi, AcpiError};

pub const ACPI_HEAP_BASE: u64 = 0x_6666_6666_0000;
pub const ACPI_HEAP_SIZE: u64 = 200 * 4096;

struct AcpiMapper<'a, F, M> where F: FrameAllocator<Size4KiB>, M: Mapper<Size4KiB> {
    vmem_allocator: DumbVmemAllocator<Size4KiB>,
    page_table: &'a mut M,
    frame_allocator: &'a mut F,
}

impl <'a, F, M> AcpiHandler for AcpiMapper<'a, F, M> where F: FrameAllocator<Size4KiB>, M: Mapper<Size4KiB> {
    fn map_physical_region<T>(
        &mut self, 
        physical_address: usize, 
        size: usize
    ) -> PhysicalMapping<T> {
        let physical_address = PhysAddr::new(physical_address as u64);
        let start_frame = PhysFrame::<Size4KiB>::containing_address(physical_address);
        let end_frame = PhysFrame::<Size4KiB>::containing_address((physical_address + size).align_up(Size4KiB::SIZE));
        let frame_range = PhysFrame::<Size4KiB>::range(start_frame, end_frame);

        let layout_size = align_up(size as u64, Size4KiB::SIZE);
        let (start_page, end_page) = self.vmem_allocator.alloc(layout_size / Size4KiB::SIZE);
        let page_range = Page::<Size4KiB>::range_inclusive(start_page, end_page);

        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;

        for (frame, page) in frame_range.zip(page_range) {
            unsafe {
                let unused_frame = UnusedPhysFrame::new(frame);
                self.page_table.map_to(page, unused_frame, flags, self.frame_allocator)
                    .expect("map_to")
                    .flush();
            }
        }

        let page_offset = physical_address - start_frame.start_address();

        PhysicalMapping {
            physical_start: start_frame.start_address().as_u64() as usize,
            virtual_start: NonNull::new((start_page.start_address() + page_offset).as_mut_ptr()).unwrap(),
            region_length: size,
            mapped_length: layout_size as usize,
        }
    }

    fn unmap_physical_region<T>(&mut self, region: PhysicalMapping<T>) {
        let start_page = Page::containing_address(VirtAddr::new(region.virtual_start.as_ptr() as usize as u64));
        let end_page = start_page + region.mapped_length as u64 / Size4KiB::SIZE;
        let page_range = Page::<Size4KiB>::range(start_page, end_page);

        for page in page_range {
            let (_frame, flush) = self.page_table.unmap(page).expect("unmap");
            // could free frame except dumb vmem allocator does nothing
            flush.flush();
        }
    }
}

pub unsafe fn read_acpi<'a, F, M>(mapper: &mut M, frame_allocator: &mut F) -> Result<Acpi, AcpiError> 
        where F: FrameAllocator<Size4KiB>, M: Mapper<Size4KiB> {
    // TODO: get acpi table addresses from bootloader
    let mut mapper = AcpiMapper {
        vmem_allocator: DumbVmemAllocator::new(VirtAddr::new(ACPI_HEAP_BASE), ACPI_HEAP_SIZE),
        page_table: mapper,
        frame_allocator: frame_allocator,
    };

    acpi::search_for_rsdp_bios(&mut mapper)
}
