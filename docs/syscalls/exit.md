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
   - Prints `[kernel] pid N exited with code C`.
   - Marks the process as a zombie via `mark_zombie(pid, code)`.
   - Does **not** reap the process immediately (still running on its kernel stack).
3. If it's a kernel thread: prints a halt message.
4. Calls `kill_current_thread()`, which marks the scheduler thread as `Dead` and spins until the timer preempts it. The thread is never re-queued.

Zombie processes are reaped lazily by `reap_zombies()`, which is called at the start of `spawn_blob` and `spawn_process` to free kernel stacks before allocating new ones.

**Source:** `libkernel/src/syscall.rs` — `sys_exit`

## Future Work

- Implement `waitpid` so a parent process can collect exit status and trigger reaping.
- Properly distinguish `exit` (single thread) from `exit_group` (all threads) once multi-threaded processes are supported.
- Free user-space page tables and physical frames on process exit (currently leaked).
- Signal handling (SIGCHLD to parent).
