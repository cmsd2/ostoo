#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(libkernel::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;
extern crate osl;

use alloc::boxed::Box;
use alloc::sync::Arc;
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
use log::{info, warn};
use acpi::platform::interrupt::InterruptModel;

// Expose task_driver at crate root so #[actor]-generated code
// (`crate::task_driver::...`) resolves in modules outside `devices`.
pub mod task_driver;

mod kernel_acpi;
mod keyboard_actor;
mod ring3;
mod shell;
mod timeline_actor;

// ---------------------------------------------------------------------------
// Constants

const APIC_BASE: u64 = 0xFFFF_8001_0000_0000;
const VGA_PHYS: u64 = 0xB8000;
const ECAM_PHYS: u64 = 0xB000_0000;
const ECAM_SIZE: usize = 1024 * 1024;

const VIRTIO_VENDOR: u16 = 0x1AF4;
const VIRTIO_BLK_MODERN: u16 = 0x1042;
const VIRTIO_BLK_LEGACY: u16 = 0x1001;
const VIRTIO_9P_MODERN: u16 = 0x1049;
const VIRTIO_9P_LEGACY: u16 = 0x1009;

// ---------------------------------------------------------------------------
// Panic handlers

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

// ---------------------------------------------------------------------------
// Entry point and early init

entry_point!(libkernel_main);

pub fn libkernel_main(boot_info: &'static BootInfo) -> ! {
    use libkernel::memory;
    use libkernel::allocator;

    libkernel::vga_buffer::init_display();
    logger::init().expect("logger");
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

    memory::init_services(mapper, frame_allocator, phys_mem_offset, &boot_info.memory_map);

    // Migrate thread 0 to a heap-allocated stack (high half) so it
    // survives CR3 switches into user page tables.
    scheduler::migrate_to_heap_stack(run_kernel);
}

// ---------------------------------------------------------------------------
// Kernel main (runs on heap stack)

fn run_kernel() -> ! {
    const BOOT_STEPS: usize = 7;
    let progress = |step, label| libkernel::vga_buffer::boot_progress(step, BOOT_STEPS, label);

    progress(0, "Remapping VGA...");
    init_vga_remap();
    progress(1, "VGA remapped");

    progress(1, "Configuring interrupts...");
    init_apic();
    progress(2, "Interrupts configured");

    progress(2, "Scanning PCI bus...");
    init_pci();
    progress(3, "PCI bus scanned");

    progress(3, "Probing virtio-blk...");
    init_virtio_blk();
    progress(4, "virtio-blk done");

    progress(4, "Probing virtio-9p...");
    let p9_client = init_virtio_9p();
    progress(5, "virtio-9p done");

    progress(5, "Mounting filesystems...");
    init_vfs_mounts(p9_client);
    progress(6, "Filesystems mounted");

    progress(6, "Starting actors...");
    init_actors();
    progress(7, "Ready");

    libkernel::vga_buffer::boot_progress_done();

    #[cfg(test)]
    test_main();

    executor::spawn(Task::new(timer_task()));
    executor::spawn(Task::new(status_task()));
    executor::spawn(Task::new(launch_userspace_shell()));

    scheduler::init();
    scheduler::spawn_thread(|| executor::run_worker());
    executor::run_worker();
}

// ---------------------------------------------------------------------------
// Init helpers

/// Remap the VGA framebuffer into the kernel high half so it is accessible
/// from isolated user page tables (entries 256–510 are shared).
fn init_vga_remap() {
    let vga_virt = libkernel::memory::with_memory(|mem| {
        mem.map_mmio_region(x86_64::PhysAddr::new(VGA_PHYS), libkernel::consts::PAGE_SIZE as usize)
    });
    libkernel::vga_buffer::remap_vga(vga_virt);
}

