use alloc::string::String;
use core::fmt::Write;

pub(super) fn generate() -> String {
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
