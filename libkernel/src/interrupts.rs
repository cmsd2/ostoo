use core::sync::atomic::{AtomicU64, Ordering};
use lazy_static::lazy_static;
use spin;
use x86_64::structures::idt::{
    InterruptDescriptorTable,
    InterruptStackFrame,
    PageFaultErrorCode,
};
use pic8259::ChainedPics;
use crate::{gdt, println, task};

static LAPIC_EOI_ADDR: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// Dynamic interrupt handler table (vectors 0x40 – 0x4F)

/// First vector in the dynamic range.
pub const DYNAMIC_BASE: u8 = 0x40;
/// Number of dynamically allocatable vectors.
pub const DYNAMIC_COUNT: usize = 16;

static DYNAMIC_HANDLERS: spin::Mutex<[Option<fn()>; DYNAMIC_COUNT]> =
    spin::Mutex::new([None; DYNAMIC_COUNT]);

fn dispatch_dynamic(idx: usize) {
    // Copy the fn pointer out before calling so the lock is released first.
    let handler = DYNAMIC_HANDLERS.lock()[idx];
    if let Some(f) = handler {
        f();
    }
    send_eoi(DYNAMIC_BASE + idx as u8);
}

macro_rules! dyn_handler {
    ($name:ident, $idx:literal) => {
        extern "x86-interrupt" fn $name(_frame: InterruptStackFrame) {
            dispatch_dynamic($idx);
        }
    };
}

dyn_handler!(dyn_handler_0,   0); dyn_handler!(dyn_handler_1,   1);
dyn_handler!(dyn_handler_2,   2); dyn_handler!(dyn_handler_3,   3);
dyn_handler!(dyn_handler_4,   4); dyn_handler!(dyn_handler_5,   5);
dyn_handler!(dyn_handler_6,   6); dyn_handler!(dyn_handler_7,   7);
dyn_handler!(dyn_handler_8,   8); dyn_handler!(dyn_handler_9,   9);
dyn_handler!(dyn_handler_10, 10); dyn_handler!(dyn_handler_11, 11);
dyn_handler!(dyn_handler_12, 12); dyn_handler!(dyn_handler_13, 13);
dyn_handler!(dyn_handler_14, 14); dyn_handler!(dyn_handler_15, 15);

type IrqTrampoline = extern "x86-interrupt" fn(InterruptStackFrame);
const DYN_TRAMPOLINES: [IrqTrampoline; DYNAMIC_COUNT] = [
    dyn_handler_0,  dyn_handler_1,  dyn_handler_2,  dyn_handler_3,
    dyn_handler_4,  dyn_handler_5,  dyn_handler_6,  dyn_handler_7,
    dyn_handler_8,  dyn_handler_9,  dyn_handler_10, dyn_handler_11,
    dyn_handler_12, dyn_handler_13, dyn_handler_14, dyn_handler_15,
];

/// Register an interrupt handler for the next free dynamic vector (0x40–0x4F).
///
/// Returns the assigned vector number, or `None` if all 16 are in use.
/// Safe to call from any kernel thread; disables interrupts while updating
/// the table to avoid deadlock with the ISR dispatcher.
pub fn register_handler(handler: fn()) -> Option<u8> {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut handlers = DYNAMIC_HANDLERS.lock();
        for (i, slot) in handlers.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(handler);
                return Some(DYNAMIC_BASE + i as u8);
            }
        }
        None
    })
}

/// Returns a bitmask of which dynamic vector slots (0x40–0x4F) are in use.
/// Bit `i` is set when vector `DYNAMIC_BASE + i` has a registered handler.
pub fn dynamic_slots_mask() -> u16 {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let handlers = DYNAMIC_HANDLERS.lock();
        let mut mask = 0u16;
        for (i, slot) in handlers.iter().enumerate() {
            if slot.is_some() {
                mask |= 1 << i;
            }
        }
        mask
    })
}

