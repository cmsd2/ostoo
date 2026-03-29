use alloc::string::String;
use core::fmt::Write;

pub(super) fn generate() -> String {
    let mut s = String::new();
    let devs = crate::pci::PCI_DEVICES.lock();
    let _ = writeln!(s, "PCI devices ({}):", devs.len());
    let _ = writeln!(s, "  Bus:Dev.Fn  Vendor  Device  Rev  Class     Description");
    for d in devs.iter() {
        let _ = writeln!(s, "  {:02x}:{:02x}.{}   {:04x}    {:04x}   {:02x}   {:02x}:{:02x}    {}",
            d.bus, d.device, d.function,
            d.vendor_id, d.device_id, d.revision,
            d.class, d.subclass,
            crate::pci::class_name(d.class, d.subclass));
    }
    s
}
