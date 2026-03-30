//! Signal-related syscall implementations: rt_sigaction, rt_sigprocmask,
//! rt_sigreturn, kill.

use crate::errno;
use crate::user_mem::validate_user_buf;

/// `rt_sigreturn` (syscall 15) — restore context after signal handler returns.
///
/// The handler did `ret` (popping pretcode), then `__restore_rt` did `syscall`
/// without touching RSP, so user RSP = frame_base + 8.
pub fn sys_rt_sigreturn() -> i64 {
    use libkernel::signal::*;
    use libkernel::syscall::{get_saved_frame_ptr, get_saved_user_rsp, set_saved_user_rsp};

    let pid = libkernel::process::current_pid();

    // Offsets must match deliver_signal in libkernel/src/syscall.rs.
    const PRETCODE_SIZE: u64 = 8;
    const UC_HEADER: u64 = 8 + 8 + 24;       // uc_flags + uc_link + uc_stack
    const SIGCONTEXT_SIZE: u64 = 32 * 8;     // 256 bytes

    let user_rsp = get_saved_user_rsp();
    let frame_base = user_rsp - PRETCODE_SIZE; // handler's `ret` consumed pretcode
    let uc_base = frame_base + PRETCODE_SIZE;
    let sc_base = uc_base + UC_HEADER;

    // Read sigcontext registers.
    let sc = sc_base as *const u64;
    let (r8, r9, r10) = unsafe {
        (sc.add(0).read(), sc.add(1).read(), sc.add(2).read())
    };
    let (rdi, rsi, rdx, rax) = unsafe {
        (sc.add(8).read(), sc.add(9).read(), sc.add(12).read(), sc.add(13).read())
    };
    let (orig_rip, orig_rflags, orig_rsp) = unsafe {
        (sc.add(16).read(), sc.add(17).read(), sc.add(15).read())
    };

    // uc_sigmask is right after sigcontext.
    let old_blocked = unsafe { *((sc_base + SIGCONTEXT_SIZE) as *const u64) };

    // Restore signal mask.
    let unblockable = (1u64 << (SIGKILL - 1)) | (1u64 << (SIGSTOP - 1));
    libkernel::process::with_process(pid, |p| {
        p.signal.blocked = old_blocked & !unblockable;
    });

    // Restore the SYSCALL saved frame.
    let frame_ptr = get_saved_frame_ptr();
    unsafe {
        let frame = &mut *frame_ptr;
        frame.rcx = orig_rip;
        frame.r11 = orig_rflags;
        frame.rdi = rdi;
        frame.rsi = rsi;
        frame.rdx = rdx;
        frame.r8 = r8;
        frame.r9 = r9;
        frame.r10 = r10;
    }

    set_saved_user_rsp(orig_rsp);

    // Returned as syscall_dispatch result → pushed/popped by asm stub → rax.
    rax as i64
}

// ---------------------------------------------------------------------------
// rt_sigaction / rt_sigprocmask

pub fn sys_rt_sigaction(signum: u64, act_ptr: u64, oldact_ptr: u64, sigsetsize: u64) -> i64 {
    use libkernel::signal::*;

    if sigsetsize != 8 {
        return -errno::EINVAL;
    }
    let sig = signum as u8;
    if sig < 1 || sig as usize > NUM_SIGNALS || sig == SIGKILL || sig == SIGSTOP {
        return -errno::EINVAL;
    }

    let pid = libkernel::process::current_pid();
    let idx = (sig - 1) as usize;

    // Write old action to user memory if requested.
    if oldact_ptr != 0 {
        if !validate_user_buf(oldact_ptr, 32) {
            return -errno::EFAULT;
        }
        let old = match libkernel::process::with_process_ref(pid, |p| p.signal.actions[idx]) {
            Some(a) => a,
            None => return -errno::ESRCH,
        };
        unsafe {
            let p = oldact_ptr as *mut u64;
            p.write(old.handler);
            p.add(1).write(old.flags);
            p.add(2).write(old.restorer);
            p.add(3).write(old.mask);
        }
    }

    // Read new action from user memory if provided.
    if act_ptr != 0 {
        if !validate_user_buf(act_ptr, 32) {
            return -errno::EFAULT;
        }
        let action = unsafe {
            let p = act_ptr as *const u64;
            SigAction {
                handler: p.read(),
                flags: p.add(1).read(),
                restorer: p.add(2).read(),
                mask: p.add(3).read(),
            }
        };
        libkernel::process::with_process(pid, |p| {
            p.signal.actions[idx] = action;
        });
    }

    0
}

pub fn sys_rt_sigprocmask(how: u64, set_ptr: u64, oldset_ptr: u64, sigsetsize: u64) -> i64 {
    use libkernel::signal::*;

    if sigsetsize != 8 {
        return -errno::EINVAL;
    }

    let pid = libkernel::process::current_pid();

    // Write old mask to user memory.
    if oldset_ptr != 0 {
        if !validate_user_buf(oldset_ptr, 8) {
            return -errno::EFAULT;
        }
        let old_mask = match libkernel::process::with_process_ref(pid, |p| p.signal.blocked) {
            Some(m) => m,
            None => return -errno::ESRCH,
        };
        unsafe { *(oldset_ptr as *mut u64) = old_mask; }
    }

    // Apply new mask if provided.
    if set_ptr != 0 {
        if !validate_user_buf(set_ptr, 8) {
            return -errno::EFAULT;
        }
        let set = unsafe { *(set_ptr as *const u64) };
        let unblockable = (1u64 << (SIGKILL - 1)) | (1u64 << (SIGSTOP - 1));

        libkernel::process::with_process(pid, |p| {
            match how {
                SIG_BLOCK => p.signal.blocked |= set & !unblockable,
                SIG_UNBLOCK => p.signal.blocked &= !set,
                SIG_SETMASK => p.signal.blocked = set & !unblockable,
                _ => {}
            }
        });

        if how > SIG_SETMASK {
            return -errno::EINVAL;
        }
    }

    0
}

// ---------------------------------------------------------------------------
// kill

/// `kill` (syscall 62) — send a signal to a process.
pub fn sys_kill(pid_arg: u64, sig: u64) -> i64 {
    use libkernel::signal::*;
    use libkernel::process;

    let sig = sig as u8;
    if sig < 1 || sig as usize > NUM_SIGNALS {
        return -errno::EINVAL;
    }

    let target_pid = process::ProcessId::from_raw(pid_arg);

    let signal_thread = match process::with_process(target_pid, |p| {
        p.signal.queue(sig);
        p.signal_thread
    }) {
        Some(t) => t,
        None => return -errno::ESRCH,
    };

    // Wake the target only if it is in an interruptible block.
    if let Some(idx) = signal_thread {
        libkernel::task::scheduler::unblock(idx);
    }

    0
}
