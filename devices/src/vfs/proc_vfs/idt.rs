use alloc::string::String;
use core::fmt::Write;

pub(super) fn generate() -> String {
    use libkernel::interrupts::{DYNAMIC_BASE, DYNAMIC_COUNT, LAPIC_TIMER_VECTOR,
                                PIC_1_OFFSET, PIC_2_OFFSET};

    let mut s = String::new();
    let _ = writeln!(s, "IDT vector assignments:");
    let _ = writeln!(s, "  0x00-0x1f  CPU exceptions");
    let _ = writeln!(s, "    0x03  Breakpoint         [handler]");
    let _ = writeln!(s, "    0x08  Double Fault       [handler, IST{}]",
        libkernel::gdt::DOUBLE_FAULT_IST_INDEX);
    let _ = writeln!(s, "    0x0e  Page Fault         [handler]");
    let _ = writeln!(s, "  PIC  (master offset={:#04x}, slave offset={:#04x})",
        PIC_1_OFFSET, PIC_2_OFFSET);
    let _ = writeln!(s, "    {:#04x}  PIT Timer          (IRQ 0)", PIC_1_OFFSET);
    let _ = writeln!(s, "    {:#04x}  PS/2 Keyboard      (IRQ 1)", PIC_1_OFFSET + 1);
    let _ = writeln!(s, "  LAPIC");
    let _ = writeln!(s, "    {:#04x}  Timer (preempt stub)", LAPIC_TIMER_VECTOR);
    let _ = writeln!(s, "    0xff  Spurious           [handler]");

    let mask = libkernel::interrupts::dynamic_slots_mask();
    let used = mask.count_ones();
    let _ = writeln!(s, "  Dynamic {:#04x}-{:#04x}  ({}/{} in use)",
        DYNAMIC_BASE, DYNAMIC_BASE + DYNAMIC_COUNT as u8 - 1,
        used, DYNAMIC_COUNT);
    if used > 0 {
        for i in 0..DYNAMIC_COUNT {
            if mask & (1 << i) != 0 {
                let _ = writeln!(s, "    {:#04x}  [in use]", DYNAMIC_BASE as usize + i);
            }
        }
    }

    s
}
