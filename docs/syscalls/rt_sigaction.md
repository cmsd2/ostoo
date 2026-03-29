# rt_sigaction (nr 13)

## Linux Signature

```c
int rt_sigaction(int signum, const struct sigaction *act,
                 struct sigaction *oldact, size_t sigsetsize);
```

## Description

Examine and change a signal action.

## Current Implementation

**Stub:** Returns 0 (success) unconditionally. No signal support is implemented. musl's runtime init calls `rt_sigaction` to install default signal handlers; the stub allows this to succeed silently.

**Source:** `osl/src/signal.rs` — `sys_rt_sigaction`
