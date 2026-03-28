//! clone(2) syscall implementation (CLONE_VM|CLONE_VFORK semantics).

use crate::errno;
use libkernel::process::{self, Process};
use libkernel::task::scheduler;

/// Flags we accept from musl's posix_spawn: CLONE_VM | CLONE_VFORK | SIGCHLD.
const CLONE_VM: u64    = 0x0000_0100;
const CLONE_VFORK: u64 = 0x0000_4000;
const SIGCHLD: u64     = 17;
const SUPPORTED_FLAGS: u64 = CLONE_VM | CLONE_VFORK | SIGCHLD;

pub fn sys_clone(flags: u64, child_stack: u64, _ptid: u64, _ctid: u64, _tls: u64) -> i64 {
    // Only support the exact flag combination musl uses for posix_spawn.
    if flags & !SUPPORTED_FLAGS != 0 || flags & CLONE_VM == 0 || flags & CLONE_VFORK == 0 {
        libkernel::serial_println!("[clone] unsupported flags: {:#x}", flags);
        return -errno::ENOSYS;
    }

    if child_stack == 0 {
        return -errno::EINVAL;
    }

    let parent_pid = process::current_pid();
    let parent_thread_idx = scheduler::current_thread_idx();

    // Read parent process info needed for the child.
    let parent_info = match process::with_process_ref(parent_pid, |p| {
        (p.pml4_phys, p.cwd.clone(), p.fd_table.clone(),
         p.brk_base, p.brk_current, p.mmap_next, p.vma_map.clone())
    }) {
        Some(info) => info,
        None => return -errno::ENOSYS,
    };
    let (pml4_phys, cwd, fd_table, brk_base, brk_current, mmap_next, vma_map) = parent_info;

    // Read user RIP, RFLAGS, and R9 saved by the SYSCALL entry stub.
    let user_rip = libkernel::syscall::get_user_rip();
    let user_rflags = libkernel::syscall::get_user_rflags();
    let user_r9 = libkernel::syscall::get_user_r9();

    // Create child process sharing the parent's address space (CLONE_VM).
    let mut child = Process::new(pml4_phys, user_rip, child_stack, brk_base);
    child.parent_pid = parent_pid;
    child.cwd = cwd;
    child.fd_table = fd_table;
    child.brk_current = brk_current;
    child.mmap_next = mmap_next;
    child.vma_map = vma_map;
    child.vfork_parent_thread = Some(parent_thread_idx);
    child.pml4_shared = true;

    let child_pid = child.pid;
    process::insert(child);

    // Spawn a scheduler thread for the child that "returns from syscall" with RAX=0.
    let thread_idx = scheduler::spawn_clone_thread(
        child_pid, pml4_phys, user_rip, child_stack, user_rflags, user_r9,
    );
    process::with_process(child_pid, |p| {
        p.thread_idx = Some(thread_idx);
    });

    libkernel::serial_println!("[clone] parent={} child={} child_stack={:#x} user_rip={:#x}",
        parent_pid.as_u64(), child_pid.as_u64(), child_stack, user_rip);

    // CLONE_VFORK: block parent until child calls execve or _exit.
    scheduler::block_current_thread();

    child_pid.as_u64() as i64
}
