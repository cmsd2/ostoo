# mprotect (nr 10)

## Linux Signature

```c
int mprotect(void *addr, size_t len, int prot);
```

## Description

Changes the access protections for the calling process's memory pages in the range `[addr, addr+len)`.

## Current Implementation

Always returns 0 (success) without modifying any page table entries. All arguments are ignored.

This is sufficient for musl's startup, which calls `mprotect` to mark certain regions read-only after initialisation.

**Source:** `osl/src/dispatch.rs` — inline in `syscall_dispatch`

## Future Work

- Actually update page table flags (remove `WRITABLE`, add/remove `NO_EXECUTE`, etc.) to honour the requested protection.
- Return `-ENOMEM` if the address range is not mapped.
- Validate alignment of `addr` (must be page-aligned).
