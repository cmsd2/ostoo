#![no_std]
#![cfg_attr(test, no_main)]
#![feature(custom_test_frameworks)]
#![test_runner(libkernel::test_runner)]
#![reexport_test_harness_main = "test_main"]

#[macro_use]
extern crate bitflags;

#[cfg(test)]
use libkernel::{hlt_loop, test_panic_handler};
#[cfg(test)]
use bootloader::{entry_point, BootInfo};
#[cfg(test)]
use core::panic::PanicInfo;

pub mod ioapic;
pub mod local_apic;
pub mod io_apic;

use spin::Mutex;
use lazy_static::lazy_static;
use libkernel::{println};
use x86_64::VirtAddr;
use x86_64::structures::paging::{
    Page,
    PhysFrame,
    Mapper,
    Size4KiB,
    FrameAllocator,
};
use local_apic::MappedLocalApic;
#[macro_use]
extern crate log;

lazy_static! {
    pub static ref LOCAL_APIC: Mutex<Option<MappedLocalApic>> = Mutex::new(None);
}

pub fn init(remap_addr: VirtAddr, mapper: &mut impl Mapper<Size4KiB>, frame_allocator: &mut impl FrameAllocator<Size4KiB>) {
    use x86_64::structures::paging::page_table::PageTableFlags as Flags;

    let page = Page::from_start_address(remap_addr).expect("remap apic page");
    let frame = PhysFrame::from_start_address(unsafe { MappedLocalApic::get_base_phys_addr() }).expect("non-0 frame offset");
    let flags = Flags::PRESENT | Flags::WRITABLE | Flags::NO_CACHE;

    let map_to_result = unsafe {
        mapper.map_to(page, frame, flags, frame_allocator)
    };

    map_to_result.expect("map_to failed").flush();

    println!("mapped local apic from {:?} to {:?}", frame, page);

    let mut local_apic = LOCAL_APIC.lock();
    let mapped_apic = MappedLocalApic::new(remap_addr);
    unsafe { mapped_apic.init(); }
    *local_apic = Some(mapped_apic);
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
