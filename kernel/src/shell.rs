use futures_util::stream::StreamExt;
use libkernel::task::keyboard::{Key, KeyStream};
use libkernel::task::{executor, scheduler, timer};
use libkernel::{print, println};

const PROMPT: &str = "ostoo> ";
/// Maximum input characters; keeps typed text on a single VGA row.
const MAX_LINE: usize = 80 - 7 - 1; // 80 cols − len("ostoo> ") − safety margin

pub async fn run() {
    println!();
    print!("{}", PROMPT);

    let mut keys = KeyStream::new();
    let mut buf = [0u8; MAX_LINE];
    let mut len = 0usize;

    while let Some(key) = keys.next().await {
        match key {
            // Enter — run whatever is in the buffer
            Key::Unicode('\n') | Key::Unicode('\r') => {
                println!();
                let line = core::str::from_utf8(&buf[..len]).unwrap_or("").trim();
                execute(line, &mut keys).await;
                len = 0;
                print!("{}", PROMPT);
            }

            // Backspace
            Key::Unicode('\x08') => {
                if len > 0 {
                    len -= 1;
                    libkernel::vga_buffer::backspace();
                }
            }

            // Printable ASCII only
            Key::Unicode(c) if c.is_ascii() && !c.is_control() => {
                if len < MAX_LINE {
                    buf[len] = c as u8;
                    len += 1;
                    print!("{}", c);
                }
            }

            // Ignore raw keys (arrows, F-keys, etc.)
            _ => {}
        }
    }
}

/// Lines of content shown per page (23-row content area minus the prompt row).
const PAGE_HEIGHT: usize = 20;

/// Capture command output, then display it page-by-page if it exceeds
/// `PAGE_HEIGHT` lines.  The caller's `KeyStream` is borrowed so the pager
/// can read keypresses without creating a competing waker.
async fn execute(line: &str, keys: &mut KeyStream) {
    if line.is_empty() { return; }
    libkernel::vga_buffer::capture_start();
    run_command(line);
    let n = libkernel::vga_buffer::capture_end();
    page_output(n, keys).await;
}

/// Show captured lines with an interactive pager.
/// Displays `PAGE_HEIGHT` lines at a time; press Space/Enter to advance or
/// `q` to quit early.
async fn page_output(n: usize, keys: &mut KeyStream) {
    if n == 0 { return; }
    let mut start = 0usize;
    loop {
        let end = (start + PAGE_HEIGHT).min(n);
        for i in start..end {
            libkernel::vga_buffer::capture_print_line(i);
        }
        start = end;
        if start >= n { break; }
        print!("-- More -- ({}/{}) [space/enter=next  q=quit]", start, n);
        loop {
            match keys.next().await {
                Some(Key::Unicode(' '))
                | Some(Key::Unicode('\n'))
                | Some(Key::Unicode('\r')) => {
                    libkernel::vga_buffer::clear_current_line();
                    break;
                }
                Some(Key::Unicode('q')) | Some(Key::Unicode('Q')) => {
                    libkernel::vga_buffer::clear_current_line();
                    return;
                }
                _ => {}
            }
        }
    }
}

fn run_command(line: &str) {
    let (cmd, rest) = match line.find(' ') {
        Some(i) => (&line[..i], line[i + 1..].trim()),
        None => (line, ""),
    };
    match cmd {
        "help" => {
            println!("Commands:");
            println!("  help              show this message");
            println!("  clear             clear the screen");
            println!("  uptime            seconds since boot");
            println!("  tasks             ready / waiting task counts");
            println!("  threads           current thread and context-switch count");
            println!("  echo <text>       print text back");
            println!("  memmap            physical memory regions (bootloader map)");
            println!("  meminfo           heap usage, frame stats, virtual layout");
            println!("  pmap              page table walk (coalesced 2 MiB view)");
            println!("  cpuinfo           CPU vendor, family/model, control registers");
            println!("  lapic             Local APIC state and timer configuration");
            println!("  ioapic            IO APIC redirection table");
            println!("  idt               IDT vector assignments");
            println!("  pci               list PCI devices");
            println!("  drivers           list registered device drivers");
            println!("  driver start <n>  start a driver by name");
            println!("  driver stop <n>   stop a driver by name");
            println!("  driver info <n>   show driver state and details");
        }
        "clear" => {
            libkernel::vga_buffer::clear_content();
        }
        "uptime" => {
            let secs = timer::ticks() / timer::TICKS_PER_SECOND;
            println!("uptime: {}s", secs);
        }
        "tasks" => {
            println!(
                "ready: {}  waiting: {}",
                executor::ready_count(),
                executor::wait_count()
            );
        }
        "threads" => {
            println!(
                "current thread: {}  context switches: {}",
                scheduler::current_thread_idx(),
                scheduler::context_switches()
            );
        }
        "echo" => {
            println!("{}", rest);
        }
        "memmap" => cmd_memmap(),
        "meminfo" => cmd_meminfo(),
        "pmap" => cmd_pmap(),
        "cpuinfo" => cmd_cpuinfo(),
        "lapic" => cmd_lapic(),
        "ioapic" => cmd_ioapic(),
        "idt" => cmd_idt(),
        "pci"     => cmd_pci(),
        "drivers" => cmd_drivers(),
        "driver"  => cmd_driver(rest),
        other => {
            println!("unknown command: '{}'  (try 'help')", other);
        }
    }
}

