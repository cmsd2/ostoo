# spawn (nr 500) — removed

The custom `spawn` syscall has been removed. Process creation now uses the
standard Linux `clone(CLONE_VM|CLONE_VFORK)` + `execve` path.

See [clone.md](clone.md) and [execve.md](execve.md).

musl's `posix_spawn` and Rust's `std::process::Command` use these standard
syscalls automatically.
