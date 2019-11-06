#![no_std]
#![cfg_attr(test, no_main)]
#![feature(custom_test_frameworks)]
#![test_runner(libkernel::test_runner)]
#![reexport_test_harness_main = "test_main"]

#[macro_use]
extern crate bitflags;

extern crate alloc;

#[cfg(test)]
use libkernel::{hlt_loop, test_panic_handler};
#[cfg(test)]
use bootloader::{entry_point, BootInfo};
#[cfg(test)]
use core::panic::PanicInfo;

pub mod ioapic;
pub mod local_apic;
pub mod io_apic;

use alloc::vec::Vec;
use acpi::interrupt::{Apic as AcpiApic, IoApic as AcpiIoApic};
use spin::Mutex;
use lazy_static::lazy_static;
use libkernel::{println};
use x86_64::{PhysAddr, VirtAddr};
use x86_64::structures::paging::{
    Page,
    PageSize,
    PhysFrame,
    Mapper,
    Size4KiB,
    FrameAllocator,
};
use local_apic::MappedLocalApic;
use io_apic::MappedIoApic;
#[macro_use]
extern crate log;

lazy_static! {
    pub static ref LOCAL_APIC: Mutex<Option<MappedLocalApic>> = Mutex::new(None);
}

lazy_static! {
    pub static ref IO_APICS: Mutex<Vec<MappedIoApic>> = Mutex::new(Vec::new());
}

pub fn init(interrupt_model: &AcpiApic, remap_addr: VirtAddr, mapper: &mut impl Mapper<Size4KiB>, frame_allocator: &mut impl FrameAllocator<Size4KiB>) {
    init_local(interrupt_model, remap_addr, mapper, frame_allocator);
    init_io(interrupt_model, remap_addr + Size4KiB::SIZE, mapper, frame_allocator);
}

pub fn init_io(interrupt_model: &AcpiApic, mut remap_addr: VirtAddr, mapper: &mut impl Mapper<Size4KiB>, frame_allocator: &mut impl FrameAllocator<Size4KiB>) {
    use x86_64::structures::paging::page_table::PageTableFlags as Flags;

    let mapped_io_apics = interrupt_model.io_apics.iter().map(|io_apic| {
        let mapped_io_apic = init_io_apic(io_apic, remap_addr, mapper, frame_allocator);

        remap_addr = remap_addr + Size4KiB::SIZE;

        mapped_io_apic
    }).collect();

    let mut io_apics = IO_APICS.lock();
    *io_apics = mapped_io_apics;
}

fn init_io_apic(io_apic: &AcpiIoApic, remap_addr: VirtAddr, mapper: &mut impl Mapper<Size4KiB>, frame_allocator: &mut impl FrameAllocator<Size4KiB>) -> MappedIoApic {
    let (frame, page) = map(remap_addr, PhysAddr::new(io_apic.address as u64), mapper, frame_allocator);

    info!("[apic] init mapped {:?} from {:?} to {:?}", io_apic, frame, page);

    let mapped_io_apic = MappedIoApic {
        id: io_apic.id,
        base_addr: remap_addr,
        interrupt_base: io_apic.global_system_interrupt_base,
    };

    mapped_io_apic.init();

    mapped_io_apic
}

pub fn init_local(interrupt_model: &AcpiApic, remap_addr: VirtAddr, mapper: &mut impl Mapper<Size4KiB>, frame_allocator: &mut impl FrameAllocator<Size4KiB>) {
    let phys_addr = unsafe {
        MappedLocalApic::get_base_phys_addr()
    };

    let (frame, page) = map(remap_addr, phys_addr, mapper, frame_allocator);

    info!("[apic] init mapped local apic from {:?} to {:?}", frame, page);

    let mut local_apic = LOCAL_APIC.lock();
    let mapped_apic = MappedLocalApic::new(remap_addr);
    unsafe { mapped_apic.init(); }
    *local_apic = Some(mapped_apic);
}

fn map(remap_addr: VirtAddr, phys_addr: PhysAddr, mapper: &mut impl Mapper<Size4KiB>, frame_allocator: &mut impl FrameAllocator<Size4KiB>) -> (PhysFrame, Page) {
    use x86_64::structures::paging::page_table::PageTableFlags as Flags;

    let page = Page::from_start_address(remap_addr).expect("remap apic page");
    let frame = PhysFrame::from_start_address(phys_addr).expect("non-0 frame offset");
    let flags = Flags::PRESENT | Flags::WRITABLE | Flags::NO_CACHE;

    let map_to_result = unsafe {
        mapper.map_to(page, frame, flags, frame_allocator)
    };

    map_to_result.expect("map_to failed").flush();

    (frame, page)
}

#[cfg(test)]
entry_point!(test_apic_main);

#[cfg(test)]
pub fn test_apic_main(_boot_info: &'static BootInfo) -> ! {
    //init();
    test_main();
    hlt_loop();
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    test_panic_handler(info)
}
