use alloc::string::String;
use core::fmt::Write;

pub(super) fn generate(pid: libkernel::process::ProcessId) -> String {
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
