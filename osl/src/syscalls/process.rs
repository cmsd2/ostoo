//! Process management syscalls: exit, wait4, getpid, set_tid_address, spawn.

use crate::errno;
use crate::user_mem::{validate_user_buf, read_user_string_array, user_slice};
use libkernel::process;

use super::{resolve_user_path, vfs_read_file};

pub(crate) fn sys_exit(code: i32) -> i64 {
    let pid = process::current_pid();
    if pid != process::ProcessId::KERNEL {
        libkernel::serial_println!("[kernel] pid {} exited with code {}", pid.as_u64(), code);
        process::terminate_process(pid, code);
    } else {
        libkernel::println!("\n[kernel] kernel sys_exit({}) — halting", code);
        libkernel::task::scheduler::kill_current_thread();
    }
}

pub(crate) fn sys_getpid() -> i64 {
    process::current_pid().as_u64() as i64
}

pub(crate) fn sys_set_tid_address() -> i64 {
    process::current_pid().as_u64() as i64
}

pub(crate) fn sys_wait4(pid_arg: u64, status_ptr: u64, _options: u64) -> i64 {
    let parent_pid = process::current_pid();
    let target_pid = pid_arg as i64;

    loop {
        if let Some((child_pid, exit_code)) = process::find_zombie_child(parent_pid, target_pid) {
            if status_ptr != 0 && validate_user_buf(status_ptr, 4) {
                let wstatus = (exit_code as u32) << 8;
                unsafe { *(status_ptr as *mut u32) = wstatus; }
            }
            process::reap(child_pid);
            libkernel::console::set_foreground(parent_pid);
            return child_pid.as_u64() as i64;
        }

        if !process::has_children(parent_pid) {
            return -errno::ECHILD;
        }

        let thread_idx = libkernel::task::scheduler::current_thread_idx();
        process::with_process(parent_pid, |p| {
            p.wait_thread = Some(thread_idx);
        });
        libkernel::task::scheduler::block_current_thread();
    }
}

pub(crate) fn sys_spawn(path_ptr: u64, path_len: u64, argv_ptr: u64, argv_count: u64, envp_ptr: u64) -> i64 {
    let path_bytes = match user_slice(path_ptr, path_len) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let path = match core::str::from_utf8(path_bytes) {
        Ok(s) => alloc::string::String::from(s),
        Err(_) => return -errno::EINVAL,
    };
    let resolved = resolve_user_path(&path);

    // Read argv from userspace.
    let mut argv: alloc::vec::Vec<alloc::vec::Vec<u8>> = alloc::vec::Vec::new();
    if argv_count > 0 && argv_ptr != 0 {
        match read_user_string_array(argv_ptr) {
            Ok(strings) => {
                // Take only argv_count entries (the array may be longer).
                for s in strings.into_iter().take(argv_count as usize) {
                    argv.push(s.into_bytes());
                }
            }
            Err(e) => return e,
        }
    }

    // Read envp from userspace (6th arg = envp_count from r9).
    let mut envp: alloc::vec::Vec<alloc::vec::Vec<u8>> = alloc::vec::Vec::new();
    if envp_ptr != 0 {
        match read_user_string_array(envp_ptr) {
            Ok(strings) => {
                let envp_count = libkernel::syscall::get_user_r9() as usize;
                for s in strings.into_iter().take(envp_count) {
                    envp.push(s.into_bytes());
                }
            }
            Err(e) => return e,
        }
    }

    // Read ELF from VFS.
    let parent_pid = process::current_pid();
    let elf_data = match vfs_read_file(&resolved, parent_pid) {
        Ok(data) => data,
        Err(_) => return -errno::ENOENT,
    };
    let argv_slices: alloc::vec::Vec<&[u8]> = argv.iter().map(|v| v.as_slice()).collect();
    let envp_slices: alloc::vec::Vec<&[u8]> = envp.iter().map(|v| v.as_slice()).collect();

    match crate::spawn::spawn_process_full(&elf_data, &argv_slices, &envp_slices, parent_pid) {
        Ok(child_pid) => {
            libkernel::console::set_foreground(child_pid);
            child_pid.as_u64() as i64
        }
        Err(_) => -errno::ENOENT,
    }
}
