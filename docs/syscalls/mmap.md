# mmap (nr 9)

## Linux Signature

```c
void *mmap(void *addr, size_t length, int prot, int flags, int fd, off_t offset);
```

## Description

Maps pages of memory into the calling process's address space.

## Current Implementation

- **Only `MAP_PRIVATE | MAP_ANONYMOUS` is supported.** File-backed mappings return `-ENOSYS` (-38).
- **`MAP_FIXED` with a non-zero addr is rejected** with `-ENOSYS`.
- The `prot` argument is ignored; all mappings are created with `PRESENT | WRITABLE | USER_ACCESSIBLE | NO_EXECUTE`.
- The `fd` and `offset` arguments are ignored (the 6th syscall argument `offset` in r9 is not reliably passed through the current assembly stub, but this is fine for anonymous mappings where offset is always 0).
- Addresses are allocated using a bump-down allocator starting at `0x0000_4000_0000_0000`. Each call decrements `mmap_next` by the page-aligned length.
- Physical frames are allocated one page at a time via `alloc_dma_pages(1)`, zeroed, and mapped into the process's page table.
- The region `(base, aligned_length)` is recorded in `Process.mmap_regions`.
- Returns the virtual address of the mapped region on success, or `-ENOMEM` (-12) on failure.

**Lock ordering:** Process table lock is acquired and released first (to read `mmap_next` and `pml4_phys`), then the memory lock is held for allocation/mapping, then the process table lock is re-acquired to update state. This avoids nested lock deadlocks.

**Source:** `osl/src/syscalls/mem.rs` — `sys_mmap`

## Future Work

- Support `MAP_FIXED` for relocating mappings.
- Honour `prot` flags (read-only, execute, etc.) by setting appropriate page table flags.
- Support file-backed mappings once a VFS file descriptor table exists.
- Implement a proper virtual memory area (VMA) tracker instead of the simple bump allocator.
- Handle the 6th argument (`offset`) properly if needed for file-backed mmap.
