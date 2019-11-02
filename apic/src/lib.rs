#![no_std]
#![cfg_attr(test, no_main)]
#![feature(custom_test_frameworks)]
#![test_runner(libkernel::test_runner)]
#![reexport_test_harness_main = "test_main"]

use libkernel::{println, hlt_loop, test_panic_handler};
use raw_cpuid::CpuId;
use spin::Mutex;
use lazy_static::lazy_static;
use x86_64::registers::model_specific::Msr;
use x86_64::{VirtAddr, PhysAddr};
use x86_64::structures::paging::{
    Page,
    PhysFrame,
    Mapper,
    Size4KiB,
    FrameAllocator,
    OffsetPageTable
};
use x86_64::structures::paging::page::PageSize;
#[cfg(test)]
use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;

#[derive(Debug)]
pub enum LocalApiError {
    MissingCpuidFeatures
}

lazy_static! {
    pub static ref LOCAL_APIC: Mutex<Option<LocalApic>> = Mutex::new(None);
}

const IA32_APIC_BASE_MSR: u32 = 0x1b;

pub struct LocalApic {
    base_addr: VirtAddr,
}

impl LocalApic {
    unsafe fn read_base_msr() -> u64 {
        let msr = Msr::new(IA32_APIC_BASE_MSR);
        msr.read()
    }

    unsafe fn write_base_msr(value: u64) {
        let mut msr = Msr::new(IA32_APIC_BASE_MSR);
        msr.write(value)
    }

    unsafe fn get_base_phys_addr() -> PhysAddr {
        let msr = Msr::new(IA32_APIC_BASE_MSR);
        let value = Self::read_base_msr();
        PhysAddr::new(value & !0xfff)
    }

    fn get_base_addr(&self) -> VirtAddr {
        self.base_addr
    }

    pub fn enable(&self) {
        
    }
}

pub fn init(remap_addr: VirtAddr, mapper: &mut impl Mapper<Size4KiB>, frame_allocator: &mut impl FrameAllocator<Size4KiB>) {
    use x86_64::structures::paging::page_table::PageTableFlags as Flags;

    let page = Page::from_start_address(remap_addr).expect("remap apic page");
    let frame = PhysFrame::containing_address(unsafe { LocalApic::get_base_phys_addr() });
    let flags = Flags::PRESENT | Flags::WRITABLE | Flags::NO_CACHE;

    let map_to_result = unsafe {
        mapper.map_to(page, frame, flags, frame_allocator)
    };

    map_to_result.expect("map_to failed").flush();

    println!("mapped local apic from {:?} to {:?}", frame, page);

    let mut local_apic = LOCAL_APIC.lock();
    *local_apic = Some(LocalApic {
        base_addr: remap_addr,
    });
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
