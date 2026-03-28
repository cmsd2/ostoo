use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write;

use super::{VfsDirEntry, VfsError};

// ---------------------------------------------------------------------------

pub struct ProcVfs;

impl ProcVfs {
    pub async fn list_dir(&self, path: &str) -> Result<Vec<VfsDirEntry>, VfsError> {
        match path {
            "/" => Ok(alloc::vec![
                VfsDirEntry { name: "cpuinfo".to_string(),  is_dir: false, size: 0 },
                VfsDirEntry { name: "drivers".to_string(),  is_dir: false, size: 0 },
                VfsDirEntry { name: "idt".to_string(),      is_dir: false, size: 0 },
                VfsDirEntry { name: "ioapic".to_string(),   is_dir: false, size: 0 },
                VfsDirEntry { name: "lapic".to_string(),    is_dir: false, size: 0 },
                VfsDirEntry { name: "maps".to_string(),     is_dir: false, size: 0 },
                VfsDirEntry { name: "meminfo".to_string(),  is_dir: false, size: 0 },
                VfsDirEntry { name: "memmap".to_string(),   is_dir: false, size: 0 },
                VfsDirEntry { name: "pci".to_string(),      is_dir: false, size: 0 },
                VfsDirEntry { name: "pmap".to_string(),     is_dir: false, size: 0 },
                VfsDirEntry { name: "tasks".to_string(),    is_dir: false, size: 0 },
                VfsDirEntry { name: "threads".to_string(),  is_dir: false, size: 0 },
                VfsDirEntry { name: "uptime".to_string(),   is_dir: false, size: 0 },
            ]),
            _ => Err(VfsError::NotFound),
        }
    }

    pub async fn read_file(&self, path: &str, caller_pid: libkernel::process::ProcessId) -> Result<Vec<u8>, VfsError> {
        match path {
            "/tasks"   => Ok(gen_tasks().into_bytes()),
            "/uptime"  => Ok(gen_uptime().into_bytes()),
            "/drivers" => Ok(gen_drivers().into_bytes()),
            "/threads" => Ok(gen_threads().into_bytes()),
            "/meminfo" => Ok(gen_meminfo().into_bytes()),
            "/memmap"  => Ok(gen_memmap().into_bytes()),
            "/cpuinfo" => Ok(gen_cpuinfo().into_bytes()),
            "/pmap"    => Ok(gen_pmap().into_bytes()),
            "/idt"     => Ok(gen_idt().into_bytes()),
            "/pci"     => Ok(gen_pci().into_bytes()),
            "/lapic"   => Ok(gen_lapic().into_bytes()),
            "/maps"    => Ok(gen_maps(caller_pid).into_bytes()),
            "/ioapic"  => Ok(gen_ioapic().into_bytes()),
            _ => Err(VfsError::NotFound),
        }
    }
}

// ---------------------------------------------------------------------------
// Existing generators

fn gen_tasks() -> String {
    let ready   = libkernel::task::executor::ready_count();
    let waiting = libkernel::task::executor::wait_count();
    alloc::format!("ready: {}  waiting: {}\n", ready, waiting)
}

fn gen_uptime() -> String {
    let secs = libkernel::task::timer::ticks() / libkernel::task::timer::TICKS_PER_SECOND;
    alloc::format!("{}s\n", secs)
}

fn gen_drivers() -> String {
    let mut s = String::new();
    crate::driver::with_drivers(|name, state| {
        s.push_str(name);
        s.push_str("  ");
        s.push_str(state.as_str());
        s.push('\n');
    });
    s
}

// ---------------------------------------------------------------------------
// threads

fn gen_threads() -> String {
    alloc::format!(
        "current thread: {}  context switches: {}\n",
        libkernel::task::scheduler::current_thread_idx(),
        libkernel::task::scheduler::context_switches()
    )
}

// ---------------------------------------------------------------------------
// meminfo

/// Well-known virtual address where the Local APIC is mapped.
const APIC_VIRT_BASE: u64 = 0xFFFF_8001_0000_0000;

fn gen_meminfo() -> String {
    let mut s = String::new();

    // Heap
    let (heap_used, heap_free) = libkernel::allocator::heap_stats();
    let heap_total = heap_used + heap_free;
    let _ = writeln!(s, "Heap: {} used  {} free  ({} KiB total)",
        heap_used, heap_free, heap_total / 1024);

    // Frame allocator
    let (frames_alloc, frames_total) = libkernel::memory::with_memory(|m| m.frame_stats());
    let _ = writeln!(s, "Frames: {} allocated / {} usable ({} MiB usable)",
        frames_alloc, frames_total, frames_total as u64 * 4 / 1024);

    // Known virtual regions
    let _ = writeln!(s, "Known virtual regions:");
    let _ = writeln!(s, "  {:#018x}  Heap ({} KiB)",
        libkernel::allocator::HEAP_START,
        libkernel::allocator::HEAP_SIZE / 1024);
    let _ = writeln!(s, "  {:#018x}  Local APIC registers", APIC_VIRT_BASE);
    let phys_off = libkernel::memory::with_memory(|m| m.phys_mem_offset().as_u64());
    let _ = writeln!(s, "  {:#018x}  Physical memory identity map", phys_off);

    s
}

