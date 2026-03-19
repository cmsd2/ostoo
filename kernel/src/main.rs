#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(libkernel::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;
extern crate osl;

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
mod ring3;
mod shell;
mod timeline_actor;

pub const APIC_BASE: u64 = 0xFFFF_8001_0000_0000;

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

    libkernel::vga_buffer::init_display();

    logger::init().expect("logger");
    debug!("debug");
    info!("info");
    warn!("warn");
    error!("error");

    println!("Hello World{}", "!");

    init();

    // Set up SYSCALL/SYSRET mechanism (must come after GDT/IDT are live).
    libkernel::syscall::init(
        libkernel::gdt::kernel_code_selector().0,
        libkernel::gdt::user_code_selector().0,
    );
    libkernel::gdt::set_kernel_stack(libkernel::syscall::kernel_stack_top());

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

    // The bootloader's stack lives in the lower canonical half.  Migrate
    // thread 0 to a heap-allocated stack (PML4 entry 256, high half) so
    // that it survives CR3 switches into user page tables.
    scheduler::migrate_to_heap_stack(run_kernel);
}

fn run_kernel() -> ! {
    use alloc::{boxed::Box, vec, vec::Vec, rc::Rc};

    // Map the VGA framebuffer into the kernel high half so it is accessible
    // from isolated user page tables (entries 256–510 are shared).
    let vga_virt = libkernel::memory::with_memory(|mem| {
        mem.map_mmio_region(x86_64::PhysAddr::new(0xb8000), libkernel::consts::PAGE_SIZE as usize)
    });
    libkernel::vga_buffer::remap_vga(vga_virt);

    let phys_mem_offset = libkernel::memory::with_memory(|mem| mem.phys_mem_offset());

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

    // Map the PCIe ECAM region (Q35 machine: 1 MiB at phys 0xB000_0000 for bus 0).
    let ecam_virt = libkernel::memory::with_memory(|mem| {
        mem.map_mmio_region(x86_64::PhysAddr::new(0xB000_0000), 1024 * 1024)
    });
    devices::virtio::set_ecam_base(ecam_virt.as_u64());

    devices::pci::init();

    // ── virtio-blk probe ─────────────────────────────────────────────────────
    // Probe legacy (0x1001) and modern-transitional (0x1042) virtio-blk devices.
    let blk_dev = devices::pci::find_devices(0x1AF4, 0x1042)
        .into_iter()
        .chain(devices::pci::find_devices(0x1AF4, 0x1001))
        .next();

    if let Some(pci_dev) = blk_dev {
        info!("[kernel] found virtio-blk at {:02x}:{:02x}.{}",
            pci_dev.bus, pci_dev.device, pci_dev.function);
        match devices::virtio::create_blk_transport(
            pci_dev.bus, pci_dev.device, pci_dev.function,
        ) {
            Some(transport) => {
                let actor = devices::virtio::blk::VirtioBlkActor::new(transport);
                let (drv, inbox) =
                    devices::virtio::blk::VirtioBlkActorDriver::new(actor);
                devices::driver::register(Box::new(drv));
                libkernel::task::registry::register("virtio-blk", inbox);
                devices::driver::start_driver("virtio-blk").ok();
                info!("[kernel] virtio-blk registered");
            }
            None => warn!("[kernel] virtio-blk transport init failed"),
        }
    } else {
        info!("[kernel] no virtio-blk device found");
    }

    // ── virtio-9p probe ─────────────────────────────────────────────────────
    let p9_dev = devices::pci::find_devices(0x1AF4, 0x1049)  // modern
        .into_iter()
        .chain(devices::pci::find_devices(0x1AF4, 0x1009))   // legacy
        .next();

    let p9_client: Option<alloc::sync::Arc<devices::virtio::p9::P9Client>> =
        if let Some(pci_dev) = p9_dev {
            info!("[kernel] found virtio-9p at {:02x}:{:02x}.{}",
                pci_dev.bus, pci_dev.device, pci_dev.function);
            devices::virtio::create_pci_transport(
                pci_dev.bus, pci_dev.device, pci_dev.function,
            ).and_then(|transport| {
                match devices::virtio::p9::P9Client::new(transport) {
                    Ok(client) => {
                        info!("[kernel] 9p client initialised");
                        Some(alloc::sync::Arc::new(client))
                    }
                    Err(e) => {
                        warn!("[kernel] 9p client init failed: {:?}", e);
                        None
                    }
                }
            })
        } else {
            info!("[kernel] no virtio-9p device found");
            None
        };

    if let Some(ref client) = p9_client {
        devices::vfs::mount("/host",
            devices::vfs::AnyVfs::Plan9(
                devices::vfs::Plan9Vfs::new(alloc::sync::Arc::clone(client))));
        info!("[kernel] 9p filesystem mounted at /host");
    }

    // Always mount /proc (no block device required).
    devices::vfs::mount("/proc", devices::vfs::AnyVfs::Proc(devices::vfs::ProcVfs));

    // Mount exFAT at / if virtio-blk was registered.
    let have_blk = if let Some(inbox) = libkernel::task::registry::get::<
        devices::virtio::blk::VirtioBlkMsg,
        devices::virtio::blk::VirtioBlkInfo,
    >("virtio-blk") {
        devices::vfs::mount("/", devices::vfs::AnyVfs::Exfat(
            devices::vfs::ExfatVfs::new(inbox)
        ));
        true
    } else {
        false
    };

    // If no block device but 9p is available, mount 9p at / as fallback
    // so /shell auto-launch works without a disk image.
    if !have_blk {
        if let Some(client) = p9_client {
            devices::vfs::mount("/",
                devices::vfs::AnyVfs::Plan9(
                    devices::vfs::Plan9Vfs::new(client)));
            info!("[kernel] 9p filesystem mounted at / (fallback)");
        }
    }

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

    let (tl_driver, tl_inbox) =
        timeline_actor::TimelineActorDriver::new(timeline_actor::TimelineActor::new());
    devices::driver::register(Box::new(tl_driver));
    libkernel::task::registry::register("timeline", tl_inbox);
    devices::driver::start_driver("timeline").ok();

    #[cfg(test)]
    test_main();

    executor::spawn(Task::new(example_task()));
    executor::spawn(Task::new(timer_task()));
    executor::spawn(Task::new(status_task()));
    executor::spawn(Task::new(launch_userspace_shell()));

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
    info!("async number: {}", number);
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

// ---------------------------------------------------------------------------
// Auto-launch userspace shell

async fn launch_userspace_shell() {
    // Wait a bit for VFS to be ready.
    Delay::from_millis(100).await;

    match devices::vfs::read_file("/shell").await {
        Ok(data) => {
            match ring3::spawn_process(&data) {
                Ok(pid) => {
                    info!("[kernel] launched /shell as pid {}", pid.as_u64());
                    libkernel::console::set_foreground(pid);
                }
                Err(e) => {
                    warn!("[kernel] failed to spawn /shell: {}", e);
                    info!("[kernel] falling back to kernel shell");
                }
            }
        }
        Err(e) => {
            info!("[kernel] /shell not found ({:?}), using kernel shell", e);
        }
    }
}