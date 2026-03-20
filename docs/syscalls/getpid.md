# getpid (nr 39)

## Linux Signature

```c
pid_t getpid(void);
```

## Description

Returns the process ID of the calling process.

## Current Implementation

Returns `current_pid().as_u64()`. Always succeeds (no error return).

**Source:** `osl/src/dispatch.rs` — `sys_getpid`
