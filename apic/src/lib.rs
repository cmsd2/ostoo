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
use acpi::platform::interrupt::{InterruptModel, IoApic as AcpiIoApic, InterruptSourceOverride, Polarity, TriggerMode};
use core::sync::atomic::{AtomicU32, Ordering};
use spin::Mutex;
use lazy_static::lazy_static;
use libkernel;
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

/// GSI that ISA IRQ 0 (PIT) was routed to, stored during init for later masking.
static TIMER_GSI: AtomicU32 = AtomicU32::new(u32::MAX);

lazy_static! {
    pub static ref IO_APICS: Mutex<Vec<MappedIoApic>> = Mutex::new(Vec::new());
}

pub fn init(interrupt_model: &InterruptModel, remap_addr: VirtAddr, mapper: &mut impl Mapper<Size4KiB>, frame_allocator: &mut impl FrameAllocator<Size4KiB>) {
    if let InterruptModel::Apic(apic_info) = interrupt_model {
        init_local(remap_addr, mapper, frame_allocator);
        init_io(&apic_info.io_apics, &apic_info.interrupt_source_overrides, remap_addr + Size4KiB::SIZE, mapper, frame_allocator);
    }
}

pub fn init_io(io_apics: &[AcpiIoApic], overrides: &[InterruptSourceOverride], mut remap_addr: VirtAddr, mapper: &mut impl Mapper<Size4KiB>, frame_allocator: &mut impl FrameAllocator<Size4KiB>) {
    let mapped_io_apics: Vec<MappedIoApic> = io_apics.iter().map(|io_apic| {
        let mapped_io_apic = init_io_apic(io_apic, remap_addr, mapper, frame_allocator);
        remap_addr = remap_addr + Size4KiB::SIZE;
        mapped_io_apic
    }).collect();

    // Mask all entries before programming
    for apic in &mapped_io_apics {
        unsafe { apic.mask_all(); }
    }

    // Route ISA IRQ 0 (timer) → vector 0x20, IRQ 1 (keyboard) → vector 0x21
    let lapic_id = unsafe {
        LOCAL_APIC.lock().as_ref().map(|a| a.id()).unwrap_or(0)
    };
    route_isa_irq(&mapped_io_apics, 0, 0x20, lapic_id, overrides);
    route_isa_irq(&mapped_io_apics, 1, 0x21, lapic_id, overrides);

    let mut io_apics_guard = IO_APICS.lock();
    *io_apics_guard = mapped_io_apics;
}

