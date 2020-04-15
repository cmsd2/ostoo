#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(libkernel::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use core::panic::PanicInfo;
use bootloader::{BootInfo, entry_point};
use libkernel::{println, init, hlt_loop};
use libkernel::logger;
use libkernel::task::Task;
use libkernel::task::executor::Executor;
use libkernel::task::keyboard;
use x86_64::VirtAddr;
use log::{debug, info, warn, error};
use acpi::interrupt::InterruptModel;

mod kernel_acpi;

pub const APIC_BASE: u64 = 0x_5555_5555_0000;

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

    logger::init().expect("logger");
    debug!("debug");
    info!("info");
    warn!("warn");
    error!("error");

    println!("Hello World{}", "!");

    init();

    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    let mut mapper = unsafe { memory::init(phys_mem_offset) };
    let mut frame_allocator = unsafe {
        memory::BootInfoFrameAllocator::init(&boot_info.memory_map)
    };
    allocator::init_heap(&mut mapper, &mut frame_allocator)
        .expect("heap initialization failed");

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

    let acpi_table = unsafe {
        kernel_acpi::read_acpi(&mut mapper, &mut frame_allocator).expect("acpi")
    };

    println!("interrupt model: {:?}", acpi_table.interrupt_model);

    if let Some(InterruptModel::Apic(acpi_apic)) = acpi_table.interrupt_model {
        info!("[kernel] init configuring apic");
        //apic::init(&acpi_apic, VirtAddr::new(APIC_BASE), &mut mapper, &mut frame_allocator);
    } else {
        info!("[kernel] init configuring pic");
        //...
    }

    #[cfg(test)]
    test_main();

    let mut executor = Executor::new();
    executor.spawn(Task::new(example_task()));
    executor.spawn(Task::new(keyboard::print_keypresses()));
    executor.run(); // doesn't return
}

async fn async_number() -> u32 {
    42
}

async fn example_task() {
    let number = async_number().await;
    println!("async number: {}", number);
}