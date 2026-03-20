# rt_sigprocmask (nr 14)

## Linux Signature

```c
int rt_sigprocmask(int how, const sigset_t *set, sigset_t *oldset, size_t sigsetsize);
```

## Description

Examine and change blocked signals.

## Current Implementation

**Stub:** Returns 0 (success) unconditionally. No signal support is implemented. musl's runtime and `posix_spawn` call `rt_sigprocmask` to configure the signal mask; the stub allows this to succeed silently.

**Source:** `osl/src/dispatch.rs` — match arm returns `0`
