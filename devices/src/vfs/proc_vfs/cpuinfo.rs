use alloc::string::String;
use core::fmt::Write;

pub(super) fn generate() -> String {
    use x86_64::registers::control::{Cr0, Cr4};
    use x86_64::registers::model_specific::Efer;
    use x86_64::registers::rflags;

    let mut s = String::new();

    let family   = libkernel::cpuid::family().unwrap_or(0);
    let model    = libkernel::cpuid::model().unwrap_or(0);
    let stepping = libkernel::cpuid::stepping().unwrap_or(0);
    let mut vbuf = [0u8; 12];
    let vlen = libkernel::cpuid::vendor_into(&mut vbuf);
    let vendor = core::str::from_utf8(&vbuf[..vlen]).unwrap_or("?");
    let _ = writeln!(s, "CPU: {}  family={:#x} model={:#x} stepping={}",
        vendor, family, model, stepping);

    let cr0 = Cr0::read().bits();
    let _ = write!(s, "  CR0: {:#010x}", cr0);
    for (bit, name) in [(0, "PE"), (1, "MP"), (2, "EM"), (3, "TS"),
                        (5, "NE"), (16, "WP"), (31, "PG")] {
        if cr0 & (1 << bit) != 0 { let _ = write!(s, " {}", name); }
    }
    let _ = writeln!(s);

    let cr4 = Cr4::read().bits();
    let _ = write!(s, "  CR4: {:#010x}", cr4);
    for (bit, name) in [(5, "PAE"), (7, "PGE"), (9, "OSFXSR"),
                        (10, "OSXMMEXCPT"), (13, "VMXE"), (20, "SMEP")] {
        if cr4 & (1 << bit) != 0 { let _ = write!(s, " {}", name); }
    }
    let _ = writeln!(s);

    let efer = Efer::read().bits();
    let _ = write!(s, "  EFER:{:#010x}", efer);
    for (bit, name) in [(0, "SCE"), (8, "LME"), (10, "LMA"), (11, "NXE")] {
        if efer & (1 << bit) != 0 { let _ = write!(s, " {}", name); }
    }
    let _ = writeln!(s);

    let rf = rflags::read().bits();
    let _ = writeln!(s, "  RFLAGS: {:#018x}  IF={} IOPL={}",
        rf, (rf >> 9) & 1, (rf >> 12) & 3);

    s
}
