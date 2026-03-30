# mmap (nr 9)

## Linux Signature

```c
void *mmap(void *addr, size_t length, int prot, int flags, int fd, off_t offset);
```

## Description

Maps pages of memory into the calling process's address space.

## Current Implementation

Supports anonymous mappings, file-backed private mappings, and shared
memory mappings via `shmem_create` fds.

**Source:** `osl/src/syscalls/mem.rs` — `sys_mmap`

### Supported modes

| Flags | fd | Behaviour |
|-------|----|-----------|
| `MAP_PRIVATE \| MAP_ANONYMOUS` | ignored | Allocate fresh zeroed pages (most common) |
| `MAP_PRIVATE` | file fd | Copy file content into private pages |
| `MAP_SHARED` | shmem fd | Map the shared memory object's physical frames |
| `MAP_SHARED \| MAP_ANONYMOUS` | — | Returns `-EINVAL` (not supported without fork) |

`MAP_SHARED` and `MAP_PRIVATE` are mutually exclusive; if both or neither
are set, `-EINVAL` is returned.

### Protection flags (`prot`)

| Flag | Value | Page table flags |
|------|-------|------------------|
| `PROT_READ` | 0x1 | `PRESENT \| USER_ACCESSIBLE` |
| `PROT_WRITE` | 0x2 | `+ WRITABLE` |
| `PROT_EXEC` | 0x4 | removes `NO_EXECUTE` |

If `prot` is 0 (PROT_NONE), pages are mapped as present but not
accessible from userspace (guard pages).

### Address selection

- **Without `MAP_FIXED`**: a top-down gap finder scans the VMA map for a
  free gap in the user address range (`0x0000_0010_0000` –
  `0x0000_4000_0000_0000`), starting from the top.  The returned address
  is the start of the gap.
- **`MAP_FIXED`**: `addr` must be page-aligned and non-zero.  Any
  existing mappings in the range are implicitly unmapped before the new
  mapping is created.

### MAP_SHARED with shmem fd

When `MAP_SHARED` is specified with a file descriptor from
`shmem_create(508)`, the kernel maps the shared memory object's existing
physical frames into the caller's page table.  Each frame's reference
count is incremented so the frame is not freed until all processes have
unmapped it and the last fd is closed.

The `offset` argument selects the starting frame within the shmem object
(must be page-aligned).

### File-backed MAP_PRIVATE

When `MAP_PRIVATE` is specified with a file fd (from `open`), the file's
content is copied into freshly allocated pages.  The pages are private to
the calling process — writes do not affect the underlying file or other
mappings.

### VMA tracking

Each mapping is recorded as a `Vma` (virtual memory area) in the
process's `vma_map` (`BTreeMap<u64, Vma>`), tracking start address,
length, protection, flags, fd, and offset.  The VMA map is used by
`munmap`, `mprotect`, the gap finder, and process cleanup.

### Lock ordering

`PROCESS_TABLE` is acquired first (to read VMA state and `pml4_phys`),
then released before acquiring `MEMORY` (to allocate/map pages), then
`PROCESS_TABLE` is re-acquired to update state.  This avoids nested lock
deadlocks.

## Errors

| Error | Condition |
|-------|-----------|
| `-EINVAL` | Length is 0, `MAP_SHARED` and `MAP_PRIVATE` both/neither set, `MAP_SHARED \| MAP_ANONYMOUS`, unaligned `MAP_FIXED` addr, unaligned offset |
| `-ENOMEM` | Physical memory exhausted or no virtual address gap found |
| `-ENODEV` | `MAP_SHARED` fd is not a shmem object |
| `-EBADF` | File-backed `MAP_PRIVATE` with an invalid fd |

## See also

- [munmap (11)](munmap.md) — unmap pages
- [mprotect (10)](mprotect.md) — change page protection
- [shmem_create (508)](shmem_create.md) — create shared memory fd
- [mmap Design](../mmap-design.md) — design document with phase roadmap
