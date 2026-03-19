//! SYSCALL dispatch — called from the assembly stub in `libkernel::syscall`.
//!
//! This module lives in `osl` so it can directly access both `libkernel`
//! (process, memory, scheduler) and `devices` (VFS) without callback trampolines.

use alloc::sync::Arc;
use x86_64::structures::paging::PageTableFlags;

use crate::errno;
use crate::syscall_nr::*;
use libkernel::consts::{PAGE_SIZE, PAGE_MASK};
use libkernel::file::FileHandle;

pub(crate) const USER_DATA_FLAGS: PageTableFlags = PageTableFlags::PRESENT
    .union(PageTableFlags::WRITABLE)
    .union(PageTableFlags::USER_ACCESSIBLE)
    .union(PageTableFlags::NO_EXECUTE);

/// Called from the assembly stub with the SysV64 calling convention.
#[no_mangle]
extern "sysv64" fn syscall_dispatch(
    nr: u64,
    a1: u64, a2: u64, a3: u64,
    a4: u64, a5: u64,
) -> i64 {
    let pid = libkernel::process::current_pid();
    if pid != libkernel::process::ProcessId::KERNEL {
        libkernel::serial_println!("[syscall] pid={} nr={} a1={:#x} a2={:#x} a3={:#x}",
            pid.as_u64(), nr, a1, a2, a3);
    }
    let ret = syscall_inner(nr, a1, a2, a3, a4, a5);
    if pid != libkernel::process::ProcessId::KERNEL {
        libkernel::serial_println!("[syscall] pid={} nr={} => {}", pid.as_u64(), nr, ret);
    }
    ret
}

