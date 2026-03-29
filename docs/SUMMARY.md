# Summary

[Project Status](status.md)

---

# Architecture

- [Scheduler](scheduler.md)
- [Paging](paging.md)
- [mmap Design](mmap-design.md)
- [Graphics Subsystem](graphics-design.md)
- [FPU / SSE State](sse-fpu.md)
- [File Descriptors & Pipes](file-descriptors.md)
- [Actor System](actors.md)

# Drivers

- [virtio-blk](virtio-blk.md)
- [VirtIO 9P](virtio-9p.md)
- [exFAT Filesystem](exfat.md)
- [VFS Layer](vfs.md)

# IPC & Async I/O

- [IPC Channels](ipc-channels.md)
- [Completion Port Design](completion-port-design.md)
- [Scheduler Donate](scheduler-donate.md)

# Signals

- [Signal Support](signals.md)

# Process Model

- [Process Spawning](process-spawning.md)
- [Userspace Plan](userspace-plan.md)

# Syscalls

- [read (0)](syscalls/read.md)
- [write (1)](syscalls/write.md)
- [open (2)](syscalls/open.md)
- [close (3)](syscalls/close.md)
- [fstat (5)](syscalls/fstat.md)
- [lseek (8)](syscalls/lseek.md)
- [mmap (9)](syscalls/mmap.md)
- [mprotect (10)](syscalls/mprotect.md)
- [munmap (11)](syscalls/munmap.md)
- [brk (12)](syscalls/brk.md)
- [rt_sigaction (13)](syscalls/rt_sigaction.md)
- [rt_sigprocmask (14)](syscalls/rt_sigprocmask.md)
- [ioctl (16)](syscalls/ioctl.md)
- [writev (20)](syscalls/writev.md)
- [pipe / pipe2 (22, 293)](syscalls/pipe2.md)
- [madvise (28)](syscalls/madvise.md)
- [dup2 (33)](syscalls/dup2.md)
- [getpid (39)](syscalls/getpid.md)
- [clone (56)](syscalls/clone.md)
- [execve (59)](syscalls/execve.md)
- [exit / exit_group (60, 231)](syscalls/exit.md)
- [wait4 (61)](syscalls/wait4.md)
- [fcntl (72)](syscalls/fcntl.md)
- [getcwd (79)](syscalls/getcwd.md)
- [chdir (80)](syscalls/chdir.md)
- [sigaltstack (131)](syscalls/sigaltstack.md)
- [arch_prctl (158)](syscalls/arch_prctl.md)
- [futex (202)](syscalls/futex.md)
- [sched_getaffinity (204)](syscalls/sched_getaffinity.md)
- [getdents64 (217)](syscalls/getdents64.md)
- [set_tid_address (218)](syscalls/set_tid_address.md)
- [clock_gettime (228)](syscalls/clock_gettime.md)
- [set_robust_list (273)](syscalls/set_robust_list.md)
- [getrandom (318)](syscalls/getrandom.md)
- [spawn (500)](syscalls/spawn.md)

# Userspace

- [Userspace Shell](userspace-shell.md)
- [Cross-Compiling C](cross-compiling-c.md)
- [Cross-Compilation (Rust host)](cross-compilation.md)
- [Rust Userspace Cross-Compilation](rust-userspace-cross-compilation.md)

# Hardware

- [APIC & IO APIC](apic-ioapic.md)
- [LAPIC Timer](lapic-timer.md)

# Design Documents

- [Microkernel Design](microkernel-design.md)
- [Networking Design](networking-design.md)

# Audits

- [Code Quality Audit](code-audit.md)
- [Unsafe Code Audit](unsafe-audit.md)
- [Testing](testing.md)
