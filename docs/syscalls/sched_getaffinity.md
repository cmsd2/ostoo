# sched_getaffinity (nr 204)

## Linux Signature

```c
int sched_getaffinity(pid_t pid, size_t cpusetsize, cpu_set_t *mask);
```

## Description

Get a thread's CPU affinity mask.

## Current Implementation

Zeroes the user-provided mask buffer, then sets bit 0 (CPU 0 only). Returns `cpusetsize` (the number of bytes written). ostoo is a single-CPU kernel.

Rust's standard library calls `sched_getaffinity` during runtime init to determine available parallelism.

**Source:** `osl/src/dispatch.rs` — `sys_sched_getaffinity`

## Errors

| Errno | Condition |
|-------|-----------|
| `-EINVAL` (-22) | `cpusetsize` is 0 |
| `-EFAULT` (-14) | Invalid `mask` pointer |
