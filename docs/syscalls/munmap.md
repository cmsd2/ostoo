# munmap (nr 11)

## Linux Signature

```c
int munmap(void *addr, size_t length);
```

## Description

Removes mappings for the specified address range, causing further references to addresses within the range to generate page faults.

## Current Implementation

Always returns 0 (success) without actually unmapping anything. Physical frames and page table entries are leaked.

**Source:** `osl/src/dispatch.rs` — inline in `syscall_dispatch`

## Future Work

- Walk the process's page table and unmap entries in the given range.
- Free the underlying physical frames back to the frame allocator.
- Remove the region from `Process.mmap_regions`.
- Return `-EINVAL` for invalid arguments (unaligned addr, zero length, etc.).
