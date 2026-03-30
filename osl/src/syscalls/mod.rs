//! SYSCALL dispatch — called from the assembly stub in `libkernel::syscall`.
//!
//! This module lives in `osl` so it can directly access both `libkernel`
//! (process, memory, scheduler) and `devices` (VFS) without callback trampolines.
//!
//! Individual syscall implementations are grouped into submodules by category.

mod fb;
mod fs;
mod io;
mod mem;
mod misc;
mod process;
mod service;
mod shmem;

use crate::errno;
use crate::syscall_nr::*;

/// Called from the assembly stub with the SysV64 calling convention.
#[no_mangle]
extern "sysv64" fn syscall_dispatch(
    nr: u64,
    a1: u64, a2: u64, a3: u64,
    a4: u64, a5: u64,
) -> i64 {
    syscall_inner(nr, a1, a2, a3, a4, a5)
}

fn syscall_inner(
    nr: u64,
    a1: u64, a2: u64, a3: u64,
    a4: u64, a5: u64,
) -> i64 {
    match nr {
        SYS_READ           => io::sys_read(a1, a2, a3),
        SYS_WRITE          => io::sys_write(a1, a2, a3),
        SYS_OPEN           => fs::sys_open(a1, a2, a3),
        SYS_CLOSE          => fs::sys_close(a1),
        SYS_FSTAT          => fs::sys_fstat(a1, a2),
        SYS_LSEEK          => -errno::ESPIPE,
        SYS_MMAP           => mem::sys_mmap(a1, a2, a3, a4, a5),
        SYS_MPROTECT       => mem::sys_mprotect(a1, a2, a3),
        SYS_MUNMAP         => mem::sys_munmap(a1, a2),
        SYS_BRK            => mem::sys_brk(a1),
        SYS_RT_SIGACTION   => crate::signal::sys_rt_sigaction(a1, a2, a3, a4),
        SYS_RT_SIGPROCMASK => crate::signal::sys_rt_sigprocmask(a1, a2, a3, a4),
        SYS_RT_SIGRETURN   => crate::signal::sys_rt_sigreturn(),
        SYS_IOCTL          => -errno::ENOTTY,
        SYS_WRITEV         => io::sys_writev(a1, a2, a3),
        SYS_MADVISE        => 0,
        SYS_DUP2           => fs::sys_dup2(a1, a2),
        SYS_GETPID         => process::sys_getpid(),
        SYS_CLONE          => crate::clone::sys_clone(a1, a2, a3, a4, a5),
        SYS_EXECVE         => crate::exec::sys_execve(a1, a2, a3),
        SYS_EXIT
        | SYS_EXIT_GROUP   => process::sys_exit(a1 as i32),
        SYS_WAIT4          => process::sys_wait4(a1, a2, a3),
        SYS_KILL           => crate::signal::sys_kill(a1, a2),
        SYS_FCNTL          => fs::sys_fcntl(a1, a2, a3),
        SYS_GETCWD         => fs::sys_getcwd(a1, a2),
        SYS_CHDIR          => fs::sys_chdir(a1),
        SYS_SIGALTSTACK    => 0,
        SYS_ARCH_PRCTL     => misc::sys_arch_prctl(a1, a2),
        SYS_FUTEX          => 0,
        SYS_SCHED_GETAFFINITY => misc::sys_sched_getaffinity(a1, a2, a3),
        SYS_GETDENTS64     => io::sys_getdents64(a1, a2, a3),
        SYS_SET_TID_ADDRESS => process::sys_set_tid_address(),
        SYS_CLOCK_GETTIME  => misc::sys_clock_gettime(a1, a2),
        SYS_SET_ROBUST_LIST => 0,
        SYS_PIPE           => fs::sys_pipe2(a1, 0),
        SYS_PIPE2          => fs::sys_pipe2(a1, a2),
        SYS_GETRANDOM      => misc::sys_getrandom(a1, a2, a3),
        SYS_IO_CREATE      => crate::io_port::sys_io_create(a1 as u32),
        SYS_IO_SUBMIT      => crate::io_port::sys_io_submit(a1 as i32, a2, a3 as u32),
        SYS_IO_WAIT        => crate::io_port::sys_io_wait(a1 as i32, a2, a3 as u32, a4 as u32, a5),
        SYS_IRQ_CREATE     => crate::irq::sys_irq_create(a1 as u32),
        SYS_IPC_CREATE     => crate::ipc::sys_ipc_create(a1, a2 as u32, a3 as u32),
        SYS_IPC_SEND       => crate::ipc::sys_ipc_send(a1 as i32, a2, a3 as u32),
        SYS_IPC_RECV       => crate::ipc::sys_ipc_recv(a1 as i32, a2, a3 as u32),
        SYS_SHMEM_CREATE   => shmem::sys_shmem_create(a1, a2 as u32),
        SYS_NOTIFY_CREATE  => crate::notify::sys_notify_create(a1 as u32),
        SYS_NOTIFY         => crate::notify::sys_notify(a1 as i32),
        SYS_IO_SETUP_RINGS => crate::io_port::sys_io_setup_rings(a1 as i32, a2),
        SYS_IO_RING_ENTER  => crate::io_port::sys_io_ring_enter(a1 as i32, a2 as u32, a3 as u32, a4 as u32),
        SYS_SVC_REGISTER   => service::sys_svc_register(a1, a2 as i32),
        SYS_SVC_LOOKUP     => service::sys_svc_lookup(a1),
        SYS_FRAMEBUFFER_OPEN => fb::sys_framebuffer_open(a1 as u32),
        other              => {
            log::warn!("unhandled syscall nr={} a1={:#x} a2={:#x} a3={:#x}",
                other, a1, a2, a3);
            -errno::ENOSYS
        }
    }
}

// ---------------------------------------------------------------------------
// Shared helpers used by syscall submodules and other osl crates

/// Resolve a path relative to the current process's CWD.
pub fn resolve_user_path(path: &str) -> alloc::string::String {
    let pid = libkernel::process::current_pid();
    let cwd = libkernel::process::with_process_ref(pid, |p| p.cwd.clone())
        .unwrap_or_else(|| alloc::string::String::from("/"));
    libkernel::path::resolve(&cwd, path)
}

/// Read a file via the VFS (blocking async bridge).
pub fn vfs_read_file(path: &str, caller_pid: libkernel::process::ProcessId) -> Result<alloc::vec::Vec<u8>, devices::vfs::VfsError> {
    let path = alloc::string::String::from(path);
    crate::blocking::blocking(async move {
        devices::vfs::read_file(&path, caller_pid).await
    })
}

/// List a directory via the VFS (blocking async bridge).
pub(crate) fn vfs_list_dir(path: &str) -> Result<alloc::vec::Vec<devices::vfs::VfsDirEntry>, devices::vfs::VfsError> {
    let path = alloc::string::String::from(path);
    crate::blocking::blocking(async move {
        devices::vfs::list_dir(&path).await
    })
}
