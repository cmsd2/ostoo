# exit (nr 60) / exit_group (nr 231)

## Linux Signature

```c
void _exit(int status);          // nr 60
void exit_group(int status);     // nr 231
```

## Description

- `exit` (60): Terminates the calling thread.
- `exit_group` (231): Terminates all threads in the calling process.

Both are handled identically in ostoo since each process currently has exactly one thread.

## Current Implementation

1. Looks up the current PID.
2. If it's a user process (not `ProcessId::KERNEL`), calls `terminate_process`:
   - Logs `pid N exited with code C` to serial.
   - **Unblocks vfork parent:** If this process was created by `clone(CLONE_VFORK)` and has not yet called `execve`, unblocks the parent thread so it can resume. Clears `vfork_parent_thread`.
   - **Closes all fds:** Releases IRQ handles, completion ports, pipes, channels, etc. while the process's page tables are still active.
   - **Frees user address space:** Switches CR3 to the kernel boot PML4 and updates the scheduler's thread record, then frees all user-half page tables and data frames via `cleanup_user_address_space`. Skipped for `CLONE_VM` children (shared PML4 still used by parent).
   - **Marks zombie:** Sets the process state to `Zombie` with the exit code.
   - **Wakes parent:** Queues `SIGCHLD` and unblocks the parent's `wait_thread` if set.
   - **Yields + dies:** Donates remaining quantum to the parent, calls `yield_now()`, then `kill_current_thread()` marks the thread as `Dead`.
3. If it's a kernel thread: prints a halt message and calls `kill_current_thread()`.

Zombie processes are reaped by `waitpid` (when a parent collects exit status) or lazily by `reap_zombies()` at the start of `spawn_process`.

**Source:** `osl/src/syscalls/process.rs` — `sys_exit`, `libkernel/src/process.rs` — `terminate_process`

### CR3 safety on exit

The process's PML4 frame must not be freed while CR3 still references it.
The frame allocator uses an intrusive free-list that overwrites the first 8
bytes of freed frames immediately; if the scheduler later reschedules the
dying thread (before `kill_current_thread` runs), a TLB refill through the
corrupted PML4 would triple-fault.  `terminate_process` therefore switches
to the kernel boot PML4 (stored in `KERNEL_PML4_PHYS` during
`memory::init_services`) and updates the scheduler via `set_current_cr3`
before calling `cleanup_user_address_space`.

## Future Work

- Properly distinguish `exit` (single thread) from `exit_group` (all threads) once multi-threaded processes are supported.
- Service auto-cleanup: remove service registry entries on process exit.
