//! Process management syscalls: exit, wait4, getpid, set_tid_address.

use crate::errno;
use crate::user_mem::validate_user_buf;
use libkernel::process;
use libkernel::wait_condition::WaitCondition;

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

    // [spec: completion_port.tla — single lock acquisition for
    //  check + register + mark_blocked eliminates the lost-wakeup race]
    loop {
        let table = process::lock_table();

        if let Some((child_pid, exit_code)) = process::find_zombie_child_in(&table, parent_pid, target_pid) {
            drop(table);
            if status_ptr != 0 && validate_user_buf(status_ptr, 4) {
                let wstatus = (exit_code as u32) << 8;
                unsafe { *(status_ptr as *mut u32) = wstatus; }
            }
            process::reap(child_pid);
            libkernel::console::set_foreground(parent_pid);
            return child_pid.as_u64() as i64;
        }

        if !process::has_children_in(&table, parent_pid) {
            return -errno::ECHILD;
        }

        // Check for pending signals under same lock.
        let has_signal = table.get(&parent_pid)
            .map_or(false, |p| (p.signal.pending & !p.signal.blocked) != 0);
        if has_signal {
            return -errno::EINTR;
        }

        WaitCondition::wait_while(Some(table), |table, idx| {
            if let Some(p) = table.get_mut(&parent_pid) {
                p.wait_thread = Some(idx);
                p.signal_thread = Some(idx);
            }
        });

        // Clear signal_thread after waking.
        process::with_process(parent_pid, |p| {
            p.signal_thread = None;
        });
    }
}

