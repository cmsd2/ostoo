//! Raw syscall wrappers via inline assembly (SYSCALL instruction).

use core::arch::asm;

// Syscall numbers (must match osl/src/syscall_nr.rs)
pub const SYS_READ: u64 = 0;
pub const SYS_WRITE: u64 = 1;
pub const SYS_OPEN: u64 = 2;
pub const SYS_CLOSE: u64 = 3;
pub const SYS_BRK: u64 = 12;
pub const SYS_EXIT: u64 = 60;
pub const SYS_WAIT4: u64 = 61;
pub const SYS_GETCWD: u64 = 79;
pub const SYS_CHDIR: u64 = 80;
pub const SYS_GETDENTS64: u64 = 217;
pub const SYS_EXIT_GROUP: u64 = 231;
pub const SYS_SPAWN: u64 = 500;

#[inline(always)]
pub unsafe fn syscall0(nr: u64) -> i64 {
    let ret: i64;
    asm!(
        "syscall",
        in("rax") nr,
        out("rcx") _,
        out("r11") _,
        lateout("rax") ret,
        options(nostack),
    );
    ret
}

#[inline(always)]
pub unsafe fn syscall1(nr: u64, a1: u64) -> i64 {
    let ret: i64;
    asm!(
        "syscall",
        in("rax") nr,
        in("rdi") a1,
        out("rcx") _,
        out("r11") _,
        lateout("rax") ret,
        options(nostack),
    );
    ret
}

#[inline(always)]
pub unsafe fn syscall2(nr: u64, a1: u64, a2: u64) -> i64 {
    let ret: i64;
    asm!(
        "syscall",
        in("rax") nr,
        in("rdi") a1,
        in("rsi") a2,
        out("rcx") _,
        out("r11") _,
        lateout("rax") ret,
        options(nostack),
    );
    ret
}

#[inline(always)]
pub unsafe fn syscall3(nr: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    let ret: i64;
    asm!(
        "syscall",
        in("rax") nr,
        in("rdi") a1,
        in("rsi") a2,
        in("rdx") a3,
        out("rcx") _,
        out("r11") _,
        lateout("rax") ret,
        options(nostack),
    );
    ret
}

#[inline(always)]
pub unsafe fn syscall4(nr: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> i64 {
    let ret: i64;
    asm!(
        "syscall",
        in("rax") nr,
        in("rdi") a1,
        in("rsi") a2,
        in("rdx") a3,
        in("r10") a4,
        out("rcx") _,
        out("r11") _,
        lateout("rax") ret,
        options(nostack),
    );
    ret
}

// ---- Typed wrappers ----

pub fn write(fd: u32, buf: &[u8]) -> i64 {
    unsafe { syscall3(SYS_WRITE, fd as u64, buf.as_ptr() as u64, buf.len() as u64) }
}

pub fn read(fd: u32, buf: &mut [u8]) -> i64 {
    unsafe { syscall3(SYS_READ, fd as u64, buf.as_mut_ptr() as u64, buf.len() as u64) }
}

pub fn open(path: *const u8, flags: u64, mode: u64) -> i64 {
    unsafe { syscall3(SYS_OPEN, path as u64, flags, mode) }
}

pub fn close(fd: u32) -> i64 {
    unsafe { syscall1(SYS_CLOSE, fd as u64) }
}

pub fn exit(code: i32) -> ! {
    unsafe { syscall1(SYS_EXIT, code as u64); }
    loop {}
}

pub fn brk(addr: u64) -> i64 {
    unsafe { syscall1(SYS_BRK, addr) }
}

pub fn getcwd(buf: &mut [u8]) -> i64 {
    unsafe { syscall2(SYS_GETCWD, buf.as_mut_ptr() as u64, buf.len() as u64) }
}

pub fn chdir(path: *const u8) -> i64 {
    unsafe { syscall1(SYS_CHDIR, path as u64) }
}

pub fn getdents64(fd: u32, buf: &mut [u8]) -> i64 {
    unsafe { syscall3(SYS_GETDENTS64, fd as u64, buf.as_mut_ptr() as u64, buf.len() as u64) }
}

pub fn spawn(path: &[u8], argv: &[*const u8], argc: usize) -> i64 {
    unsafe {
        syscall4(
            SYS_SPAWN,
            path.as_ptr() as u64,
            path.len() as u64,
            argv.as_ptr() as u64,
            argc as u64,
        )
    }
}

pub fn wait4(pid: i64, status: *mut u32, options: u64) -> i64 {
    unsafe { syscall3(SYS_WAIT4, pid as u64, status as u64, options) }
}
