# munmap (nr 11)

## Linux Signature

```c
int munmap(void *addr, size_t length);
```

## Description

Removes mappings for the specified address range, causing further references to addresses within the range to generate page faults.

## Current Implementation

Fully implemented. Validates arguments, splits/removes VMAs, unmaps page table entries, frees physical frames to the free list, and flushes the TLB.

**Source:** `osl/src/syscalls/mem.rs` — `sys_munmap`

### Behaviour

1. `addr` must be page-aligned; `length` must be > 0. Returns `-EINVAL` otherwise.
2. `length` is rounded up to the next page boundary.
3. Overlapping VMAs are split or removed:
   - **Entire VMA consumed** — removed from `vma_map`.
   - **Front consumed** — VMA start/len adjusted forward.
   - **Tail consumed** — VMA len shortened.
   - **Middle consumed** — VMA split into two fragments.
4. Each page in the unmapped range is removed from the page table.
   Physical frames are released via refcount-aware logic: shared frames
   (from `MAP_SHARED` mappings) are only freed when their reference count
   reaches 0 (i.e. all processes have unmapped the frame and the backing
   `shmem_create` fd has been closed).  Non-shared frames are freed
   immediately.
5. If no VMAs overlap the range, returns 0 (Linux no-op semantics).

### Lock ordering

`PROCESS_TABLE` is acquired first (to call `munmap_vmas`), then released before acquiring `MEMORY` (to unmap and free pages). Same ordering as `sys_mmap` and `sys_brk`.

## Errors

| Error | Condition |
|-------|-----------|
| `-EINVAL` | `addr` not page-aligned, `length` is 0, or caller is kernel |
