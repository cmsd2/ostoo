#![no_std]
#![cfg_attr(test, no_main)]
#![feature(custom_test_frameworks)]
#![test_runner(libkernel::test_runner)]
#![reexport_test_harness_main = "test_main"]

#[macro_use]
extern crate bitflags;

use libkernel::{println};
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
};

#[cfg(test)]
use libkernel::{hlt_loop, test_panic_handler};
#[cfg(test)]
use bootloader::{entry_point, BootInfo};
#[cfg(test)]
use core::panic::PanicInfo;

bitflags! {
    pub struct ApicBaseMsrFlags: u64 {
        const BSP           = 0b0001_00000000;
        const GLOBAL_ENABLE = 0b1000_00000000;
    }
}

bitflags! {
    pub struct SivrFlags: u32 {
        const VECTOR = 0b0_0000_11111111;
        const ENABLE = 0b0_0001_00000000;
        const FPC    = 0b0_0010_00000000;
        const EOI    = 0b1_0000_00000000;
    }
}

impl ApicBaseMsrFlags {

}

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
        let value = Self::read_base_msr();
        PhysAddr::new(value & !0xfff)
    }

    pub unsafe fn is_global_enabled(&self) -> bool {
        let value = Self::read_base_msr();
        let flags = ApicBaseMsrFlags::from_bits_truncate(value);
        flags.contains(ApicBaseMsrFlags::GLOBAL_ENABLE)
    }

    pub unsafe fn global_disable(&self) {
        let value = Self::read_base_msr();
        let mut flags = ApicBaseMsrFlags::from_bits_unchecked(value);
        flags.set(ApicBaseMsrFlags::GLOBAL_ENABLE, false);
        Self::write_base_msr(flags.bits());
    }

    pub unsafe fn is_enabled(&self) -> bool {
        self.read_sivr().contains(SivrFlags::ENABLE)
    }

    pub unsafe fn enable(&self) {
        let mut flags = self.read_sivr();
        flags.set(SivrFlags::ENABLE, true);
        self.write_sivr(flags);
    }

    pub unsafe fn read_sivr(&self) -> SivrFlags {
        let sivr = self.read_reg_32(LocalApicRegister::Sivr);
        SivrFlags::from_bits_truncate(sivr)
    }

    pub unsafe fn write_sivr(&self, flags: SivrFlags) {
        self.write_reg_32(LocalApicRegister::Sivr, flags.bits());
    }

    pub unsafe fn read_reg_32(&self, register: LocalApicRegister) -> u32 {
        let addr = register.addr(self.base_addr);
        let ptr = addr.as_ptr::<u32>();
        *ptr
    }

    pub unsafe fn write_reg_32(&self, register: LocalApicRegister, value: u32) {
        let addr = register.addr(self.base_addr);
        let ptr = addr.as_mut_ptr::<u32>();
        *ptr = value;
    }
}

#[repr(u32)]
pub enum LocalApicRegister {
    Id = 0x20,
    Version = 0x30,
    Sivr = 0xf0,
}

impl LocalApicRegister {
    pub fn as_u32(self) -> u32 {
        self as u32
    }

    pub fn as_u64(self) -> u64 {
        u64::from(self.as_u32())
    }

    pub fn addr(self, base: VirtAddr) -> VirtAddr {
        VirtAddr::new(base.as_u64() + self.as_u64())
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
