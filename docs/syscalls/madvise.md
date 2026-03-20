# madvise (nr 28)

## Linux Signature

```c
int madvise(void *addr, size_t length, int advice);
```

## Description

Give advice about use of memory.

## Current Implementation

**Stub:** Returns 0 (success) unconditionally. All advice is ignored. musl and Rust std may call `madvise(MADV_DONTNEED)` on freed memory regions.

**Source:** `osl/src/dispatch.rs` — match arm returns `0`