// ---------------------------------------------------------------------------
// memmap — physical memory regions

fn cmd_memmap() {
    use bootloader::bootinfo::MemoryRegionType;

    println!("Physical memory map:");
    let mut total_usable: u64 = 0;

    libkernel::memory::with_memory(|mem| {
        for r in mem.iter_memory_regions() {
            let start = r.range.start_addr();
            let end   = r.range.end_addr();
            let kib   = (end - start + 1023) / 1024;
            if r.region_type == MemoryRegionType::Usable {
                total_usable += end - start;
            }
            println!("  {:#011x}-{:#011x}  {:22?}  {} KiB",
                start, end.saturating_sub(1), r.region_type, kib);
        }
    });

    println!("  Usable total: {} KiB ({} MiB)",
        total_usable / 1024, total_usable / 1024 / 1024);
}

// ---------------------------------------------------------------------------
// meminfo — heap usage, frame stats, known virtual regions

fn cmd_meminfo() {
    // Heap
    let (heap_used, heap_free) = libkernel::allocator::heap_stats();
    let heap_total = heap_used + heap_free;
    println!("Heap: {} used  {} free  ({} KiB total)",
        heap_used, heap_free, heap_total / 1024);

    // Frame allocator
    let (frames_alloc, frames_total) = libkernel::memory::with_memory(|m| m.frame_stats());
    println!("Frames: {} allocated / {} usable ({} MiB usable)",
        frames_alloc,
        frames_total,
        frames_total as u64 * 4 / 1024);

    // Known virtual regions
    println!("Known virtual regions:");
    println!("  {:#018x}  Heap ({} KiB)",
        libkernel::allocator::HEAP_START,
        libkernel::allocator::HEAP_SIZE / 1024);
    println!("  {:#018x}  Local APIC registers", crate::APIC_BASE);
    let phys_off = libkernel::memory::with_memory(|m| m.phys_mem_offset().as_u64());
    println!("  {:#018x}  Physical memory identity map", phys_off);
}

// ---------------------------------------------------------------------------
// pmap — walk the active page tables, coalescing contiguous regions

