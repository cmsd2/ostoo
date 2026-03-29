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
2. If it's a user process (not `ProcessId::KERNEL`):
   - Logs `pid N exited with code C` to serial.
   - **Unblocks vfork parent:** If this process was created by `clone(CLONE_VFORK)` and has not yet called `execve`, unblocks the parent thread so it can resume. Clears `vfork_parent_thread`.
   - Reads the process's `parent_pid` (separate lock acquisition).
   - Marks the process as a zombie via `mark_zombie(pid, code)`.
   - Checks if the parent process has a `wait_thread` set (meaning it's blocked in `waitpid`). If so, calls `scheduler::unblock()` to wake the parent.
3. If it's a kernel thread: prints a halt message.
4. Calls `kill_current_thread()`, which marks the scheduler thread as `Dead` and spins until the timer preempts it. The thread is never re-queued.

Zombie processes are reaped by `waitpid` (when a parent collects exit status) or lazily by `reap_zombies()` at the start of `spawn_process`.

**Source:** `osl/src/syscalls/process.rs` — `sys_exit`

## Future Work

- Properly distinguish `exit` (single thread) from `exit_group` (all threads) once multi-threaded processes are supported.
- Free user-space page tables and physical frames on process exit (currently leaked).
- Signal handling (SIGCHLD to parent).
- Close all open file descriptors on exit.