fn syscall_inner(
    nr: u64,
    a1: u64, a2: u64, a3: u64,
    a4: u64, a5: u64,
) -> i64 {
    match nr {
        SYS_READ           => sys_read(a1, a2, a3),
        SYS_WRITE          => sys_write(a1, a2, a3),
        SYS_OPEN           => sys_open(a1, a2, a3),
        SYS_CLOSE          => sys_close(a1),
        SYS_FSTAT          => sys_fstat(a1, a2),
        SYS_LSEEK          => -errno::ESPIPE, // stdout is not seekable
        SYS_MMAP           => sys_mmap(a1, a2, a3, a4, a5),
        SYS_MPROTECT       => 0, // no-op
        SYS_MUNMAP         => 0, // stub (leak frames)
        SYS_BRK            => sys_brk(a1),
        SYS_IOCTL          => -errno::ENOTTY,
        SYS_WRITEV         => sys_writev(a1, a2, a3),
        SYS_EXIT
        | SYS_EXIT_GROUP   => sys_exit(a1 as i32),
        SYS_WAIT4          => sys_wait4(a1, a2, a3),
        SYS_GETCWD         => sys_getcwd(a1, a2),
        SYS_CHDIR          => sys_chdir(a1),
        SYS_ARCH_PRCTL     => sys_arch_prctl(a1, a2),
        SYS_FUTEX          => 0, // stub (single-threaded, lock never contended)
        SYS_GETDENTS64     => sys_getdents64(a1, a2, a3),
        SYS_SET_TID_ADDRESS => sys_set_tid_address(),
        SYS_SET_ROBUST_LIST => 0, // no-op
        SYS_SPAWN          => sys_spawn(a1, a2, a3, a4),
        other              => {
            log::warn!("unhandled syscall nr={} a1={:#x} a2={:#x} a3={:#x}",
                other, a1, a2, a3);
            -errno::ENOSYS
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers

/// Validate that a user buffer [ptr..ptr+len) is within user address space.
fn validate_user_buf(ptr: u64, len: u64) -> bool {
    const USER_LIMIT: u64 = 0x0000_8000_0000_0000;
    ptr != 0 && len <= USER_LIMIT && ptr.checked_add(len).map_or(false, |end| end <= USER_LIMIT)
}

/// Read a null-terminated string from user space. Returns None on bad pointer.
fn read_user_string(ptr: u64, max_len: usize) -> Option<alloc::string::String> {
    const USER_LIMIT: u64 = 0x0000_8000_0000_0000;
    if ptr == 0 || ptr >= USER_LIMIT { return None; }
    let mut len = 0usize;
    while len < max_len {
        let addr = ptr + len as u64;
        if addr >= USER_LIMIT { return None; }
        let b = unsafe { *(addr as *const u8) };
        if b == 0 { break; }
        len += 1;
    }
    let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len) };
    core::str::from_utf8(bytes).ok().map(|s| alloc::string::String::from(s))
}

/// Get a file handle from the current process's fd table, returning a Linux errno on failure.
fn get_fd_handle(fd: u64) -> Result<Arc<dyn FileHandle>, i64> {
    let pid = libkernel::process::current_pid();
    match libkernel::process::with_process_ref(pid, |p| p.get_fd(fd as usize)) {
        Some(Ok(h)) => Ok(h),
        Some(Err(e)) => Err(errno::file_errno(e)),
        None => Err(-errno::EBADF),
    }
}

/// Resolve a path relative to the current process's CWD.
fn resolve_user_path(path: &str) -> alloc::string::String {
    let pid = libkernel::process::current_pid();
    let cwd = libkernel::process::with_process_ref(pid, |p| p.cwd.clone())
        .unwrap_or_else(|| alloc::string::String::from("/"));
    libkernel::path::resolve(&cwd, path)
}

// ---------------------------------------------------------------------------
// VFS helpers (direct calls into devices::vfs, no callback trampolines)

fn vfs_read_file(path: &str) -> Result<alloc::vec::Vec<u8>, devices::vfs::VfsError> {
    let path = alloc::string::String::from(path);
    crate::blocking::blocking(async move {
        devices::vfs::read_file(&path).await
    })
}

fn vfs_list_dir(path: &str) -> Result<alloc::vec::Vec<devices::vfs::VfsDirEntry>, devices::vfs::VfsError> {
    let path = alloc::string::String::from(path);
    crate::blocking::blocking(async move {
        devices::vfs::list_dir(&path).await
    })
}

// ---------------------------------------------------------------------------
// Syscall implementations

fn sys_write(fd: u64, buf: u64, count: u64) -> i64 {
    if !validate_user_buf(buf, count) {
        return -errno::EFAULT;
    }
    let handle = match get_fd_handle(fd) {
        Ok(h) => h,
        Err(e) => return e,
    };
    let bytes = unsafe { core::slice::from_raw_parts(buf as *const u8, count as usize) };
    match handle.write(bytes) {
        Ok(n) => n as i64,
        Err(e) => errno::file_errno(e),
    }
}

fn sys_exit(code: i32) -> i64 {
    let pid = libkernel::process::current_pid();
    if pid != libkernel::process::ProcessId::KERNEL {
        libkernel::serial_println!("[kernel] pid {} exited with code {}", pid.as_u64(), code);

        let parent_pid = libkernel::process::with_process_ref(pid, |p| p.parent_pid);
        libkernel::process::mark_zombie(pid, code);

        if let Some(parent_pid) = parent_pid {
            let wait_thread = libkernel::process::with_process(parent_pid, |pp| pp.wait_thread.take());
            if let Some(Some(thread_idx)) = wait_thread {
                libkernel::task::scheduler::unblock(thread_idx);
            }
        }
    } else {
        libkernel::println!("\n[kernel] kernel sys_exit({}) — halting", code);
    }
    libkernel::task::scheduler::kill_current_thread();
}

fn sys_arch_prctl(code: u64, addr: u64) -> i64 {
    const ARCH_SET_FS: u64 = 0x1002;
    match code {
        ARCH_SET_FS => {
            // Safety: `addr` comes from userspace via arch_prctl(ARCH_SET_FS)
            // and will be used as the TLS base for FS-relative accesses.
            unsafe { libkernel::msr::write_fs_base(addr); }
            0
        }
        _ => -errno::EINVAL,
    }
}

fn sys_read(fd: u64, buf: u64, count: u64) -> i64 {
    if count == 0 { return 0; }
    if !validate_user_buf(buf, count) {
        return -errno::EFAULT;
    }
    let handle = match get_fd_handle(fd) {
        Ok(h) => h,
        Err(e) => return e,
    };
    let user_buf = unsafe { core::slice::from_raw_parts_mut(buf as *mut u8, count as usize) };
    match handle.read(user_buf) {
        Ok(n) => n as i64,
        Err(e) => errno::file_errno(e),
    }
}

fn sys_fstat(_fd: u64, buf: u64) -> i64 {
    const STAT_SIZE: usize = 144;
    const S_IFCHR: u32 = 0o020000;
    let stat_ptr = buf as *mut u8;
    unsafe {
        core::ptr::write_bytes(stat_ptr, 0, STAT_SIZE);
        let mode_ptr = stat_ptr.add(24) as *mut u32;
        mode_ptr.write(S_IFCHR | 0o666);
    }
    0
}

fn sys_set_tid_address() -> i64 {
    libkernel::process::current_pid().as_u64() as i64
}

fn sys_brk(addr: u64) -> i64 {
    use libkernel::process;
    use libkernel::memory::with_memory;

    let pid = process::current_pid();
    if pid == process::ProcessId::KERNEL {
        return 0;
    }

    let (brk_base, brk_current, pml4_phys) = match process::with_process_ref(pid, |p| {
        (p.brk_base, p.brk_current, p.pml4_phys)
    }) {
        Some(v) => v,
        None => return 0,
    };

    if addr == 0 || addr < brk_base {
        return brk_current as i64;
    }

    let new_brk = (addr + PAGE_MASK) & !PAGE_MASK;
    if new_brk <= brk_current {
        process::with_process(pid, |p| p.brk_current = new_brk);
        return new_brk as i64;
    }

    let pages_needed = ((new_brk - brk_current) / PAGE_SIZE) as usize;
    let ok = with_memory(|mem| {
        mem.alloc_and_map_user_pages(pages_needed, brk_current, pml4_phys, USER_DATA_FLAGS)
            .is_ok()
    });

    if ok {
        process::with_process(pid, |p| p.brk_current = new_brk);
        new_brk as i64
    } else {
        brk_current as i64
    }
}

fn sys_writev(fd: u64, iov_ptr: u64, iovcnt: u64) -> i64 {
    let handle = match get_fd_handle(fd) {
        Ok(h) => h,
        Err(e) => return e,
    };
    let mut total: usize = 0;
    for i in 0..iovcnt as usize {
        let iov_addr = iov_ptr + (i * 16) as u64;
        let iov_base = unsafe { *(iov_addr as *const u64) };
        let iov_len = unsafe { *((iov_addr + 8) as *const u64) } as usize;
        if iov_len == 0 {
            continue;
        }
        let bytes = unsafe { core::slice::from_raw_parts(iov_base as *const u8, iov_len) };
        match handle.write(bytes) {
            Ok(n) => total += n,
            Err(e) => return errno::file_errno(e),
        }
    }
    total as i64
}

fn sys_close(fd: u64) -> i64 {
    let pid = libkernel::process::current_pid();
    match libkernel::process::with_process(pid, |p| p.close_fd(fd as usize)) {
        Some(Ok(())) => 0,
        Some(Err(e)) => errno::file_errno(e),
        None => -errno::EBADF,
    }
}

fn sys_mmap(addr: u64, length: u64, _prot: u64, flags: u64, _a5: u64) -> i64 {
    use libkernel::process;
    use libkernel::memory::with_memory;

    const MAP_ANONYMOUS: u64 = 0x20;
    const MAP_PRIVATE: u64 = 0x02;
    const MAP_FIXED: u64 = 0x10;

    if flags & MAP_ANONYMOUS == 0 {
        return -errno::ENOSYS;
    }
    if flags & MAP_FIXED != 0 && addr != 0 {
        return -errno::ENOSYS;
    }
    let _ = MAP_PRIVATE;

    let pid = process::current_pid();
    if pid == process::ProcessId::KERNEL {
        return -errno::ENOMEM;
    }

    let aligned_len = (length + PAGE_MASK) & !PAGE_MASK;
    let num_pages = (aligned_len / PAGE_SIZE) as usize;

    let (mmap_next, pml4_phys) = match process::with_process_ref(pid, |p| {
        (p.mmap_next, p.pml4_phys)
    }) {
        Some(v) => v,
        None => return -errno::ENOMEM,
    };

    let region_base = mmap_next - aligned_len;

    let ok = with_memory(|mem| {
        mem.alloc_and_map_user_pages(num_pages, region_base, pml4_phys, USER_DATA_FLAGS)
            .is_ok()
    });

    if ok {
        process::with_process(pid, |p| {
            p.mmap_next = region_base;
            p.mmap_regions.push((region_base, aligned_len));
        });
        region_base as i64
    } else {
        -errno::ENOMEM
    }
}

// ---------------------------------------------------------------------------
// VFS syscalls

fn sys_open(path_ptr: u64, flags: u64, _mode: u64) -> i64 {
    use alloc::sync::Arc;

    let path = match read_user_string(path_ptr, 4096) {
        Some(p) => p,
        None => return -errno::EFAULT,
    };

    let resolved = resolve_user_path(&path);

    const O_DIRECTORY: u64 = 0o200000;
    let want_dir = flags & O_DIRECTORY != 0;

    // Try to open as file first (unless O_DIRECTORY), then fall back to dir.
    if !want_dir {
        match vfs_read_file(&resolved) {
            Ok(data) => {
                let handle = Arc::new(crate::file::VfsHandle::new(data));
                let pid = libkernel::process::current_pid();
                return match libkernel::process::with_process(pid, |p| p.alloc_fd(handle)) {
                    Some(Ok(fd)) => fd as i64,
                    Some(Err(e)) => errno::file_errno(e),
                    None => -errno::EBADF,
                };
            }
            Err(devices::vfs::VfsError::NotFound) | Err(devices::vfs::VfsError::NotAFile) => {
                // ENOENT or EISDIR — fall through to try as directory.
            }
            Err(ref e) => return errno::vfs_errno(e),
        }
    }

    // Try as directory.
    match vfs_list_dir(&resolved) {
        Ok(entries) => {
            let handle = Arc::new(crate::file::DirHandle::new(entries));
            let pid = libkernel::process::current_pid();
            match libkernel::process::with_process(pid, |p| p.alloc_fd(handle)) {
                Some(Ok(fd)) => fd as i64,
                Some(Err(e)) => errno::file_errno(e),
                None => -errno::EBADF,
            }
        }
        Err(ref e) => errno::vfs_errno(e),
    }
}

fn sys_getdents64(fd: u64, buf: u64, count: u64) -> i64 {
    if !validate_user_buf(buf, count) {
        return -errno::EFAULT;
    }
    let handle = match get_fd_handle(fd) {
        Ok(h) => h,
        Err(e) => return e,
    };
    let user_buf = unsafe { core::slice::from_raw_parts_mut(buf as *mut u8, count as usize) };
    match handle.getdents64(user_buf) {
        Ok(n) => n as i64,
        Err(e) => errno::file_errno(e),
    }
}

fn sys_getcwd(buf: u64, size: u64) -> i64 {
    if !validate_user_buf(buf, size) {
        return -errno::EFAULT;
    }
    let pid = libkernel::process::current_pid();
    let cwd = match libkernel::process::with_process_ref(pid, |p| p.cwd.clone()) {
        Some(c) => c,
        None => return -errno::EFAULT,
    };
    let cwd_bytes = cwd.as_bytes();
    let needed = cwd_bytes.len() + 1;
    if needed > size as usize {
        return -errno::ERANGE;
    }
    let user_buf = unsafe { core::slice::from_raw_parts_mut(buf as *mut u8, needed) };
    user_buf[..cwd_bytes.len()].copy_from_slice(cwd_bytes);
    user_buf[cwd_bytes.len()] = 0;
    buf as i64
}

fn sys_chdir(path_ptr: u64) -> i64 {
    let path = match read_user_string(path_ptr, 4096) {
        Some(p) => p,
        None => return -errno::EFAULT,
    };
    let resolved = resolve_user_path(&path);

    match vfs_list_dir(&resolved) {
        Ok(_) => {
            let pid = libkernel::process::current_pid();
            libkernel::process::with_process(pid, |p| {
                p.cwd = resolved;
            });
            0
        }
        Err(ref e) => errno::vfs_errno(e),
    }
}

// ---------------------------------------------------------------------------
// Process management syscalls

fn sys_wait4(pid_arg: u64, status_ptr: u64, _options: u64) -> i64 {
    let parent_pid = libkernel::process::current_pid();
    let target_pid = pid_arg as i64;

    loop {
        if let Some((child_pid, exit_code)) = libkernel::process::find_zombie_child(parent_pid, target_pid) {
            if status_ptr != 0 && validate_user_buf(status_ptr, 4) {
                let wstatus = (exit_code as u32) << 8;
                unsafe { *(status_ptr as *mut u32) = wstatus; }
            }
            libkernel::process::reap(child_pid);
            libkernel::console::set_foreground(parent_pid);
            return child_pid.as_u64() as i64;
        }

        if !libkernel::process::has_children(parent_pid) {
            return -errno::ECHILD;
        }

        let thread_idx = libkernel::task::scheduler::current_thread_idx();
        libkernel::process::with_process(parent_pid, |p| {
            p.wait_thread = Some(thread_idx);
        });
        libkernel::task::scheduler::block_current_thread();
    }
}

fn sys_spawn(path_ptr: u64, path_len: u64, argv_ptr: u64, argv_count: u64) -> i64 {
    if !validate_user_buf(path_ptr, path_len) {
        return -errno::EFAULT;
    }
    let path_bytes = unsafe { core::slice::from_raw_parts(path_ptr as *const u8, path_len as usize) };
    let path = match core::str::from_utf8(path_bytes) {
        Ok(s) => alloc::string::String::from(s),
        Err(_) => return -errno::EINVAL,
    };
    let resolved = resolve_user_path(&path);

    let mut argv: alloc::vec::Vec<alloc::vec::Vec<u8>> = alloc::vec::Vec::new();
    if argv_count > 0 && argv_ptr != 0 {
        for i in 0..argv_count as usize {
            let ptr_addr = argv_ptr + (i * 8) as u64;
            if !validate_user_buf(ptr_addr, 8) {
                return -errno::EFAULT;
            }
            let str_ptr = unsafe { *(ptr_addr as *const u64) };
            match read_user_string(str_ptr, 4096) {
                Some(s) => argv.push(s.into_bytes()),
                None => return -errno::EFAULT,
            }
        }
    }

    // Read ELF from VFS — direct call, no callback trampolines.
    let elf_data = match vfs_read_file(&resolved) {
        Ok(data) => data,
        Err(_) => return -errno::ENOENT,
    };

    let parent_pid = libkernel::process::current_pid();
    let argv_slices: alloc::vec::Vec<&[u8]> = argv.iter().map(|v| v.as_slice()).collect();

    // Direct call to osl::spawn — no function pointer transmute.
    match crate::spawn::spawn_process_full(&elf_data, &argv_slices, parent_pid) {
        Ok(child_pid) => {
            libkernel::console::set_foreground(child_pid);
            child_pid.as_u64() as i64
        }
        Err(_) => -errno::ENOENT,
    }
}
