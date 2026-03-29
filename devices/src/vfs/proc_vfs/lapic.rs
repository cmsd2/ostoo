use alloc::string::String;
use core::fmt::Write;

pub(super) fn generate() -> String {
    let mut s = String::new();
    let guard = libkernel::apic::LOCAL_APIC.lock();
    let Some(lapic) = guard.as_ref() else {
        let _ = writeln!(s, "Local APIC not initialised");
        return s;
    };
    let id       = lapic.id();
    let phys     = unsafe { libkernel::apic::local_apic::MappedLocalApic::get_base_phys_addr() };
    let enabled  = lapic.is_global_enabled();
    let ver_raw  = lapic.read_version_raw();
    let ver_byte = ver_raw as u8;
    let max_lvt  = (ver_raw >> 16) as u8 & 0xFF;

    let _ = writeln!(s, "Local APIC:");
    let _ = writeln!(s, "  ID: {}  phys={:#x}  globally enabled: {}",
        id, phys.as_u64(), enabled);
    let _ = writeln!(s, "  Version: {:#04x}  Max LVT: {}", ver_byte, max_lvt);

    let lvt   = lapic.read_lvt_timer();
    let vector = lvt as u8;
    let masked = (lvt >> 16) & 1 != 0;
    let mode   = match (lvt >> 17) & 3 {
        0 => "one-shot",
        1 => "periodic",
        2 => "TSC-deadline",
        _ => "unknown",
    };
    let init_cnt = lapic.read_timer_initial_count();
    let curr_cnt = lapic.read_current_count();
    let _ = writeln!(s, "  Timer: {}  vec={:#04x}  {}  initial={} current={}",
        mode, vector, if masked { "[MASKED]" } else { "" },
        init_cnt, curr_cnt);
    s
}
