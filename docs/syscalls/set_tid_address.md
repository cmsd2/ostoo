# set_tid_address (nr 218)

## Linux Signature

```c
pid_t set_tid_address(int *tidptr);
```

## Description

Sets the `clear_child_tid` pointer for the calling thread. When the thread exits, the kernel writes 0 to `*tidptr` and wakes any futex waiters. Returns the caller's TID.

## Current Implementation

- Ignores the `tidptr` argument entirely (no `clear_child_tid` tracking).
- Returns the current process's PID (used as TID since each process is single-threaded).

This is sufficient for musl's early startup, which calls `set_tid_address` to discover its own TID.

**Source:** `osl/src/dispatch.rs` — `sys_set_tid_address`

## Future Work

- Store `tidptr` in the thread/process structure.
- On thread exit, write 0 to `*tidptr` and perform a futex wake (needed for `pthread_join`).
- Return a per-thread TID rather than PID once multi-threading is supported.