/// Parse ACPI tables and configure APIC (or fall back to legacy PIC).
fn init_apic() {
    let phys_mem_offset = libkernel::memory::with_memory(|mem| mem.phys_mem_offset());
    let interrupt_model = unsafe {
        kernel_acpi::read_acpi(phys_mem_offset).expect("acpi")
    };

    if let InterruptModel::Apic(_) = interrupt_model {
        info!("[kernel] configuring APIC");
        libkernel::memory::with_memory(|mem| {
            apic::init(&interrupt_model, VirtAddr::new(APIC_BASE), mem);
        });
        libkernel::interrupts::disable_pic();
        apic::calibrate_and_start_lapic_timer();
    } else {
        info!("[kernel] configuring PIC (legacy)");
    }
}

/// Map the PCIe ECAM region and scan for devices.
fn init_pci() {
    let ecam_virt = libkernel::memory::with_memory(|mem| {
        mem.map_mmio_region(x86_64::PhysAddr::new(ECAM_PHYS), ECAM_SIZE)
    });
    devices::virtio::set_ecam_base(ecam_virt.as_u64());
    devices::pci::init();
}

/// Find the first PCI device matching the given vendor and either device ID.
fn find_virtio_device(modern_id: u16, legacy_id: u16) -> Option<devices::pci::PciDevice> {
    devices::pci::find_devices(VIRTIO_VENDOR, modern_id)
        .into_iter()
        .chain(devices::pci::find_devices(VIRTIO_VENDOR, legacy_id))
        .next()
}

/// Probe, initialise, and register the virtio-blk actor.
fn init_virtio_blk() {
    let pci_dev = match find_virtio_device(VIRTIO_BLK_MODERN, VIRTIO_BLK_LEGACY) {
        Some(d) => d,
        None => { info!("[kernel] no virtio-blk device found"); return; }
    };

    info!("[kernel] found virtio-blk at {:02x}:{:02x}.{}",
        pci_dev.bus, pci_dev.device, pci_dev.function);

    let transport = match devices::virtio::create_pci_transport(
        pci_dev.bus, pci_dev.device, pci_dev.function,
    ) {
        Some(t) => t,
        None => { warn!("[kernel] virtio-blk transport init failed"); return; }
    };

    let actor = devices::virtio::blk::VirtioBlkActor::new(transport);
    let (drv, inbox) = devices::virtio::blk::VirtioBlkActorDriver::new(actor);
    devices::driver::register(Box::new(drv));
    libkernel::task::registry::register("virtio-blk", inbox);
    devices::driver::start_driver("virtio-blk").ok();
    info!("[kernel] virtio-blk registered");
}

/// Probe and initialise the virtio-9p client (if present).
fn init_virtio_9p() -> Option<Arc<devices::virtio::p9::P9Client>> {
    let pci_dev = match find_virtio_device(VIRTIO_9P_MODERN, VIRTIO_9P_LEGACY) {
        Some(d) => d,
        None => { info!("[kernel] no virtio-9p device found"); return None; }
    };

    info!("[kernel] found virtio-9p at {:02x}:{:02x}.{}",
        pci_dev.bus, pci_dev.device, pci_dev.function);

    let transport = devices::virtio::create_pci_transport(
        pci_dev.bus, pci_dev.device, pci_dev.function,
    )?;

    match devices::virtio::p9::P9Client::new(transport) {
        Ok(client) => {
            info!("[kernel] 9p client initialised");
            Some(Arc::new(client))
        }
        Err(e) => {
            warn!("[kernel] 9p client init failed: {:?}", e);
            None
        }
    }
}

/// Set up VFS mount table: /host (9p), /proc, / (exfat or 9p fallback).
fn init_vfs_mounts(p9_client: Option<Arc<devices::virtio::p9::P9Client>>) {
    if let Some(ref client) = p9_client {
        devices::vfs::mount("/host",
            devices::vfs::AnyVfs::Plan9(
                devices::vfs::Plan9Vfs::new(Arc::clone(client))));
        info!("[kernel] 9p filesystem mounted at /host");
    }

    devices::vfs::mount("/proc", devices::vfs::AnyVfs::Proc(devices::vfs::ProcVfs));

    // / — exFAT if disk present, else 9p fallback
    if let Some(inbox) = libkernel::task::registry::get::<
        devices::virtio::blk::VirtioBlkMsg,
        devices::virtio::blk::VirtioBlkInfo,
    >("virtio-blk") {
        devices::vfs::mount("/", devices::vfs::AnyVfs::Exfat(
            devices::vfs::ExfatVfs::new(inbox)));
    } else if let Some(client) = p9_client {
        devices::vfs::mount("/",
            devices::vfs::AnyVfs::Plan9(
                devices::vfs::Plan9Vfs::new(client)));
        info!("[kernel] 9p filesystem mounted at / (fallback)");
    }
}

