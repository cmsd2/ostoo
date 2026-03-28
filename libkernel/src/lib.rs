#![no_std]
#![cfg_attr(test, no_main)]
#![feature(custom_test_frameworks)]
#![test_runner(crate::test_runner)]
#![reexport_test_harness_main = "test_main"]
#![feature(abi_x86_interrupt)]
#![feature(alloc_error_handler)]

extern crate alloc;

#[macro_use]
extern crate log;

#[cfg(test)]
use bootloader::{entry_point, BootInfo};

use core::panic::PanicInfo;
use linked_list_allocator::LockedHeap;
#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

pub mod serial;
pub mod irq_mutex;
pub mod vga_buffer;
pub mod interrupts;
pub mod gdt;
pub mod memory;
pub mod allocator;
pub mod logger;
pub mod cpuid;
pub mod task;
pub mod syscall;
pub mod process;
pub mod elf;
pub mod md5;
pub mod file;
pub mod completion_port;
pub mod console;
pub mod consts;
pub mod apic;
pub mod irq_handle;
pub mod msr;
pub mod path;
pub mod gap;
pub mod signal;

pub fn init() {
    cpuid::init();
    gdt::init();
    enable_sse();
    interrupts::init();
    x86_64::instructions::interrupts::enable();
}

/// Enable SSE/SSE2 so user-space (and kernel) code can use XMM registers.
///
/// Clears CR0.EM (no x87 emulation), sets CR0.MP (monitor coprocessor),
/// sets CR4.OSFXSR (enable FXSAVE/FXRSTOR) and CR4.OSXMMEXCPT (enable
/// unmasked SIMD floating-point exceptions via #XM instead of #UD).
fn enable_sse() {
    use x86_64::registers::control::{Cr0, Cr0Flags, Cr4, Cr4Flags};
    unsafe {
        let mut cr0 = Cr0::read();
        cr0.remove(Cr0Flags::EMULATE_COPROCESSOR);   // clear EM
        cr0.insert(Cr0Flags::MONITOR_COPROCESSOR);    // set MP
        Cr0::write(cr0);

        let mut cr4 = Cr4::read();
        cr4.insert(Cr4Flags::OSFXSR);                 // enable FXSAVE/FXRSTOR
        cr4.insert(Cr4Flags::OSXMMEXCPT_ENABLE);      // enable #XM
        Cr4::write(cr4);
    }
}

#[alloc_error_handler]
fn alloc_error_handler(layout: alloc::alloc::Layout) -> ! {
    panic!("allocation error: {:?}", layout)
}

pub fn test_runner(tests: &[&dyn Fn()]) {
    serial_println!("Running {} tests", tests.len());
    for test in tests {
        test();
    }
    exit_qemu(QemuExitCode::Success);
}

pub fn test_panic_handler(info: &PanicInfo) -> ! {
    serial_println!("[failed]\n");
    serial_println!("Error: {}\n", info);
    exit_qemu(QemuExitCode::Failed);
    hlt_loop();
}

#[cfg(test)]
entry_point!(test_kernel_main);

#[cfg(test)]
pub fn test_kernel_main(boot_info: &'static BootInfo) -> ! {
    use x86_64::VirtAddr;
    init();
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    let mut mapper = unsafe { memory::init(phys_mem_offset) };
    let mut frame_allocator = unsafe {
        memory::BootInfoFrameAllocator::init(&boot_info.memory_map)
    };
    allocator::init_heap(&mut mapper, &mut frame_allocator)
        .expect("heap initialization failed");
    test_main();
    hlt_loop();
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    test_panic_handler(info)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode {
    Success = 0x10,
    Failed = 0x11,
}

pub fn exit_qemu(exit_code: QemuExitCode) {
    use x86_64::instructions::port::Port;

    unsafe {
        let mut port = Port::new(0xf4);
        port.write(exit_code as u32);
    }
}

pub fn hlt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}