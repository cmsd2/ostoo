extern crate alloc;

use alloc::string::String;
use alloc::string::ToString;
use libkernel::task::{executor, scheduler, timer};
use libkernel::task::registry;
use libkernel::{print, println};

// ---------------------------------------------------------------------------
// Messages

pub enum ShellMsg {
    /// A complete line of input from the keyboard actor.
    KeyLine(String),
}

// ---------------------------------------------------------------------------
// Shell actor

pub struct Shell {
    cwd: spin::Mutex<String>,
}

impl Shell {
    pub fn new() -> Self {
        Shell { cwd: spin::Mutex::new("/".to_string()) }
    }
}

// ---------------------------------------------------------------------------
// Path helpers

/// Resolve `path` against `cwd`: absolute paths pass through; relative paths
/// are joined to `cwd`.  The result is then normalised (`.` and `..` removed).
fn resolve_path(cwd: &str, path: &str) -> String {
    let raw = if path.starts_with('/') {
        path.to_string()
    } else if path.is_empty() {
        cwd.to_string()
    } else {
        let mut base = cwd.to_string();
        if !base.ends_with('/') { base.push('/'); }
        base.push_str(path);
        base
    };
    normalize_path(&raw)
}

/// Collapse `.` and `..` components, return a canonical absolute path.
fn normalize_path(path: &str) -> String {
    let mut parts: alloc::vec::Vec<&str> = alloc::vec::Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => { parts.pop(); }
            s    => parts.push(s),
        }
    }
    if parts.is_empty() {
        "/".to_string()
    } else {
        let mut out = String::new();
        for p in &parts {
            out.push('/');
            out.push_str(p);
        }
        out
    }
}

#[devices::actor("shell", ShellMsg)]
impl Shell {
    // ── Startup ──────────────────────────────────────────────────────────
    #[on_start]
    async fn on_start(&self) {
        println!();
        self.print_prompt();
    }

    // ── Message handler ───────────────────────────────────────────────────
    #[on_message(KeyLine)]
    async fn on_key_line(&self, line: String) {
        self.execute_command(&line).await;
        self.print_prompt();
    }

    fn print_prompt(&self) {
        use libkernel::task::mailbox::ActorMsg;
        use crate::keyboard_actor::{KeyboardMsg, KeyboardInfo};

        let cwd = self.cwd.lock().clone();
        let mut prompt = String::from("ostoo:");
        prompt.push_str(&cwd);
        prompt.push_str("> ");

        // Notify the keyboard actor so it uses the correct column for cursor
        // positioning and can reprint the prompt on Ctrl+C / Ctrl+L.
        if let Some(kb) = libkernel::task::registry::get::<KeyboardMsg, KeyboardInfo>("keyboard") {
            kb.send(ActorMsg::Inner(KeyboardMsg::SetPrompt(prompt.clone())));
        }
        print!("{}", prompt);
    }

