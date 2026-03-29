//! Miscellaneous syscalls: arch_prctl, getrandom, clock_gettime, sched_getaffinity.

use crate::errno;
use crate::user_mem::{validate_user_buf, user_slice_mut};

pub(crate) fn sys_arch_prctl(code: u64, addr: u64) -> i64 {
    const ARCH_SET_FS: u64 = 0x1002;
    match code {
        ARCH_SET_FS => {
            unsafe { libkernel::msr::write_fs_base(addr); }
            0
        }
        _ => -errno::EINVAL,
    }
}

pub(crate) fn sys_getrandom(buf: u64, count: u64, _flags: u64) -> i64 {
    let user_buf = match user_slice_mut(buf, count) {
        Ok(s) => s,
        Err(e) => return e,
    };
    // Simple xorshift64* PRNG seeded from TSC.
    let mut state: u64 = unsafe { core::arch::x86_64::_rdtsc() };
    if state == 0 { state = 0xDEAD_BEEF_CAFE_BABE; }
    for byte in user_buf.iter_mut() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *byte = state as u8;
    }
    count as i64
}

pub(crate) fn sys_sched_getaffinity(_pid: u64, cpusetsize: u64, mask_ptr: u64) -> i64 {
    if cpusetsize == 0 {
        return -errno::EINVAL;
    }
    let user_buf = match user_slice_mut(mask_ptr, cpusetsize) {
        Ok(s) => s,
        Err(e) => return e,
    };
    for b in user_buf.iter_mut() { *b = 0; }
    user_buf[0] = 1;
    cpusetsize as i64
}

pub(crate) fn sys_clock_gettime(_clk_id: u64, tp: u64) -> i64 {
    if !validate_user_buf(tp, 16) {
        return -errno::EFAULT;
    }
    unsafe {
        *(tp as *mut u64) = 0;         // tv_sec
        *((tp + 8) as *mut u64) = 0;   // tv_nsec
    }
    0
}