fn route_isa_irq(io_apics: &[MappedIoApic], isa_irq: u8, vector: u8, lapic_id: u8, overrides: &[InterruptSourceOverride]) {
    let ovr = overrides.iter().find(|o| o.isa_source == isa_irq);
    let gsi        = ovr.map_or(isa_irq as u32, |o| o.global_system_interrupt);
    let active_low = ovr.map_or(false, |o| matches!(o.polarity, Polarity::ActiveLow));
    let level_trig = ovr.map_or(false, |o| matches!(o.trigger_mode, TriggerMode::Level));

    for apic in io_apics {
        let max = unsafe { apic.max_redirect_entries() };
        let end_gsi = apic.interrupt_base + max + 1;
        if gsi >= apic.interrupt_base && gsi < end_gsi {
            let offset = gsi - apic.interrupt_base;
            unsafe { apic.set_irq(offset, vector, lapic_id, active_low, level_trig); }
            info!("[apic] isa_irq={} gsi={} vector={:#x} lapic={}", isa_irq, gsi, vector, lapic_id);
            if isa_irq == 0 {
                TIMER_GSI.store(gsi, Ordering::Relaxed);
            }
            return;
        }
    }
    warn!("[apic] route_isa_irq: no IO APIC found for gsi={}", gsi);
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

pub fn init_local(remap_addr: VirtAddr, mapper: &mut impl Mapper<Size4KiB>, frame_allocator: &mut impl FrameAllocator<Size4KiB>) {
    let phys_addr = unsafe {
        MappedLocalApic::get_base_phys_addr()
    };

    let (frame, page) = map(remap_addr, phys_addr, mapper, frame_allocator);

    info!("[apic] init mapped local apic from {:?} to {:?}", frame, page);

    let mapped_apic = MappedLocalApic::new(remap_addr);
    unsafe {
        mapped_apic.init();
        mapped_apic.enable();
    }

    // Register LAPIC EOI virtual address with libkernel (offset 0xB0 = EndOfInterrupt)
    libkernel::interrupts::set_local_apic_eoi_addr((remap_addr + 0xB0u64).as_u64());

    let mut local_apic = LOCAL_APIC.lock();
    *local_apic = Some(mapped_apic);
}

const CALIBRATION_PIT_TICKS: u64 = 50;  // 50 × 10ms = 500ms window
const CALIBRATION_PIT_HZ:    u64 = 100;
const LAPIC_TARGET_HZ:       u64 = 1000;
const LAPIC_DIVIDE_BY_16:    u8  = 0x3;

/// Calibrate the LAPIC timer against the PIT and start it in periodic mode at 1000 Hz.
/// Must be called after `init()` and with interrupts enabled (PIT drives TICK_COUNT during calibration).
pub fn calibrate_and_start_lapic_timer() {
    use libkernel::{interrupts::LAPIC_TIMER_VECTOR, task::timer};

    info!("[apic] calibrating LAPIC timer against PIT ({}ms window)...",
        CALIBRATION_PIT_TICKS * 1000 / CALIBRATION_PIT_HZ);

    // Phase 1: start one-shot countdown. Release lock before entering HLT loop.
    {
        let guard = LOCAL_APIC.lock();
        let apic = guard.as_ref().expect("LOCAL_APIC not initialised");
        unsafe {
            apic.start_oneshot_timer(0xFFFF_FFFF, LAPIC_DIVIDE_BY_16, LAPIC_TIMER_VECTOR);
        }
    }

    // Phase 2: busy-wait 500ms (PIT drives TICK_COUNT via timer ISR during this window).
    timer::wait_ticks(CALIBRATION_PIT_TICKS);

    // Phase 3: read elapsed count, compute bus frequency, start periodic.
    let guard = LOCAL_APIC.lock();
    let apic = guard.as_ref().unwrap();
    unsafe {
        let remaining = apic.read_current_count();
        let elapsed = 0xFFFF_FFFFu64 - remaining as u64;
        apic.stop_timer();

        let lapic_bus_freq = elapsed * 16 * CALIBRATION_PIT_HZ / CALIBRATION_PIT_TICKS;
        let initial_count  = lapic_bus_freq / (16 * LAPIC_TARGET_HZ);
        assert!(initial_count > 0 && initial_count <= 0xFFFF_FFFF,
            "LAPIC calibration out of range: {}", initial_count);

        info!("[apic] LAPIC bus {} MHz, timer initial_count={} ({}Hz)",
            lapic_bus_freq / 1_000_000, initial_count, LAPIC_TARGET_HZ);

        apic.start_periodic_timer(initial_count as u32, LAPIC_DIVIDE_BY_16, LAPIC_TIMER_VECTOR);
    }

    // Mask the PIT's IO APIC redirection entry so it no longer fires.
    let timer_gsi = TIMER_GSI.load(Ordering::Relaxed);
    if timer_gsi != u32::MAX {
        let io_apics = IO_APICS.lock();
        for apic in io_apics.iter() {
            let max = unsafe { apic.max_redirect_entries() };
            if timer_gsi >= apic.interrupt_base && timer_gsi <= apic.interrupt_base + max {
                let offset = timer_gsi - apic.interrupt_base;
                unsafe { apic.mask_entry(offset); }
                info!("[apic] masked PIT gsi={} in IO APIC", timer_gsi);
                break;
            }
        }
    }
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