    // ── Command dispatch ──────────────────────────────────────────────────
    async fn execute_command(&self, line: &str) {
        let (cmd, rest) = match line.find(' ') {
            Some(i) => (&line[..i], line[i + 1..].trim()),
            None    => (line, ""),
        };
        match cmd {
            "help"    => cmd_help(),
            "clear"   => libkernel::vga_buffer::clear_content(),
            "uptime"  => {
                let secs = timer::ticks() / timer::TICKS_PER_SECOND;
                println!("uptime: {}s", secs);
            }
            "tasks"   => {
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
            "echo"    => println!("{}", rest),
            "memmap"  => cmd_memmap(),
            "meminfo" => cmd_meminfo(),
            "pmap"    => cmd_pmap(),
            "cpuinfo" => cmd_cpuinfo(),
            "lapic"   => cmd_lapic(),
            "ioapic"  => cmd_ioapic(),
            "idt"     => cmd_idt(),
            "pci"     => cmd_pci(),
            "drivers" => cmd_drivers(),
            "driver"  => self.cmd_driver(rest).await,
            "blk"     => self.cmd_blk(rest).await,
            "ls"      => self.cmd_blk_ls(rest).await,
            "cat"     => self.cmd_blk_cat(rest).await,
            "pwd"     => self.cmd_pwd(),
            "cd"      => self.cmd_cd(rest).await,
            "mount"   => self.cmd_mount(rest).await,
            other     => println!("unknown command: '{}'  (try 'help')", other),
        }
    }

    // ── blk command ───────────────────────────────────────────────────────────
    async fn cmd_blk(&self, rest: &str) {
        use libkernel::task::mailbox::ActorMsg;
        use devices::virtio::blk::{VirtioBlkMsg, VirtioBlkInfo};

        let (sub, arg) = match rest.find(' ') {
            Some(i) => (rest[..i].trim(), rest[i + 1..].trim()),
            None    => (rest.trim(), ""),
        };

        match sub {
            "ls"  => self.cmd_blk_ls(arg).await,
            "cat" => self.cmd_blk_cat(arg).await,
            "info" => {
                match libkernel::task::registry::ask_info("virtio-blk").await {
                    Some(s) => {
                        println!("  name:    {}", s.name);
                        println!("  running: {}", s.running);
                        println!("  info:    {:?}", s.info);
                    }
                    None => println!("virtio-blk: not found or not responding"),
                }
            }
            "read" => {
                let sector: u64 = match arg.parse() {
                    Ok(n)  => n,
                    Err(_) => { println!("usage: blk read <sector>"); return; }
                };
                let inbox = match libkernel::task::registry::get::<VirtioBlkMsg, VirtioBlkInfo>(
                    "virtio-blk"
                ) {
                    Some(mb) => mb,
                    None => { println!("virtio-blk: driver not found"); return; }
                };
                let result: Option<Result<alloc::vec::Vec<u8>, ()>> = inbox.ask(|reply| {
                    ActorMsg::Inner(VirtioBlkMsg::Read(sector, reply))
                }).await;
                match result {
                    Some(Ok(buf)) => {
                        println!("sector {}  (first 64 bytes):", sector);
                        let end = 64.min(buf.len());
                        for chunk in buf[..end].chunks(16) {
                            for b in chunk { print!("{:02x} ", b); }
                            println!();
                        }
                    }
                    Some(Err(())) => println!("virtio-blk: read error"),
                    None          => println!("virtio-blk: no response"),
                }
            }
            _ => println!("usage: blk <ls [path]|cat <path>|info|read <sector>>"),
        }
    }

    // ── pwd ───────────────────────────────────────────────────────────────────
    fn cmd_pwd(&self) {
        println!("{}", self.cwd.lock().clone());
    }

    // ── cd ────────────────────────────────────────────────────────────────────
    async fn cmd_cd(&self, path: &str) {
        let cwd    = self.cwd.lock().clone();
        let target = resolve_path(&cwd, if path.is_empty() { "/" } else { path });

        match devices::vfs::list_dir(&target).await {
            Ok(_)                                         => *self.cwd.lock() = target,
            Err(devices::vfs::VfsError::NotFound)         => println!("cd: not found: {}", target),
            Err(devices::vfs::VfsError::NotADirectory)    => println!("cd: not a directory: {}", target),
            Err(e)                                        => println!("cd: {:?}", e),
        }
    }

    // ── blk ls ────────────────────────────────────────────────────────────────
    async fn cmd_blk_ls(&self, path: &str) {
        let cwd  = self.cwd.lock().clone();
        let path = resolve_path(&cwd, path);

        match devices::vfs::list_dir(&path).await {
            Ok(entries) => {
                if entries.is_empty() {
                    println!("  (empty)");
                } else {
                    for e in &entries {
                        if e.is_dir {
                            println!("  [DIR]        {}", e.name);
                        } else {
                            println!("  [FILE {:5}]  {}", e.size, e.name);
                        }
                    }
                }
            }
            Err(e) => println!("error: {:?}", e),
        }
    }

    // ── blk cat ───────────────────────────────────────────────────────────────
    async fn cmd_blk_cat(&self, path: &str) {
        if path.is_empty() {
            println!("usage: cat <path>");
            return;
        }

        let cwd  = self.cwd.lock().clone();
        let path = resolve_path(&cwd, path);

        match devices::vfs::read_file(&path).await {
            Ok(data) => {
                for &b in &data {
                    if (0x20..0x7F).contains(&b) || b == b'\n' || b == b'\r' || b == b'\t' {
                        print!("{}", b as char);
                    } else {
                        print!(".");
                    }
                }
                println!();
            }
            Err(e) => println!("error: {:?}", e),
        }
    }

    // ── mount ─────────────────────────────────────────────────────────────────
    async fn cmd_mount(&self, rest: &str) {
        use devices::virtio::blk::{VirtioBlkMsg, VirtioBlkInfo};

        let rest = rest.trim();
        if rest.is_empty() {
            // List current mounts.
            devices::vfs::with_mounts(|mounts| {
                if mounts.is_empty() {
                    println!("  (no mounts)");
                } else {
                    for (mp, fs) in mounts {
                        println!("  {}  {}", mp, fs.fs_type());
                    }
                }
            });
            return;
        }

        let (fstype, mountpoint) = match rest.find(' ') {
            Some(i) => (rest[..i].trim(), rest[i + 1..].trim()),
            None    => { println!("usage: mount [<fstype> <mountpoint>]"); return; }
        };

        if mountpoint.is_empty() {
            println!("usage: mount [<fstype> <mountpoint>]");
            return;
        }

        match fstype {
            "proc" => {
                devices::vfs::mount(mountpoint, devices::vfs::AnyVfs::Proc(devices::vfs::ProcVfs));
                println!("mounted proc at {}", mountpoint);
            }
            "blk" => {
                let inbox = match libkernel::task::registry::get::<VirtioBlkMsg, VirtioBlkInfo>(
                    "virtio-blk"
                ) {
                    Some(mb) => mb,
                    None => { println!("virtio-blk: driver not found"); return; }
                };
                devices::vfs::mount(mountpoint, devices::vfs::AnyVfs::Exfat(
                    devices::vfs::ExfatVfs::new(inbox)
                ));
                println!("mounted blk at {}", mountpoint);
            }
            other => println!("unknown filesystem type '{}' (use: proc | blk)", other),
        }
    }

    async fn cmd_driver(&self, rest: &str) {
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
                } else if name == "shell" {
                    // Sending ErasedInfo to our own mailbox would deadlock —
                    // we can't recv() while blocked executing this command.
                    println!("  name:    shell");
                    println!("  running: true");
                } else {
                    match registry::ask_info(name).await {
                        Some(s) => {
                            println!("  name:    {}", s.name);
                            println!("  running: {}", s.running);
                            println!("  info:    {:?}", s.info);
                        }
                        None => println!("error: '{}' not found or not responding", name),
                    }
                }
            }
            _ => println!("usage: driver <start|stop|info> <name>"),
        }
    }
}