// ---------------------------------------------------------------------------
// memmap

fn gen_memmap() -> String {
    use bootloader::bootinfo::MemoryRegionType;

    let mut s = String::new();
    let _ = writeln!(s, "Physical memory map:");
    let mut total_usable: u64 = 0;

    libkernel::memory::with_memory(|mem| {
        for r in mem.iter_memory_regions() {
            let start = r.range.start_addr();
            let end   = r.range.end_addr();
            let kib   = (end - start + 1023) / 1024;
            if r.region_type == MemoryRegionType::Usable {
                total_usable += end - start;
            }
            let _ = writeln!(s, "  {:#011x}-{:#011x}  {:22?}  {} KiB",
                start, end.saturating_sub(1), r.region_type, kib);
        }
    });

    let _ = writeln!(s, "  Usable total: {} KiB ({} MiB)",
        total_usable / 1024, total_usable / 1024 / 1024);

    s
}

// ---------------------------------------------------------------------------
// cpuinfo

fn gen_cpuinfo() -> String {
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

// ---------------------------------------------------------------------------
// pmap — walk the active page tables, coalescing contiguous regions

fn gen_pmap() -> String {
    use x86_64::registers::control::Cr3;
    use x86_64::structures::paging::{PageTable, PageTableFlags as F};

    let mut s = String::new();
    let phys_off = libkernel::memory::with_memory(|m| m.phys_mem_offset().as_u64());

    let (pml4_frame, _) = Cr3::read();
    let cr3_phys = pml4_frame.start_address().as_u64();

    let _ = writeln!(s, "Page table (CR3={:#x}):", cr3_phys);
    let _ = writeln!(s, "  {:18}  {:12}  {:6}  Flags", "Virtual", "Physical", "Size");

    let mut run_v    = 0u64;
    let mut run_p    = 0u64;
    let mut run_size = 0u64;
    let mut run_flags = F::empty();
    let mut line_count = 0usize;
    const MAX_LINES: usize = 100;

    let pml4: &PageTable = unsafe { &*((phys_off + cr3_phys) as *const PageTable) };

    'walk: for (i, pml4e) in pml4.iter().enumerate() {
        if !pml4e.flags().contains(F::PRESENT) {
            flush_run(&mut s, &mut run_v, &mut run_p, &mut run_size, &mut run_flags,
                      &mut line_count, MAX_LINES);
            if line_count >= MAX_LINES { break 'walk; }
            continue;
        }
        let va_pml4 = sign_extend((i as u64) << 39);
        let pdpt: &PageTable = unsafe {
            &*((phys_off + pml4e.addr().as_u64()) as *const PageTable)
        };

        for (j, pdpte) in pdpt.iter().enumerate() {
            if !pdpte.flags().contains(F::PRESENT) {
                flush_run(&mut s, &mut run_v, &mut run_p, &mut run_size, &mut run_flags,
                          &mut line_count, MAX_LINES);
                if line_count >= MAX_LINES { break 'walk; }
                continue;
            }
            let va_pdpt = va_pml4 + ((j as u64) << 30);

            if pdpte.flags().contains(F::HUGE_PAGE) {
                push_run(&mut s, &mut run_v, &mut run_p, &mut run_size, &mut run_flags,
                         &mut line_count, MAX_LINES,
                         va_pdpt, pdpte.addr().as_u64(), 1u64 << 30, pdpte.flags());
                if line_count >= MAX_LINES { break 'walk; }
                continue;
            }

            let pd: &PageTable = unsafe {
                &*((phys_off + pdpte.addr().as_u64()) as *const PageTable)
            };

            for (k, pde) in pd.iter().enumerate() {
                if !pde.flags().contains(F::PRESENT) {
                    flush_run(&mut s, &mut run_v, &mut run_p, &mut run_size, &mut run_flags,
                              &mut line_count, MAX_LINES);
                    if line_count >= MAX_LINES { break 'walk; }
                    continue;
                }
                let va_pd = va_pdpt + ((k as u64) << 21);
                let phys = pde.addr().as_u64();
                push_run(&mut s, &mut run_v, &mut run_p, &mut run_size, &mut run_flags,
                         &mut line_count, MAX_LINES,
                         va_pd, phys, 1u64 << 21, pde.flags());
                if line_count >= MAX_LINES { break 'walk; }
            }
        }
    }

    flush_run(&mut s, &mut run_v, &mut run_p, &mut run_size, &mut run_flags,
              &mut line_count, MAX_LINES);

    if line_count >= MAX_LINES {
        let _ = writeln!(s, "  (output truncated at {} entries)", MAX_LINES);
    } else {
        let _ = writeln!(s, "  {} region(s)", line_count);
    }

    s
}

fn push_run(
    s: &mut String,
    run_v: &mut u64, run_p: &mut u64, run_size: &mut u64,
    run_flags: &mut x86_64::structures::paging::PageTableFlags,
    line_count: &mut usize, max_lines: usize,
    virt: u64, phys: u64, size: u64,
    flags: x86_64::structures::paging::PageTableFlags,
) {
    use x86_64::structures::paging::PageTableFlags as F;
    let norm = flags & (F::PRESENT | F::WRITABLE | F::USER_ACCESSIBLE
                        | F::NO_EXECUTE | F::NO_CACHE);
    if *run_size > 0
        && virt == *run_v + *run_size
        && phys == *run_p + *run_size
        && norm == *run_flags
    {
        *run_size += size;
    } else {
        flush_run(s, run_v, run_p, run_size, run_flags, line_count, max_lines);
        *run_v     = virt;
        *run_p     = phys;
        *run_size  = size;
        *run_flags = norm;
    }
}

fn flush_run(
    s: &mut String,
    run_v: &mut u64, run_p: &mut u64, run_size: &mut u64,
    run_flags: &mut x86_64::structures::paging::PageTableFlags,
    line_count: &mut usize, max_lines: usize,
) {
    if *run_size == 0 { return; }
    if *line_count < max_lines {
        write_pmap_region(s, *run_v, *run_p, *run_size, *run_flags);
        *line_count += 1;
    }
    *run_size = 0;
}

fn write_pmap_region(
    s: &mut String,
    virt: u64, phys: u64, size: u64,
    flags: x86_64::structures::paging::PageTableFlags,
) {
    let (n, unit) = if size >= 1 << 30 { (size >> 30, 'G') }
                    else if size >= 1 << 20 { (size >> 20, 'M') }
                    else { (size >> 10, 'K') };
    let f = fmt_flags(flags);
    let _ = writeln!(s, "  {:#018x}  {:#012x}  {:4}{}  {}{}{}{}",
        virt, phys, n, unit,
        f[0] as char, f[1] as char, f[2] as char, f[3] as char);
}

fn fmt_flags(flags: x86_64::structures::paging::PageTableFlags) -> [u8; 4] {
    use x86_64::structures::paging::PageTableFlags as F;
    [
        b'R',
        if flags.contains(F::WRITABLE)         { b'W' } else { b'-' },
        if flags.contains(F::NO_EXECUTE)        { b'-' } else { b'X' },
        if flags.contains(F::USER_ACCESSIBLE)   { b'U' } else { b'K' },
    ]
}

fn sign_extend(addr: u64) -> u64 {
    if addr & (1 << 47) != 0 { addr | 0xffff_0000_0000_0000 } else { addr }
}

// ---------------------------------------------------------------------------
// maps — per-process memory map (like /proc/self/maps)

fn gen_maps(pid: libkernel::process::ProcessId) -> String {
    use libkernel::process;

    let mut s = String::new();
    if pid == process::ProcessId::KERNEL {
        let _ = writeln!(s, "(kernel — no user address space)");
        return s;
    }

    let info = process::with_process_ref(pid, |p| {
        (
            p.brk_base,
            p.brk_current,
            p.user_stack_top,
            p.vma_map.clone(),
        )
    });

    let Some((brk_base, brk_current, user_stack_top, vma_map)) = info else {
        let _ = writeln!(s, "(process not found)");
        return s;
    };

    // Heap (brk region)
    if brk_current > brk_base {
        let _ = writeln!(s, "{:012x}-{:012x} rw-p 00000000 00:00 0  [heap]",
            brk_base, brk_current);
    }

    // mmap regions — BTreeMap is already sorted by start address
    for vma in vma_map.values() {
        let r = if vma.prot & process::PROT_READ  != 0 { 'r' } else { '-' };
        let w = if vma.prot & process::PROT_WRITE != 0 { 'w' } else { '-' };
        let x = if vma.prot & process::PROT_EXEC  != 0 { 'x' } else { '-' };
        let p = if vma.flags & process::MAP_PRIVATE != 0 { 'p' } else { 's' };
        let _ = writeln!(s, "{:012x}-{:012x} {}{}{}{} 00000000 00:00 0",
            vma.start, vma.start + vma.len, r, w, x, p);
    }

    // User stack — grows down, so the mapped region ends at user_stack_top.
    // The stack size is 8 pages (32 KiB) as set in osl::spawn / osl::exec.
    const STACK_SIZE: u64 = 8 * 0x1000;
    if user_stack_top > STACK_SIZE {
        let stack_base = user_stack_top - STACK_SIZE;
        let _ = writeln!(s, "{:012x}-{:012x} rw-p 00000000 00:00 0  [stack]",
            stack_base, user_stack_top);
    }

    s
}

// ---------------------------------------------------------------------------
// idt

fn gen_idt() -> String {
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

// ---------------------------------------------------------------------------
// pci

fn gen_pci() -> String {
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

// ---------------------------------------------------------------------------
// lapic

fn gen_lapic() -> String {
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

// ---------------------------------------------------------------------------
// ioapic

fn gen_ioapic() -> String {
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