fn cmd_pmap() {
    use x86_64::registers::control::Cr3;
    use x86_64::structures::paging::{PageTable, PageTableFlags as F};

    let phys_off = libkernel::memory::with_memory(|m| m.phys_mem_offset().as_u64());

    // Safety: phys_off + frame_phys is a valid virtual address for any physical
    // frame, because the bootloader maps all physical memory at phys_off.
    let (pml4_frame, _) = Cr3::read();
    let cr3_phys = pml4_frame.start_address().as_u64();

    println!("Page table (CR3={:#x}):", cr3_phys);
    println!("  {:18}  {:12}  {:6}  Flags", "Virtual", "Physical", "Size");

    // State for run coalescing
    let mut run_v    = 0u64;
    let mut run_p    = 0u64;
    let mut run_size = 0u64;
    let mut run_flags = F::empty();
    let mut line_count = 0usize;
    const MAX_LINES: usize = 100;

    let pml4: &PageTable = unsafe { &*((phys_off + cr3_phys) as *const PageTable) };

    'walk: for (i, pml4e) in pml4.iter().enumerate() {
        if !pml4e.flags().contains(F::PRESENT) {
            flush_run(&mut run_v, &mut run_p, &mut run_size, &mut run_flags,
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
                flush_run(&mut run_v, &mut run_p, &mut run_size, &mut run_flags,
                          &mut line_count, MAX_LINES);
                if line_count >= MAX_LINES { break 'walk; }
                continue;
            }
            let va_pdpt = va_pml4 + ((j as u64) << 30);

            if pdpte.flags().contains(F::HUGE_PAGE) {
                // 1 GiB page
                push_run(&mut run_v, &mut run_p, &mut run_size, &mut run_flags,
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
                    flush_run(&mut run_v, &mut run_p, &mut run_size, &mut run_flags,
                              &mut line_count, MAX_LINES);
                    if line_count >= MAX_LINES { break 'walk; }
                    continue;
                }
                let va_pd = va_pdpt + ((k as u64) << 21);
                // Treat both huge-2M and sub-page PD entries as 2 MiB regions.
                let phys = pde.addr().as_u64();
                push_run(&mut run_v, &mut run_p, &mut run_size, &mut run_flags,
                         &mut line_count, MAX_LINES,
                         va_pd, phys, 1u64 << 21, pde.flags());
                if line_count >= MAX_LINES { break 'walk; }
            }
        }
    }

    flush_run(&mut run_v, &mut run_p, &mut run_size, &mut run_flags,
              &mut line_count, MAX_LINES);

    if line_count >= MAX_LINES {
        println!("  (output truncated at {} entries)", MAX_LINES);
    } else {
        println!("  {} region(s)", line_count);
    }
}

/// Try to extend the current run; if the new entry doesn't continue it, flush
/// and start a new run.
fn push_run(
    run_v: &mut u64, run_p: &mut u64, run_size: &mut u64,
    run_flags: &mut x86_64::structures::paging::PageTableFlags,
    line_count: &mut usize, max_lines: usize,
    virt: u64, phys: u64, size: u64,
    flags: x86_64::structures::paging::PageTableFlags,
) {
    use x86_64::structures::paging::PageTableFlags as F;
    // Normalise: only track these flag bits for coalescing.
    let norm = flags & (F::PRESENT | F::WRITABLE | F::USER_ACCESSIBLE
                        | F::NO_EXECUTE | F::NO_CACHE);

    if *run_size > 0
        && virt == *run_v + *run_size
        && phys == *run_p + *run_size
        && norm == *run_flags
    {
        *run_size += size;
    } else {
        flush_run(run_v, run_p, run_size, run_flags, line_count, max_lines);
        *run_v     = virt;
        *run_p     = phys;
        *run_size  = size;
        *run_flags = norm;
    }
}

fn flush_run(
    run_v: &mut u64, run_p: &mut u64, run_size: &mut u64,
    run_flags: &mut x86_64::structures::paging::PageTableFlags,
    line_count: &mut usize, max_lines: usize,
) {
    if *run_size == 0 { return; }
    if *line_count < max_lines {
        print_pmap_region(*run_v, *run_p, *run_size, *run_flags);
        *line_count += 1;
    }
    *run_size = 0;
}

fn print_pmap_region(
    virt: u64, phys: u64, size: u64,
    flags: x86_64::structures::paging::PageTableFlags,
) {
    let (n, unit) = if size >= 1 << 30 { (size >> 30, 'G') }
                    else if size >= 1 << 20 { (size >> 20, 'M') }
                    else { (size >> 10, 'K') };
    let f = fmt_flags(flags);
    println!("  {:#018x}  {:#012x}  {:4}{}  {}{}{}{}",
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

/// Sign-extend bit 47 of a virtual address to produce a canonical address.
fn sign_extend(addr: u64) -> u64 {
    if addr & (1 << 47) != 0 { addr | 0xffff_0000_0000_0000 } else { addr }
}

// ---------------------------------------------------------------------------
// cpuinfo — CPU identity and key control-register flags

fn cmd_cpuinfo() {
    use x86_64::registers::control::{Cr0, Cr4};
    use x86_64::registers::model_specific::Efer;
    use x86_64::registers::rflags;

    // CPUID
    let family   = libkernel::cpuid::family().unwrap_or(0);
    let model    = libkernel::cpuid::model().unwrap_or(0);
    let stepping = libkernel::cpuid::stepping().unwrap_or(0);
    let mut vbuf = [0u8; 12];
    let vlen = libkernel::cpuid::vendor_into(&mut vbuf);
    let vendor = core::str::from_utf8(&vbuf[..vlen]).unwrap_or("?");
    println!("CPU: {}  family={:#x} model={:#x} stepping={}", vendor, family, model, stepping);

    // CR0 — key protection/paging flags
    let cr0 = Cr0::read().bits();
    print!("  CR0: {:#010x}", cr0);
    for (bit, name) in [(0, "PE"), (1, "MP"), (2, "EM"), (3, "TS"),
                        (5, "NE"), (16, "WP"), (31, "PG")] {
        if cr0 & (1 << bit) != 0 { print!(" {}", name); }
    }
    println!();

    // CR4 — paging / extension flags
    let cr4 = Cr4::read().bits();
    print!("  CR4: {:#010x}", cr4);
    for (bit, name) in [(5, "PAE"), (7, "PGE"), (9, "OSFXSR"),
                        (10, "OSXMMEXCPT"), (13, "VMXE"), (20, "SMEP")] {
        if cr4 & (1 << bit) != 0 { print!(" {}", name); }
    }
    println!();

    // EFER MSR — long-mode / NX bits
    let efer = Efer::read().bits();
    print!("  EFER:{:#010x}", efer);
    for (bit, name) in [(0, "SCE"), (8, "LME"), (10, "LMA"), (11, "NXE")] {
        if efer & (1 << bit) != 0 { print!(" {}", name); }
    }
    println!();

    // RFLAGS — interrupt enable etc.
    let rf = rflags::read().bits();
    println!("  RFLAGS: {:#018x}  IF={} IOPL={}", rf,
        (rf >> 9) & 1, (rf >> 12) & 3);
}

// ---------------------------------------------------------------------------
// lapic — Local APIC state and timer configuration

fn cmd_lapic() {
    let guard = apic::LOCAL_APIC.lock();
    let Some(lapic) = guard.as_ref() else {
        println!("Local APIC not initialised");
        return;
    };
    unsafe {
        let id       = lapic.id();
        let phys     = apic::local_apic::MappedLocalApic::get_base_phys_addr();
        let enabled  = lapic.is_global_enabled();
        let ver_raw  = lapic.read_version_raw();
        let ver_byte = ver_raw as u8;
        let max_lvt  = (ver_raw >> 16) as u8 & 0xFF;

        println!("Local APIC:");
        println!("  ID: {}  phys={:#x}  globally enabled: {}",
            id, phys.as_u64(), enabled);
        println!("  Version: {:#04x}  Max LVT: {}",
            ver_byte, max_lvt);

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
        println!("  Timer: {}  vec={:#04x}  {}  initial={} current={}",
            mode, vector, if masked { "[MASKED]" } else { "" },
            init_cnt, curr_cnt);
    }
}

// ---------------------------------------------------------------------------
// ioapic — IO APIC redirection table

fn cmd_ioapic() {
    let io_apics = apic::IO_APICS.lock();
    if io_apics.is_empty() {
        println!("No IO APICs found");
        return;
    }
    for apic in io_apics.iter() {
        let (max_entries, ver) = unsafe {
            let ver_raw = apic.read_version_raw();
            ((ver_raw >> 16) as u8 + 1, ver_raw as u8)
        };
        println!("IO APIC {}:  gsi_base={}  version={:#04x}  entries={}",
            apic.id, apic.interrupt_base, ver, max_entries);
        println!("  GSI  Flags    Vec   Delivery  Trigger  Polarity  Dest");
        for i in 0..max_entries as u32 {
            let entry = unsafe { apic.read_redirect_entry(i) };
            let vector    = (entry & 0xFF) as u8;
            let delivery  = (entry >> 8) & 0x7;
            let dest_mode = (entry >> 11) & 1;  // 0=physical, 1=logical
            let polarity  = (entry >> 13) & 1;  // 0=high, 1=low
            let trigger   = (entry >> 15) & 1;  // 0=edge, 1=level
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
            println!("  {:3}  {:7}  {:#04x}  {:8}  {:5}    {:8}  {} ({})",
                apic.interrupt_base + i,
                if masked { "[MASKED]" } else { "" },
                vector,
                delivery_str,
                if trigger == 0 { "edge" } else { "level" },
                if polarity == 0 { "hi" } else { "lo" },
                dest,
                if dest_mode == 0 { "phys" } else { "log" });
        }
    }
}

// ---------------------------------------------------------------------------
// idt — IDT vector assignments

fn cmd_idt() {
    use libkernel::interrupts::{DYNAMIC_BASE, DYNAMIC_COUNT, LAPIC_TIMER_VECTOR,
                                PIC_1_OFFSET, PIC_2_OFFSET};

    println!("IDT vector assignments:");

    // CPU exceptions — just note which have handlers installed
    println!("  0x00-0x1f  CPU exceptions");
    println!("    0x03  Breakpoint         [handler]");
    println!("    0x08  Double Fault       [handler, IST{}]",
        libkernel::gdt::DOUBLE_FAULT_IST_INDEX);
    println!("    0x0e  Page Fault         [handler]");

    // PIC-routed IRQs
    println!("  PIC  (master offset={:#04x}, slave offset={:#04x})",
        PIC_1_OFFSET, PIC_2_OFFSET);
    println!("    {:#04x}  PIT Timer          (IRQ 0)", PIC_1_OFFSET);
    println!("    {:#04x}  PS/2 Keyboard      (IRQ 1)", PIC_1_OFFSET + 1);

    // LAPIC
    println!("  LAPIC");
    println!("    {:#04x}  Timer (preempt stub)", LAPIC_TIMER_VECTOR);
    println!("    0xff  Spurious           [handler]");

    // Dynamic range
    let mask = libkernel::interrupts::dynamic_slots_mask();
    let used = mask.count_ones();
    println!("  Dynamic {:#04x}-{:#04x}  ({}/{} in use)",
        DYNAMIC_BASE, DYNAMIC_BASE + DYNAMIC_COUNT as u8 - 1,
        used, DYNAMIC_COUNT);
    if used > 0 {
        for i in 0..DYNAMIC_COUNT {
            if mask & (1 << i) != 0 {
                println!("    {:#04x}  [in use]", DYNAMIC_BASE as usize + i);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// pci — list enumerated PCI devices

fn cmd_pci() {
    let devs = devices::pci::PCI_DEVICES.lock();
    println!("PCI devices ({}):", devs.len());
    println!("  Bus:Dev.Fn  Vendor  Device  Rev  Class     Description");
    for d in devs.iter() {
        println!("  {:02x}:{:02x}.{}   {:04x}    {:04x}   {:02x}   {:02x}:{:02x}    {}",
            d.bus, d.device, d.function,
            d.vendor_id, d.device_id, d.revision,
            d.class, d.subclass,
            devices::pci::class_name(d.class, d.subclass));
    }
}

// ---------------------------------------------------------------------------
// drivers — list registered device drivers

fn cmd_drivers() {
    println!("Drivers:");
    println!("  {:<16}  State", "Name");
    devices::driver::with_drivers(|name, state| {
        println!("  {:16}  {:?}", name, state);
    });
}

// ---------------------------------------------------------------------------
// driver — manage a driver lifecycle

fn cmd_driver(rest: &str) {
    let (subcmd, name) = match rest.find(' ') {
        Some(i) => (rest[..i].trim(), rest[i + 1..].trim()),
        None    => (rest.trim(), ""),
    };
    match subcmd {
        "start" => {
            if name.is_empty() {
                println!("usage: driver start <name>");
            } else {
                match devices::driver::start_driver(name) {
                    Ok(())   => println!("driver '{}' started", name),
                    Err(msg) => println!("error: {}", msg),
                }
            }
        }
        "stop" => {
            if name.is_empty() {
                println!("usage: driver stop <name>");
            } else {
                match devices::driver::stop_driver(name) {
                    Ok(())   => println!("driver '{}' stop requested", name),
                    Err(msg) => println!("error: {}", msg),
                }
            }
        }
        "info" => {
            if name.is_empty() {
                println!("usage: driver info <name>");
            } else {
                match devices::driver::with_driver_info(name, |k, v| {
                    println!("  {:<20}  {}", k, v);
                }) {
                    Ok(())   => {}
                    Err(msg) => println!("error: {}", msg),
                }
            }
        }
        _ => println!("usage: driver <start|stop|info> <name>"),
    }
}
