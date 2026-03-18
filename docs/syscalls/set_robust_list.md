# set_robust_list (nr 273)

## Linux Signature

```c
long set_robust_list(struct robust_list_head *head, size_t len);
```

## Description

Registers a list of robust futexes with the kernel. If a thread exits while holding a robust futex, the kernel marks it as dead and wakes waiters, preventing permanent deadlocks.

## Current Implementation

Always returns 0 (success) without recording anything. Both arguments are ignored.

This is sufficient for musl's startup, which registers a robust list as part of thread initialisation.

**Source:** `libkernel/src/syscall.rs` — inline in `syscall_dispatch`

## Future Work

- Store the robust list head pointer in the thread structure.
- On thread exit, walk the robust list and wake any futex waiters on held locks.
- Implement `get_robust_list` (nr 274) for completeness.
