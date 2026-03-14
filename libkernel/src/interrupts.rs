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

    panic!("EXCEPTION: PAGE FAULT\nAccessed Address: {:?}\nError Code: {:?}\n{:#?}", Cr2::read(), error_code, stack_frame);
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
    // use x86_64::instructions::port::Port;
    // use pc_keyboard::{Keyboard, ScancodeSet1, DecodedKey, HandleControl, layouts};
    // use spin::Mutex;

    // lazy_static! {
    //     static ref KEYBOARD: Mutex<Keyboard<layouts::Us104Key, ScancodeSet1>> =
    //         Mutex::new(Keyboard::new(layouts::Us104Key, ScancodeSet1, HandleControl::Ignore));
    // }

    // let mut keyboard = KEYBOARD.lock();
    // let mut port = Port::new(0x60);

    // let scancode: u8 = unsafe { port.read() };
    // if let Ok(Some(key_event)) = keyboard.add_byte(scancode) {
    //     if let Some(key) = keyboard.process_keyevent(key_event) {
    //         match key {
    //             DecodedKey::Unicode(character) => print!("{}", character),
    //             DecodedKey::RawKey(key) => print!("{:?}", key),
    //         }
    //     }
    // }
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