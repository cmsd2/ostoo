//! Linux x86-64 syscall numbers.

pub const SYS_READ: u64 = 0;
pub const SYS_WRITE: u64 = 1;
pub const SYS_OPEN: u64 = 2;
pub const SYS_CLOSE: u64 = 3;
pub const SYS_FSTAT: u64 = 5;
pub const SYS_LSEEK: u64 = 8;
pub const SYS_MMAP: u64 = 9;
pub const SYS_MPROTECT: u64 = 10;
pub const SYS_MUNMAP: u64 = 11;
pub const SYS_BRK: u64 = 12;
pub const SYS_IOCTL: u64 = 16;
pub const SYS_WRITEV: u64 = 20;
pub const SYS_EXIT: u64 = 60;
pub const SYS_WAIT4: u64 = 61;
pub const SYS_GETCWD: u64 = 79;
pub const SYS_CHDIR: u64 = 80;
pub const SYS_ARCH_PRCTL: u64 = 158;
pub const SYS_FUTEX: u64 = 202;
pub const SYS_GETDENTS64: u64 = 217;
pub const SYS_SET_TID_ADDRESS: u64 = 218;
pub const SYS_EXIT_GROUP: u64 = 231;
pub const SYS_SET_ROBUST_LIST: u64 = 273;
pub const SYS_SPAWN: u64 = 500;
