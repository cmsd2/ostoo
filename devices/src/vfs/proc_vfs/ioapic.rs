use alloc::string::String;
use core::fmt::Write;

pub(super) fn generate() -> String {
    let mut s = String::new();
    let io_apics = libkernel::apic::IO_APICS.lock();
    if io_apics.is_empty() {
        let _ = writeln!(s, "No IO APICs found");
        return s;
    }
    for ioapic in io_apics.iter() {
        let ver_raw = ioapic.read_version_raw();
        let (max_entries, ver) = ((ver_raw >> 16) as u8 + 1, ver_raw as u8);
        let _ = writeln!(s, "IO APIC {}:  gsi_base={}  version={:#04x}  entries={}",
            ioapic.id, ioapic.interrupt_base, ver, max_entries);
        let _ = writeln!(s, "  GSI  Flags    Vec   Delivery  Trigger  Polarity  Dest");
        for i in 0..max_entries as u32 {
            let entry = ioapic.read_redirect_entry(i);
            let vector    = (entry & 0xFF) as u8;
            let delivery  = (entry >> 8) & 0x7;
            let dest_mode = (entry >> 11) & 1;
            let polarity  = (entry >> 13) & 1;
            let trigger   = (entry >> 15) & 1;
            let masked    = (entry >> 16) & 1 != 0;
            let dest      = (entry >> 56) as u8;

            let delivery_str = match delivery {
                0 => "fixed",
                1 => "low-pri",
                2 => "SMI",
                4 => "NMI",
                5 => "INIT",
                7 => "ExtINT",
                _ => "?",
            };
            let _ = writeln!(s, "  {:3}  {:7}  {:#04x}  {:8}  {:5}    {:8}  {} ({})",
                ioapic.interrupt_base + i,
                if masked { "[MASKED]" } else { "" },
                vector,
                delivery_str,
                if trigger == 0 { "edge" } else { "level" },
                if polarity == 0 { "hi" } else { "lo" },
                dest,
                if dest_mode == 0 { "phys" } else { "log" });
        }
    }
    s
}
