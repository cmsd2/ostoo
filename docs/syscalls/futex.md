# futex (nr 202)

## Linux Signature

```c
long futex(uint32_t *uaddr, int futex_op, uint32_t val,
           const struct timespec *timeout, uint32_t *uaddr2, uint32_t val3);
```

## Description

Provides fast user-space locking primitives. `FUTEX_WAIT` blocks the calling thread
until the value at `uaddr` changes; `FUTEX_WAKE` wakes threads waiting on `uaddr`.

## Current Implementation

Always returns 0 (success). Each process is single-threaded, so musl's internal locks
(used by stdio, malloc, etc.) are never contended. The `FUTEX_WAIT` path is never
reached in practice, and `FUTEX_WAKE` returning 0 (no waiters woken) is correct.

**Source:** `libkernel/src/syscall.rs` — inline in `syscall_dispatch`

## Future Work

- Implement `FUTEX_WAIT` and `FUTEX_WAKE` properly once multi-threaded user
  processes are supported.
- Support `FUTEX_WAIT_BITSET` and other operations used by musl's condition variables.
