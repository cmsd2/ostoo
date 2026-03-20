# sigaltstack (nr 131)

## Linux Signature

```c
int sigaltstack(const stack_t *ss, stack_t *old_ss);
```

## Description

Set and/or get the alternate signal stack.

## Current Implementation

**Stub:** Returns 0 (success) unconditionally. No signal support is implemented. Rust's standard library calls `sigaltstack` during runtime init to set up an alternate stack for signal handlers.

**Source:** `osl/src/dispatch.rs` — match arm returns `0`
