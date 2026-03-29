# clock_gettime (nr 228)

## Linux Signature

```c
int clock_gettime(clockid_t clk_id, struct timespec *tp);
```

## Description

Retrieves the time of the specified clock.

## Current Implementation

**Stub:** Writes zero for both `tv_sec` and `tv_nsec` in the user-provided `timespec` struct. The `clk_id` parameter is accepted but ignored. Returns 0 (success).

This satisfies Rust std's runtime init which calls `clock_gettime(CLOCK_MONOTONIC, ...)` but doesn't depend on the actual time value.

**Source:** `osl/src/syscalls/misc.rs` — `sys_clock_gettime`

## Errors

| Errno | Condition |
|-------|-----------|
| `-EFAULT` (-14) | Invalid `tp` pointer |

## Future Work

- Return real time based on the PIT/HPET/TSC timer.
- Distinguish `CLOCK_REALTIME`, `CLOCK_MONOTONIC`, etc.
