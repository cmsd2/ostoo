#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(libkernel::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use core::panic::PanicInfo;
use bootloader::{BootInfo, entry_point};
use libkernel::{println, init};
#[cfg(not(test))]
use libkernel::hlt_loop;
use libkernel::logger;
use libkernel::task::Task;
use libkernel::task::executor;
use libkernel::task::scheduler;
use libkernel::task::timer::{Delay, ticks};
use x86_64::VirtAddr;
use log::{debug, info, warn, error};
use acpi::platform::interrupt::InterruptModel;

// Expose task_driver at crate root so #[actor]-generated code
// (`crate::task_driver::...`) resolves in modules outside `devices`.
pub mod task_driver;

mod kernel_acpi;
mod keyboard_actor;
mod shell;

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

    libkernel::vga_buffer::init_display();

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

    // Hand ownership to the global memory services so drivers can map memory
    // at runtime via libkernel::memory::with_memory().
    libkernel::memory::init_services(mapper, frame_allocator, phys_mem_offset, &boot_info.memory_map);

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

    let interrupt_model = unsafe {
        kernel_acpi::read_acpi(phys_mem_offset).expect("acpi")
    };

    println!("interrupt model: {:?}", interrupt_model);

    if let InterruptModel::Apic(_) = interrupt_model {
        info!("[kernel] init configuring apic");
        libkernel::memory::with_memory(|mem| {
            apic::init(&interrupt_model, VirtAddr::new(APIC_BASE), mem);
        });
        libkernel::interrupts::disable_pic();
        apic::calibrate_and_start_lapic_timer();
    } else {
        info!("[kernel] init configuring pic");
    }

    devices::pci::init();
    let (dummy_driver, dummy_inbox) =
        devices::dummy::DummyDriver::new(devices::dummy::Dummy::new());
    devices::driver::register(Box::new(dummy_driver));
    libkernel::task::registry::register("dummy", dummy_inbox);

    let (shell_driver, shell_inbox) =
        shell::ShellDriver::new(shell::Shell::new());
    devices::driver::register(Box::new(shell_driver));
    libkernel::task::registry::register("shell", shell_inbox.clone());
    devices::driver::start_driver("shell").ok();

    let (kb_driver, kb_inbox) =
        keyboard_actor::KeyboardActorDriver::new(keyboard_actor::KeyboardActor::new());
    devices::driver::register(Box::new(kb_driver));
    libkernel::task::registry::register("keyboard", kb_inbox);
    devices::driver::start_driver("keyboard").ok();

    #[cfg(test)]
    test_main();

    executor::spawn(Task::new(example_task()));
    executor::spawn(Task::new(timer_task()));
    executor::spawn(Task::new(status_task()));

    // Register the current context as thread 0 of the preemptive scheduler.
    scheduler::init();

    // Spawn thread 1 — also runs the executor, demonstrating multi-threaded
    // task dispatch.  Both threads compete for tasks from the shared queue.
    scheduler::spawn_thread(|| executor::run_worker());

    // Thread 0 enters the executor loop.  The LAPIC timer will preempt it
    // every 10 ms regardless of what the running async task is doing.
    executor::run_worker();
}

async fn async_number() -> u32 {
    42
}

async fn example_task() {
    let number = async_number().await;
    println!("async number: {}", number);
}

async fn timer_task() {
    loop {
        Delay::from_secs(1).await;
        info!("[timer] tick: {}s elapsed", ticks() / libkernel::task::timer::TICKS_PER_SECOND);
    }
}

async fn status_task() {
    loop {
        Delay::from_millis(250).await;
        let ctx = scheduler::context_switches();
        let rdy = executor::ready_count();
        let wait = executor::wait_count();
        let secs = ticks() / libkernel::task::timer::TICKS_PER_SECOND;
        libkernel::status_bar!(
            " T{} | ctx:{:6} | rdy:{} wait:{} | up:{:6}s",
            scheduler::current_thread_idx(), ctx, rdy, wait, secs
        );
    }
}