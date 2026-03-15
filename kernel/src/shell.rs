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
                execute(line);
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

fn execute(line: &str) {
    if line.is_empty() {
        return;
    }
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