// ---------------------------------------------------------------------------
// help

fn cmd_help() {
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
    println!("  driver info <n>   query driver info");
    println!("  blk info          virtio-blk device info");
    println!("  blk read <n>      hex-dump sector N from virtio-blk");
    println!("  blk ls [path]     list exFAT directory (default: /)");
    println!("  blk cat <path>    print exFAT file as text");
    println!("  ls [path]         list directory via VFS");
    println!("  cat <path>        print file via VFS");
    println!("  pwd               print working directory");
    println!("  cd [path]         change working directory");
    println!("  mount             list mounted filesystems");
    println!("  mount proc <mp>   mount procfs at <mountpoint>");
    println!("  mount blk <mp>    mount exFAT block device at <mountpoint>");
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

    let (pml4_frame, _) = Cr3::read();
    let cr3_phys = pml4_frame.start_address().as_u64();

    println!("Page table (CR3={:#x}):", cr3_phys);
    println!("  {:18}  {:12}  {:6}  Flags", "Virtual", "Physical", "Size");

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

fn push_run(
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

fn sign_extend(addr: u64) -> u64 {
    if addr & (1 << 47) != 0 { addr | 0xffff_0000_0000_0000 } else { addr }
}

// ---------------------------------------------------------------------------
// cpuinfo — CPU identity and key control-register flags

fn cmd_cpuinfo() {
    use x86_64::registers::control::{Cr0, Cr4};
    use x86_64::registers::model_specific::Efer;
    use x86_64::registers::rflags;

    let family   = libkernel::cpuid::family().unwrap_or(0);
    let model    = libkernel::cpuid::model().unwrap_or(0);
    let stepping = libkernel::cpuid::stepping().unwrap_or(0);
    let mut vbuf = [0u8; 12];
    let vlen = libkernel::cpuid::vendor_into(&mut vbuf);
    let vendor = core::str::from_utf8(&vbuf[..vlen]).unwrap_or("?");
    println!("CPU: {}  family={:#x} model={:#x} stepping={}", vendor, family, model, stepping);

    let cr0 = Cr0::read().bits();
    print!("  CR0: {:#010x}", cr0);
    for (bit, name) in [(0, "PE"), (1, "MP"), (2, "EM"), (3, "TS"),
                        (5, "NE"), (16, "WP"), (31, "PG")] {
        if cr0 & (1 << bit) != 0 { print!(" {}", name); }
    }
    println!();

    let cr4 = Cr4::read().bits();
    print!("  CR4: {:#010x}", cr4);
    for (bit, name) in [(5, "PAE"), (7, "PGE"), (9, "OSFXSR"),
                        (10, "OSXMMEXCPT"), (13, "VMXE"), (20, "SMEP")] {
        if cr4 & (1 << bit) != 0 { print!(" {}", name); }
    }
    println!();

    let efer = Efer::read().bits();
    print!("  EFER:{:#010x}", efer);
    for (bit, name) in [(0, "SCE"), (8, "LME"), (10, "LMA"), (11, "NXE")] {
        if efer & (1 << bit) != 0 { print!(" {}", name); }
    }
    println!();

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
    println!("  0x00-0x1f  CPU exceptions");
    println!("    0x03  Breakpoint         [handler]");
    println!("    0x08  Double Fault       [handler, IST{}]",
        libkernel::gdt::DOUBLE_FAULT_IST_INDEX);
    println!("    0x0e  Page Fault         [handler]");
    println!("  PIC  (master offset={:#04x}, slave offset={:#04x})",
        PIC_1_OFFSET, PIC_2_OFFSET);
    println!("    {:#04x}  PIT Timer          (IRQ 0)", PIC_1_OFFSET);
    println!("    {:#04x}  PS/2 Keyboard      (IRQ 1)", PIC_1_OFFSET + 1);
    println!("  LAPIC");
    println!("    {:#04x}  Timer (preempt stub)", LAPIC_TIMER_VECTOR);
    println!("    0xff  Spurious           [handler]");

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
