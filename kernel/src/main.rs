#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(libkernel::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use core::alloc::Layout;
use core::panic::PanicInfo;
use core::ptr::NonNull;
use bootloader::{BootInfo, entry_point};
use libkernel::{println, init, hlt_loop};
use libkernel::memory::{VmemAllocator, DumbVmemAllocator};
use x86_64::{PhysAddr, VirtAddr, align_up};
use x86_64::structures::paging::{Page, PageSize, PageTableFlags, PhysFrame, FrameAllocator, Size4KiB, Mapper};
use linked_list_allocator::LockedHeap;
use acpi::handler::{PhysicalMapping, AcpiHandler};

pub const APIC_BASE: u64 = 0x_5555_5555_0000;
pub const ACPI_HEAP_BASE: u64 = 0x_6666_6666_0000;
pub const ACPI_HEAP_SIZE: u64 = 100 * 4096;

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("{}", info);

    hlt_loop();
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    libkernel::test_panic_handler(info)
}

entry_point!(libkernel_main);

pub fn libkernel_main(boot_info: &'static BootInfo) -> ! {
    use libkernel::memory;
    use libkernel::allocator;
    use alloc::{boxed::Box, vec, vec::Vec, rc::Rc};

    println!("Hello World{}", "!");

    init();

    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    let mut mapper = unsafe { memory::init(phys_mem_offset) };
    let mut frame_allocator = unsafe {
        memory::BootInfoFrameAllocator::init(&boot_info.memory_map)
    };
    allocator::init_heap(&mut mapper, &mut frame_allocator)
        .expect("heap initialization failed");

    apic::init(VirtAddr::new(APIC_BASE), &mut mapper, &mut frame_allocator);

    let heap_value = Box::new(41);
    println!("heap_value at {:p}", heap_value);

    // create a dynamically sized vector
    let mut vec = Vec::new();
    for i in 0..500 {
        vec.push(i);
    }
    println!("vec at {:p}", vec.as_slice());

    // create a reference counted vector -> will be freed when count reaches 0
    let reference_counted = Rc::new(vec![1, 2, 3]);
    let cloned_reference = reference_counted.clone();
    println!("current reference count is {}", Rc::strong_count(&cloned_reference));
    core::mem::drop(reference_counted);
    println!("reference count is {} now", Rc::strong_count(&cloned_reference));

    unsafe {
        // TODO: get acpi table addresses from bootloader
        acpi::search_for_rsdp_bios(&mut AcpiMapper {
            vmem_allocator: DumbVmemAllocator::new(VirtAddr::new(ACPI_HEAP_BASE), ACPI_HEAP_SIZE),
            page_table: &mut mapper,
            frame_allocator: &mut frame_allocator,
        }).expect("acpi");
    }

    #[cfg(test)]
    test_main();

    hlt_loop();
}

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
        let start_frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(physical_address as u64));
        let end_frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(physical_address as u64 + size as u64).align_up(Size4KiB::SIZE));
        let frame_range = PhysFrame::<Size4KiB>::range(start_frame, end_frame);

        let layout_size = align_up(size as u64, Size4KiB::SIZE);
        let (start_page, end_page) = self.vmem_allocator.alloc(layout_size / Size4KiB::SIZE);
        let page_range = Page::<Size4KiB>::range_inclusive(start_page, end_page);

        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;

        for (frame, page) in frame_range.zip(page_range) {
            unsafe {
                self.page_table.map_to(page, frame, flags, self.frame_allocator)
                    .expect("map_to")
                    .flush();
            }
        }

        PhysicalMapping {
            physical_start: start_frame.start_address().as_u64() as usize,
            virtual_start: NonNull::new(start_page.start_address().as_mut_ptr()).unwrap(),
            region_length: size,
            mapped_length: layout_size as usize,
        }
    }

    fn unmap_physical_region<T>(&mut self, region: PhysicalMapping<T>) {
        let start_page = Page::from_start_address(VirtAddr::new(region.virtual_start.as_ptr() as usize as u64)).expect("page");
        let end_page = start_page + region.mapped_length as u64 / Size4KiB::SIZE;
        let page_range = Page::<Size4KiB>::range(start_page, end_page);

        for page in page_range {
            let (_frame, flush) = self.page_table.unmap(page).expect("unmap");
            // could free frame except dumb vmem allocator does nothing
            flush.flush();
        }
    }
}