/// Release a previously assigned dynamic interrupt vector.
pub fn free_vector(vector: u8) {
    if (vector < DYNAMIC_BASE)
        || (vector as usize >= DYNAMIC_BASE as usize + DYNAMIC_COUNT)
    {
        return;
    }
    x86_64::instructions::interrupts::without_interrupts(|| {
        DYNAMIC_HANDLERS.lock()[(vector - DYNAMIC_BASE) as usize] = None;
    });
}

pub fn set_local_apic_eoi_addr(addr: u64) {
    LAPIC_EOI_ADDR.store(addr, Ordering::Relaxed);
}

pub fn disable_pic() {
    unsafe {
        use x86_64::instructions::port::Port;
        let mut master: Port<u8> = Port::new(0x21);
        let mut slave: Port<u8> = Port::new(0xA1);
        master.write(0xFF);
        slave.write(0xFF);
    }
}

/// Send an EOI for the LAPIC timer vector.  Called from `preempt_tick` in the
/// scheduler, which runs inside the timer ISR with interrupts already disabled.
pub(crate) fn lapic_eoi() {
    send_eoi(LAPIC_TIMER_VECTOR);
}

fn send_eoi(pic_vector: u8) {
    let addr = LAPIC_EOI_ADDR.load(Ordering::Relaxed);
    unsafe {
        if addr != 0 {
            *(addr as *mut u32) = 0;
        } else {
            PICS.lock().notify_end_of_interrupt(pic_vector);
        }
    }
}

pub const PIC_1_OFFSET: u8 = 32;
pub const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;

pub const LAPIC_TIMER_VECTOR: u8 = 0x30;

pub static PICS: spin::Mutex<ChainedPics> =
    spin::Mutex::new(unsafe { ChainedPics::new(PIC_1_OFFSET, PIC_2_OFFSET) });

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum InterruptIndex {
    Timer = PIC_1_OFFSET,
    Keyboard,
}

impl InterruptIndex {
    fn as_u8(self) -> u8 {
        self as u8
    }

    #[allow(dead_code)]
    fn as_usize(self) -> usize {
        usize::from(self.as_u8())
    }
}

extern "C" {
    /// Assembly context-switch stub defined in `task/scheduler.rs` via
    /// `global_asm!`.  Registered directly in the IDT so the CPU jumps
    /// straight to it without the `extern "x86-interrupt"` wrapper overhead.
    fn lapic_timer_stub();
}

lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();
        idt.breakpoint.set_handler_fn(breakpoint_handler);
        idt.invalid_opcode.set_handler_fn(invalid_opcode_handler);
        idt.general_protection_fault.set_handler_fn(general_protection_fault_handler);
        idt.stack_segment_fault.set_handler_fn(stack_segment_fault_handler);
        idt.page_fault.set_handler_fn(page_fault_handler);
        unsafe {
            idt.double_fault.set_handler_fn(double_fault_handler)
                .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
            // Register the raw assembly stub directly so it can manipulate
            // RSP for context switching before/after iretq.
            idt[LAPIC_TIMER_VECTOR]
                .set_handler_addr(x86_64::VirtAddr::new(lapic_timer_stub as *const () as usize as u64));
        }
        idt[InterruptIndex::Timer.as_u8()]
            .set_handler_fn(timer_interrupt_handler);
        idt[InterruptIndex::Keyboard.as_u8()]
            .set_handler_fn(keyboard_interrupt_handler);
        idt[0xFF_u8].set_handler_fn(spurious_interrupt_handler);
        // Dynamic vectors 0x40–0x4F
        for (i, &trampoline) in DYN_TRAMPOLINES.iter().enumerate() {
            idt[DYNAMIC_BASE + i as u8].set_handler_fn(trampoline);
        }
        idt
    };
}

pub fn init() {
    init_idt();
    init_pics();
}

fn init_idt() {
    IDT.load();
}

fn init_pics() {
    unsafe { PICS.lock().initialize(); }
    configure_pit_100hz();
}

