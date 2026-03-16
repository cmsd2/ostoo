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
    use alloc::{boxed::Box, vec, vec::Vec, rc::Rc};

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

    // Always mount /proc (no block device required).
    devices::vfs::mount("/proc", devices::vfs::AnyVfs::Proc(devices::vfs::ProcVfs));

    // Mount exFAT at / if virtio-blk was registered.
    if let Some(inbox) = libkernel::task::registry::get::<
        devices::virtio::blk::VirtioBlkMsg,
        devices::virtio::blk::VirtioBlkInfo,
    >("virtio-blk") {
        devices::vfs::mount("/", devices::vfs::AnyVfs::Exfat(
            devices::vfs::ExfatVfs::new(inbox)
        ));
    }

    // Ring-3 smoke test: print "Hello from ring 3!\n" via syscall, then exit.
    // Halts the machine after sys_exit; never reaches the driver/executor code.
    // Build with `--features ring3_test` to enable.
    #[cfg(feature = "ring3_test")]
    run_ring3_test();

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

// Assembly blob that will run in ring 3.  Position-independent: the message
// pointer uses RIP-relative addressing, so the blob works wherever it is copied.
// `ring3_user_code_end` lets us compute the size at compile time.
#[cfg(feature = "ring3_test")]
core::arch::global_asm!(r#"
.global ring3_user_code_start
.global ring3_user_code_end
ring3_user_code_start:
    mov  rax, 1                     /* SYS_write */
    mov  rdi, 1                     /* fd = stdout */
    lea  rsi, [rip + ring3_msg]     /* buf (RIP-relative, works after copy) */
    mov  rdx, 19                    /* len */
    syscall
    mov  rax, 60                    /* SYS_exit */
    xor  edi, edi                   /* exit code 0 */
    syscall
ring3_msg:
    .ascii "Hello from ring 3!\n"
ring3_user_code_end:
"#);

// Linker-defined symbols bracketing the ring-3 code blob.
#[cfg(feature = "ring3_test")]
extern "C" {
    static ring3_user_code_start: u8;
    static ring3_user_code_end:   u8;
}

/// One-shot ring-3 smoke test.  Enabled by `--features ring3_test`.
///
/// Copies the `ring3_user_code_start..end` blob to a fresh user-accessible
/// page at 0x0040_0000, then drops to ring 3 via `iretq`.  The user code
/// calls `write(1, msg, 19)` and `exit(0)` via `syscall`.  `sys_exit` halts.
///
/// Declared as `fn` (not `fn -> !`) so the caller doesn't get an
/// unreachable-code lint on the executor path below it.
#[cfg(feature = "ring3_test")]
fn run_ring3_test() {
    use libkernel::memory::with_memory;
    use x86_64::structures::paging::{Page, PageTableFlags, Size4KiB};

    let (code_ptr, code_len) = unsafe {
        let start = &raw const ring3_user_code_start as *const u8;
        let end   = &raw const ring3_user_code_end   as *const u8;
        (start, end.offset_from(start) as usize)
    };
    assert!(code_len <= 0x1000, "ring3_test code blob exceeds one page");

    const USER_CODE_VIRT: u64 = 0x0040_0000;
    const USER_STACK_VIRT: u64 = 0x0050_0000;
    const USER_STACK_TOP: u64  = USER_STACK_VIRT + 0x1000;

    with_memory(|mem| {
        // Allocate a physical frame for user code, copy the blob into it via
        // the physical-memory map, then make it visible at USER_CODE_VIRT.
        let code_phys = mem
            .alloc_dma_pages(1)
            .expect("ring3_test: out of frames (code)");
        let dst_virt = mem.phys_mem_offset() + code_phys.as_u64();
        unsafe {
            core::ptr::copy_nonoverlapping(
                code_ptr,
                dst_virt.as_mut_ptr::<u8>(),
                code_len,
            );
        }
        mem.map_page(
            Page::<Size4KiB>::from_start_address(x86_64::VirtAddr::new(USER_CODE_VIRT)).unwrap(),
            code_phys,
            PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE,
        );

        // Allocate a physical frame for user stack and map it at USER_STACK_VIRT.
        let stack_phys = mem
            .alloc_dma_pages(1)
            .expect("ring3_test: out of frames (stack)");
        mem.map_page(
            Page::<Size4KiB>::from_start_address(x86_64::VirtAddr::new(USER_STACK_VIRT)).unwrap(),
            stack_phys,
            PageTableFlags::PRESENT
                | PageTableFlags::WRITABLE
                | PageTableFlags::USER_ACCESSIBLE
                | PageTableFlags::NO_EXECUTE,
        );
    });

    // Swap GS so that GS.BASE = 0 (user) and KERNEL_GS_BASE = &PER_CPU.
    // The SYSCALL entry stub will then restore kernel GS with its own swapgs.
    libkernel::syscall::prepare_swapgs();

    let user_cs = libkernel::gdt::user_code_selector().0 as u64;
    let user_ss = libkernel::gdt::user_data_selector().0 as u64;

    // Drop to ring 3 via iretq.
    // iretq frame (popped in order): RIP, CS, RFLAGS, RSP, SS.
    // We push in reverse order so RIP ends up at the top.
    unsafe {
        core::arch::asm!(
            "push {ss}",        // SS  (user data selector, RPL=3)
            "push {usp}",       // RSP (user stack top)
            "push {rf}",        // RFLAGS (IF=1, IOPL=0)
            "push {cs}",        // CS  (user code selector, RPL=3)
            "push {ip}",        // RIP (user entry point)
            "iretq",
            ss  = in(reg) user_ss,
            usp = in(reg) USER_STACK_TOP,
            rf  = in(reg) 0x0202u64,
            cs  = in(reg) user_cs,
            ip  = in(reg) USER_CODE_VIRT,
            options(noreturn),
        );
    }
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