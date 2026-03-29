# mprotect (nr 10)

## Linux Signature

```c
int mprotect(void *addr, size_t len, int prot);
```

## Description

Changes the access protections for the calling process's memory pages in the range `[addr, addr+len)`.

## Implementation

1. Validates `addr` is page-aligned and `len > 0` (returns `-EINVAL` otherwise).
2. Aligns `len` up to the next page boundary.
3. Splits/updates VMAs in the range via `Process::mprotect_vmas()`:
   - Entire VMA overlap: updates prot in place.
   - Partial overlap (front, tail, middle): splits VMA and sets new prot on the affected portion.
4. Converts `prot` to x86-64 page table flags (`prot_to_page_flags()`):
   - `PROT_NONE` → `USER_ACCESSIBLE` only (no `PRESENT` — any access faults).
   - `PROT_READ` → `PRESENT | USER_ACCESSIBLE | NO_EXECUTE`.
   - `PROT_WRITE` → adds `WRITABLE`.
   - `PROT_EXEC` → removes `NO_EXECUTE`.
5. Updates page table entries via `MemoryServices::update_user_page_flags()` with TLB flush.
6. Returns 0 on success.

Lock ordering: PROCESS_TABLE first (VMA split), then MEMORY (page table update).

Returns 0 (no-op) if no VMAs overlap the requested range (Linux semantics).

**Source:** `osl/src/syscalls/mem.rs` (`sys_mprotect`), `libkernel/src/process.rs` (`mprotect_vmas`), `libkernel/src/memory/mod.rs` (`update_user_page_flags`)
