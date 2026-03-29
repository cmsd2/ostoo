//! ELF process spawning with argv and parent PID support.

use libkernel::consts::PAGE_SIZE;
use libkernel::process::{Process, ProcessId};

use crate::elf_loader;

/// Spawn with argv and explicit parent.
/// Used by the spawn syscall.
pub fn spawn_process_full(
    elf_data: &[u8],
    argv: &[&[u8]],
    envp: &[&[u8]],
    parent_pid: ProcessId,
) -> Result<ProcessId, &'static str> {
    // Free kernel stacks of previously exited processes so the heap doesn't run out.
    libkernel::process::reap_zombies();

    let info = libkernel::elf::parse(elf_data).map_err(|e| {
        log::error!("ELF parse error: {:?} (data len={}, first 4 bytes={:02x?})",
            e, elf_data.len(), &elf_data[..elf_data.len().min(4)]);
        "invalid ELF binary"
    })?;

    if info.segments.is_empty() {
        return Err("ELF has no loadable segments");
    }

    let (pml4_phys, stack_kernel_base) = elf_loader::load_elf_address_space(elf_data, &info)?;

    let brk_base = elf_loader::compute_brk_base(&info);

    // Build the initial user stack: argc/argv/envp/auxv.
    let user_rsp = build_initial_stack(
        stack_kernel_base,
        elf_loader::ELF_STACK_VIRT,
        elf_loader::ELF_STACK_SIZE,
        &info,
        argv,
        envp,
    );

    // Create the process and insert it into the process table.
    let mut proc = Process::new(pml4_phys, info.entry, user_rsp, brk_base);
    proc.parent_pid = parent_pid;
    let pid = proc.pid;
    libkernel::process::insert(proc);

    // Spawn a scheduler thread and record the thread index on the process.
    let thread_idx = libkernel::task::scheduler::spawn_user_thread(pid, pml4_phys);
    libkernel::process::with_process(pid, |p| {
        p.thread_idx = Some(thread_idx);
    });

    log::info!("spawn_process: pid={} entry={:#x} pml4={:#x}",
        pid.as_u64(), info.entry, pml4_phys.as_u64());

    Ok(pid)
}

// ---------------------------------------------------------------------------
// Initial stack builder for ELF processes

/// Build the initial user stack layout that musl expects:
///
/// ```text
/// [stack_top]
///   argv/envp string data (null-terminated strings)
///   16 bytes of zeros (AT_RANDOM target)
///   auxv pairs (AT_NULL terminator)
///   envp[n-1] ptr ... envp[0] ptr
///   NULL                    <- envp terminator
///   argv[argc-1] ptr
///   ...
///   argv[0] ptr
///   NULL                    <- argv terminator
///   argc
/// [RSP points here, 16-byte aligned]
/// ```
pub fn build_initial_stack(
    kernel_base: x86_64::VirtAddr,
    user_virt_base: u64,
    stack_size: u64,
    info: &libkernel::elf::ElfInfo,
    argv: &[&[u8]],
    envp: &[&[u8]],
) -> u64 {
    let kernel_top = kernel_base.as_u64() + stack_size;
    let user_top = user_virt_base + stack_size;

    let mut cursor = kernel_top;

    let push = |cursor: &mut u64, val: u64| {
        *cursor -= 8;
        unsafe { *(*cursor as *mut u64) = val; }
    };

    let k2u = |kaddr: u64| -> u64 {
        user_top - (kernel_top - kaddr)
    };

    // 1. Write argv string data at the top of the stack.
    let mut argv_user_addrs: alloc::vec::Vec<u64> = alloc::vec::Vec::new();
    for arg in argv {
        let len = arg.len() + 1; // +1 for null terminator
        cursor -= len as u64;
        let str_user_addr = k2u(cursor);
        unsafe {
            let p = cursor as *mut u8;
            core::ptr::copy_nonoverlapping(arg.as_ptr(), p, arg.len());
            *p.add(arg.len()) = 0; // null-terminate
        }
        argv_user_addrs.push(str_user_addr);
    }

    // 1b. Write envp string data.
    let mut envp_user_addrs: alloc::vec::Vec<u64> = alloc::vec::Vec::new();
    for env in envp {
        let len = env.len() + 1;
        cursor -= len as u64;
        let str_user_addr = k2u(cursor);
        unsafe {
            let p = cursor as *mut u8;
            core::ptr::copy_nonoverlapping(env.as_ptr(), p, env.len());
            *p.add(env.len()) = 0;
        }
        envp_user_addrs.push(str_user_addr);
    }

    // 2. AT_RANDOM data: 16 bytes of "random" data.
    cursor -= 16;
    let random_user_addr = k2u(cursor);
    unsafe {
        let p = cursor as *mut u8;
        for i in 0..16u8 {
            *p.add(i as usize) = i.wrapping_mul(7).wrapping_add(0x42);
        }
    }

    // Align cursor to 8 bytes.
    cursor &= !7;

    // 3. Auxiliary vector.
    const AT_NULL: u64 = 0;
    const AT_PHDR: u64 = 3;
    const AT_PHENT: u64 = 4;
    const AT_PHNUM: u64 = 5;
    const AT_PAGESZ: u64 = 6;
    const AT_ENTRY: u64 = 9;
    const AT_UID: u64 = 11;
    const AT_RANDOM: u64 = 25;

    // Pre-compute alignment: count all items that will be pushed below the
    // cursor, then check if the resulting RSP is 16-byte aligned.  If not,
    // add one padding word above AT_NULL (where musl never looks).
    // Items: 8 auxv pairs (16) + envp NULL + envp ptrs + argv NULL + argv ptrs + argc
    let total_pushes: u64 = 16 + 1 + envp.len() as u64 + 1 + argv.len() as u64 + 1;
    let prospective_cursor = cursor - total_pushes * 8;
    let prospective_rsp = user_top - (kernel_top - prospective_cursor);
    if prospective_rsp % 16 != 0 {
        push(&mut cursor, 0); // alignment pad above AT_NULL (harmless dead zone)
    }

    push(&mut cursor, 0); push(&mut cursor, AT_NULL);
    push(&mut cursor, random_user_addr); push(&mut cursor, AT_RANDOM);
    push(&mut cursor, info.entry); push(&mut cursor, AT_ENTRY);
    push(&mut cursor, info.phnum as u64); push(&mut cursor, AT_PHNUM);
    push(&mut cursor, info.phentsize as u64); push(&mut cursor, AT_PHENT);
    push(&mut cursor, info.phdr_vaddr); push(&mut cursor, AT_PHDR);
    push(&mut cursor, PAGE_SIZE); push(&mut cursor, AT_PAGESZ);
    push(&mut cursor, 0); push(&mut cursor, AT_UID);

    // 4. envp pointers: NULL terminator, then pointers in reverse order.
    push(&mut cursor, 0); // envp NULL terminator
    for addr in envp_user_addrs.iter().rev() {
        push(&mut cursor, *addr);
    }

    // 5. argv pointers: NULL terminator, then pointers in reverse order.
    push(&mut cursor, 0); // argv NULL terminator
    for addr in argv_user_addrs.iter().rev() {
        push(&mut cursor, *addr);
    }

    // 6. argc — immediately followed by argv[0] with no padding.
    push(&mut cursor, argv.len() as u64); // argc

    let offset_from_top = kernel_top - cursor;
    let user_rsp = user_top - offset_from_top;

    debug_assert!(user_rsp % 16 == 0, "user RSP must be 16-byte aligned, got {:#x}", user_rsp);

    user_rsp
}
