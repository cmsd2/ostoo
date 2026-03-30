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

#[inline(always)]
pub unsafe fn syscall5(nr: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> i64 {
    let ret: i64;
    asm!(
        "syscall",
        in("rax") nr,
        in("rdi") a1,
        in("rsi") a2,
        in("rdx") a3,
        in("r10") a4,
        in("r8") a5,
        out("rcx") _,
        out("r11") _,
        lateout("rax") ret,
        options(nostack),
    );
    ret
}

#[inline(always)]
pub unsafe fn syscall6(nr: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: u64) -> i64 {
    let ret: i64;
    asm!(
        "syscall",
        in("rax") nr,
        in("rdi") a1,
        in("rsi") a2,
        in("rdx") a3,
        in("r10") a4,
        in("r8") a5,
        in("r9") a6,
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

pub fn wait4(pid: i64, status: *mut u32, options: u64) -> i64 {
    unsafe { syscall3(SYS_WAIT4, pid as u64, status as u64, options) }
}

// Additional syscall numbers
pub const SYS_MMAP: u64 = 9;
pub const SYS_DUP2: u64 = 33;
pub const SYS_CLONE: u64 = 56;
pub const SYS_EXECVE: u64 = 59;
pub const SYS_GETPID: u64 = 39;
pub const SYS_PIPE2: u64 = 293;
pub const SYS_KILL: u64 = 62;

pub fn pipe2(fds: &mut [i32; 2], flags: u32) -> i64 {
    unsafe { syscall2(SYS_PIPE2, fds.as_mut_ptr() as u64, flags as u64) }
}

pub fn dup2(oldfd: i32, newfd: i32) -> i64 {
    unsafe { syscall2(SYS_DUP2, oldfd as u64, newfd as u64) }
}

/// `clone(flags, child_stack, ...)` — we use the CLONE_VM|CLONE_VFORK|SIGCHLD
/// calling convention.  `child_stack` = 0 means share parent's stack.
pub fn clone(flags: u64) -> i64 {
    unsafe { syscall5(SYS_CLONE, flags, 0, 0, 0, 0) }
}

/// `execve(path, argv, envp)` — all pointers must be in user memory.
pub fn execve(path: *const u8, argv: *const *const u8, envp: *const *const u8) -> i64 {
    unsafe { syscall3(SYS_EXECVE, path as u64, argv as u64, envp as u64) }
}

pub fn getpid() -> i64 {
    unsafe { syscall0(SYS_GETPID) }
}

pub fn kill(pid: i64, sig: i32) -> i64 {
    unsafe { syscall2(SYS_KILL, pid as u64, sig as u64) }
}