/// Register and start the built-in actors (dummy, shell, keyboard, timeline).
fn init_actors() {
    let (drv, inbox) = devices::dummy::DummyDriver::new(devices::dummy::Dummy::new());
    devices::driver::register(Box::new(drv));
    libkernel::task::registry::register("dummy", inbox);

    let (drv, inbox) = shell::ShellDriver::new(shell::Shell::new());
    devices::driver::register(Box::new(drv));
    libkernel::task::registry::register("shell", inbox.clone());
    devices::driver::start_driver("shell").ok();

    let (drv, inbox) =
        keyboard_actor::KeyboardActorDriver::new(keyboard_actor::KeyboardActor::new());
    devices::driver::register(Box::new(drv));
    libkernel::task::registry::register("keyboard", inbox);
    devices::driver::start_driver("keyboard").ok();

    let (drv, inbox) =
        timeline_actor::TimelineActorDriver::new(timeline_actor::TimelineActor::new());
    devices::driver::register(Box::new(drv));
    libkernel::task::registry::register("timeline", inbox);
    devices::driver::start_driver("timeline").ok();
}

// ---------------------------------------------------------------------------
// Async tasks

async fn timer_task() {
    loop {
        Delay::from_secs(1).await;
        info!("[timer] tick: {}s elapsed", ticks() / libkernel::task::timer::TICKS_PER_SECOND);
    }
}

async fn status_task() {
    loop {
        Delay::from_millis(250).await;
        libkernel::status_bar!(
            " T{} | ctx:{:6} | rdy:{} wait:{} | up:{:6}s",
            scheduler::current_thread_idx(),
            scheduler::context_switches(),
            executor::ready_count(),
            executor::wait_count(),
            ticks() / libkernel::task::timer::TICKS_PER_SECOND,
        );
    }
}

async fn launch_userspace_shell() {
    Delay::from_millis(100).await; // let VFS settle

    let data = match devices::vfs::read_file("/shell").await {
        Ok(d) => d,
        Err(e) => {
            info!("[kernel] /shell not found ({:?}), using kernel shell", e);
            return;
        }
    };

    let pid = match ring3::spawn_process(&data) {
        Ok(pid) => {
            info!("[kernel] launched /shell as pid {}", pid.as_u64());
            libkernel::console::set_foreground(pid);
            pid
        }
        Err(e) => {
            warn!("[kernel] failed to spawn /shell: {}", e);
            info!("[kernel] falling back to kernel shell");
            return;
        }
    };

    wait_and_reap(pid).await;
    println!("\n[kernel] userspace shell exited — type 'help' for kernel commands");

    // Tell the kernel shell actor to redraw its prompt.
    if let Some(inbox) = libkernel::task::registry::get::<
        shell::ShellMsg, (),
    >("shell") {
        use libkernel::task::mailbox::ActorMsg;
        inbox.send(ActorMsg::Inner(shell::ShellMsg::Reprompt));
    }
}

/// Poll until `pid` becomes a zombie, then reap it and reset the foreground
/// back to the kernel shell.
async fn wait_and_reap(pid: libkernel::process::ProcessId) {
    loop {
        Delay::from_millis(50).await;
        if libkernel::process::is_zombie(pid) {
            break;
        }
    }
    libkernel::process::reap(pid);
    libkernel::console::set_foreground(libkernel::process::ProcessId::KERNEL);
}