/// Configure PIT channel 0 to fire at 100 Hz (reload value = 11932).
/// At 100 ticks/s, 100 ticks = 1 second.
fn configure_pit_100hz() {
    use x86_64::instructions::port::Port;
    // PIT clock = 1,193,182 Hz; reload = 1,193,182 / 100 = 11,932
    const RELOAD: u16 = 11932;
    unsafe {
        let mut cmd: Port<u8> = Port::new(0x43);
        let mut data: Port<u8> = Port::new(0x40);
        cmd.write(0x34); // channel 0, lo/hi byte, mode 2 (rate generator), binary
        data.write((RELOAD & 0xFF) as u8);
        data.write((RELOAD >> 8) as u8);
    }
}

extern "x86-interrupt" fn breakpoint_handler(
    stack_frame: InterruptStackFrame)
{
    println!("EXCEPTION: BREAKPOINT\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn invalid_opcode_handler(
    stack_frame: InterruptStackFrame)
{
    if stack_frame.code_segment.rpl() == x86_64::PrivilegeLevel::Ring3 {
        error!("ring-3 invalid opcode — halting (TODO: kill process)\n{:#?}", stack_frame);
        crate::hlt_loop();
    }
    panic!("EXCEPTION: INVALID OPCODE\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn general_protection_fault_handler(
    stack_frame: InterruptStackFrame, error_code: u64)
{
    if stack_frame.code_segment.rpl() == x86_64::PrivilegeLevel::Ring3 {
        error!(
            "ring-3 general protection fault (error={:#x}) — halting (TODO: kill process)\n{:#?}",
            error_code, stack_frame
        );
        crate::hlt_loop();
    }
    panic!(
        "EXCEPTION: GENERAL PROTECTION FAULT (error={:#x})\n{:#?}",
        error_code, stack_frame
    );
}

extern "x86-interrupt" fn stack_segment_fault_handler(
    stack_frame: InterruptStackFrame, error_code: u64)
{
    panic!(
        "EXCEPTION: STACK SEGMENT FAULT (error={:#x})\n{:#?}",
        error_code, stack_frame
    );
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame, _error_code: u64) -> !
{
    panic!("EXCEPTION: DOUBLE FAULT\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    use x86_64::registers::control::Cr2;

    let faulting_addr = Cr2::read();

    // Check whether the fault came from ring 3 (RPL field of the saved CS).
    if stack_frame.code_segment.rpl() == x86_64::PrivilegeLevel::Ring3 {
        // Ring-3 fault: log and halt.
        // Phase 3 will replace this with process termination + rescheduling.
        error!(
            "ring-3 page fault at {:?} (error: {:?}) — halting (TODO: kill process)",
            faulting_addr, error_code
        );
        crate::hlt_loop();
    }

    panic!(
        "EXCEPTION: PAGE FAULT\nAccessed Address: {:?}\nError Code: {:?}\n{:#?}",
        faulting_addr, error_code, stack_frame
    );
}

extern "x86-interrupt" fn timer_interrupt_handler(
    _stack_frame: InterruptStackFrame)
{
    task::timer::tick();
    send_eoi(InterruptIndex::Timer.as_u8());
}

extern "x86-interrupt" fn keyboard_interrupt_handler(
    _stack_frame: InterruptStackFrame)
{
    use x86_64::instructions::port::Port;
    let mut port = Port::new(0x60);
    let scancode: u8 = unsafe { port.read() };
    crate::task::keyboard::add_scancode(scancode);
    send_eoi(InterruptIndex::Keyboard.as_u8());
}

extern "x86-interrupt" fn spurious_interrupt_handler(
    _stack_frame: InterruptStackFrame)
{
    // Spurious LAPIC interrupts must not receive an EOI
}

#[cfg(test)]
mod test {
    use crate::{serial_print, serial_println};

    #[test_case]
    fn test_breakpoint_exception() {
        serial_print!("test_breakpoint_exception...");
        // invoke a breakpoint exception
        x86_64::instructions::interrupts::int3();
        serial_println!("[ok]");
    }
}