# mmap Phased Design

## Overview

This document describes a phased plan for improving the virtual memory
management subsystem, starting from the current minimal `mmap` implementation
and building towards file-backed, shared mappings.

Each phase is self-contained and independently testable.

---

## Current State

### mmap (syscall 9)

- Anonymous-only (`MAP_ANONYMOUS`).  Returns `-ENOSYS` for file-backed.
- `MAP_FIXED` is rejected (`-ENOSYS`).
- Allocations bump down from `0x0000_4000_0000_0000` (`MMAP_BASE`).
- Pages are eagerly allocated, zeroed, and mapped.
- `prot` argument is honoured — page table flags are derived from
  `PROT_READ`, `PROT_WRITE`, `PROT_EXEC` via `Vma::page_table_flags()`.
- Regions are tracked as `BTreeMap<u64, Vma>` (`vma_map` in `Process`).
- `/proc/maps` displays actual `rwxp` flags from VMA metadata.

### munmap (syscall 11)

Implemented — unmaps pages, frees frames to the free list, and splits/removes
VMAs.  Supports partial unmaps (front, tail, middle split).

### mprotect (syscall 10)

Stub — returns 0 but does not change page permissions.

### Process cleanup on exit

`sys_exit` marks the process as a zombie and kills the thread.  No page
tables, frames, or VMA metadata are freed.

---

## Phase 1: VMA Tracking + PROT Flags ✓ (implemented)

**Goal:** Replace the bare `Vec<(u64, u64)>` region list with a proper VMA
(Virtual Memory Area) structure, and honour the `prot` argument in `mmap`.

### VMA struct

Add to `libkernel/src/process.rs` (or a new `libkernel/src/vma.rs`):

```rust
#[derive(Debug, Clone)]
pub struct Vma {
    pub start: u64,        // page-aligned
    pub len: u64,          // page-aligned
    pub prot: u32,         // PROT_READ | PROT_WRITE | PROT_EXEC
    pub flags: u32,        // MAP_PRIVATE | MAP_ANONYMOUS | MAP_SHARED | ...
    pub fd: Option<usize>, // file descriptor (Phase 5)
    pub offset: u64,       // file offset   (Phase 5)
}
```

Store VMAs in a `BTreeMap<u64, Vma>` keyed by start address, replacing
`mmap_regions: Vec<(u64, u64)>`.

### PROT flag translation

Map Linux `PROT_*` to x86-64 page table flags:

| Linux | x86-64 PTF | Notes |
|---|---|---|
| `PROT_READ` | PRESENT \| USER_ACCESSIBLE | x86 has no read-only without NX |
| `PROT_WRITE` | + WRITABLE | |
| `PROT_EXEC` | clear NO_EXECUTE | |
| `PROT_NONE` | clear PRESENT | |

Apply these flags in `alloc_and_map_user_pages` instead of the current
hardcoded `USER_DATA_FLAGS`.

### Changes

| File | Change |
|---|---|
| `libkernel/src/process.rs` | Add `Vma` struct, replace `mmap_regions` with `BTreeMap<u64, Vma>` |
| `osl/src/dispatch.rs` (`sys_mmap`) | Parse `prot`, compute PTF, store VMA |
| `osl/src/clone.rs` | Clone the VMA map instead of `Vec<(u64, u64)>` |
| `osl/src/exec.rs` | Clear VMA map on execve |

### Test

Allocate an mmap region with `PROT_READ` only, attempt a write from
userspace — should page-fault.

---

## Phase 2: Frame Free List + munmap ✓ (implemented)

**Goal:** Actually free physical frames when `munmap` is called.

### Frame allocator changes

The current frame allocator (`BootInfoFrameAllocator` wrapping an iterator of
usable frames) is allocate-only.  Two options:

1. **Bitmap allocator** — replace the iterator with a bitmap over all usable
   RAM.  Deallocation sets a bit.  Simple, O(1) free, but O(n) alloc in the
   worst case.
2. **Free-list overlay** — keep the bitmap for the initial boot-time pool,
   but maintain a singly-linked free list of returned frames (write the next
   pointer into the first 8 bytes of the freed page via the physical memory
   map).  O(1) alloc and free.

**Decision:** free-list overlay.  The bitmap is needed anyway to know which
frames are in use, but a free list on top gives O(1) alloc from returned
frames.

### Unmap primitive

Add `unmap_user_page(pml4_phys, vaddr) -> Option<PhysAddr>` to the memory
subsystem.  This walks the page table, clears the PTE, invokes `invlpg`, and
returns the physical frame address so the caller can free it.

### sys_munmap implementation

```
fn sys_munmap(addr: u64, length: u64) -> i64
```

1. Page-align addr and length.
2. Look up overlapping VMAs.
3. For each page in the range: call `unmap_user_page`, push the returned
   frame onto the free list.
4. Split/remove VMAs as needed (a munmap in the middle of a VMA creates two
   smaller VMAs).
5. TLB flush (per-page `invlpg` is fine for now; batch flush can come later).

### Changes

| File | Change |
|---|---|
| `libkernel/src/memory/` | Add `unmap_user_page`, frame free list |
| `osl/src/dispatch.rs` | Implement `sys_munmap` |
| `libkernel/src/process.rs` | VMA split/remove helpers |

### Contiguous DMA allocations

`alloc_dma_pages(pages)` with `pages > 1` bypasses the free list and uses
`allocate_frame_sequential` to guarantee physical contiguity.  The sequential
allocator walks the boot-time memory map and can be exhausted — once `next`
exceeds the total usable frames, it returns `None` even if the free list has
recycled frames available.

In practice this is fine because multi-page contiguous allocations only happen
during early boot (VirtIO descriptor rings).  If this becomes a problem in the
future, options include:

- Fall back to the free list for single-frame DMA when sequential is exhausted.
- Replace the sequential allocator with a buddy allocator that can satisfy
  contiguous requests from recycled frames.

### Test

`mmap` a region, write a pattern, `munmap` it, `mmap` a new region — should
get the same (or nearby) frames back, zero-filled.

---

## Phase 3: mprotect + Process Cleanup

**Goal:** Change page permissions on existing mappings, and free all process
memory on exit/execve.

### sys_mprotect

```
fn sys_mprotect(addr: u64, length: u64, prot: u64) -> i64
```

1. Validate addr is page-aligned.
2. Walk VMAs in the range, update `vma.prot`.
3. For each page: rewrite the PTE flags to match the new prot (reuse the
   PROT→PTF translation from Phase 1).
4. `invlpg` each modified page.
5. May need to split VMAs if the prot change covers only part of a VMA.

### Process cleanup on exit

When a process exits (`sys_exit` / `sys_exit_group`), before marking zombie:

1. Iterate all VMAs.
2. For each page in each VMA: unmap and free the frame (reuse Phase 2
   primitives).
3. Free the user page tables themselves (PML4, PDPT, PD, PT pages).
4. Free the brk region (iterate from `brk_base` to `brk_current`).
5. Free the user stack pages.

### Process cleanup on execve

`sys_execve` already creates a fresh PML4.  After the new PML4 is set up,
free the old page tables and all frames from the old VMA map (same cleanup
logic as exit, but targeting the old PML4).

### Changes

| File | Change |
|---|---|
| `osl/src/dispatch.rs` | Implement `sys_mprotect`, call cleanup in `sys_exit` |
| `osl/src/exec.rs` | Call cleanup for old address space before jump |
| `libkernel/src/memory/` | PTE flag update helper, page table walker for cleanup |
| `libkernel/src/process.rs` | VMA split for partial mprotect |

### Test

`mmap` RW, write data, `mprotect` to read-only, attempt write — should
fault.  Run a long-lived process that repeatedly spawns children — memory
usage should stay bounded.

---

## Phase 4: MAP_FIXED + Gap Finding

**Goal:** Support `MAP_FIXED` placement and smarter allocation that avoids
fragmenting the address space.

### MAP_FIXED

Currently rejected with `-ENOSYS`.  Implementation:

1. If `MAP_FIXED` is set and addr != 0: unmap any existing pages in
   `[addr, addr+length)` (implicit munmap, same as Linux).
2. Map new pages at the requested address.
3. Insert/replace VMAs.

### Gap-finding allocator

Replace the simple bump-down pointer (`mmap_next`) with a gap search over
the VMA tree:

1. Walk the VMA `BTreeMap` to find the highest gap that fits the requested
   size (top-down, matching Linux's default).
2. Fall back to searching lower if the top is full.
3. Remove `mmap_next` from `Process` — the VMA map is the source of truth.

This avoids the current problem where `munmap` frees space but `mmap_next`
never reclaims it.

### Changes

| File | Change |
|---|---|
| `osl/src/dispatch.rs` (`sys_mmap`) | MAP_FIXED support, gap-finding allocation |
| `libkernel/src/process.rs` | Remove `mmap_next`, add gap-search helper on VMA map |

### Test

`mmap` three regions, `munmap` the middle one, `mmap` with a size that fits
the gap — should reuse the freed space.  `MAP_FIXED` at a specific address
overlapping an existing mapping — old mapping should be silently replaced.

---

## Phase 5: File-Backed mmap + MAP_SHARED

**Goal:** Map files into the address space and support shared mappings with
reference-counted frames.

### 6th syscall argument

The Linux `mmap` signature is:

```c
void *mmap(void *addr, size_t length, int prot, int flags, int fd, off_t offset);
```

`fd` and `offset` are the 5th and 6th arguments.  The current assembly stub
saves all 6 user args (r9 is saved to `per_cpu.user_r9` at offset 32) but
only passes 5 to `syscall_dispatch` via the SysV64 calling convention (which
has exactly 6 register args, one consumed by `nr`).

Two options to deliver the 6th user arg:

1. **Read from PerCpuData** — `sys_mmap` reads `per_cpu.user_r9` directly
   via inline assembly or a helper.  No ABI change needed.
2. **Stack arg** — push the 6th arg onto the stack before `call
   syscall_dispatch`.  Requires changing both the assembly stub and the
   dispatch signature.

**Decision:** read from PerCpuData.  It's already saved there for clone, and
only mmap needs it.  Avoids touching the hot-path assembly for all syscalls.

### File-backed MAP_PRIVATE

1. `sys_mmap` validates `fd` refers to a regular file (via the fd table).
2. Read `length` bytes from the file at `offset` into freshly mapped pages.
3. Pages are CoW-private: the process owns them and modifications are not
   written back.  (True CoW with page-fault handling is deferred — initially
   just copy the data eagerly.)
4. Store `fd` and `offset` in the VMA for bookkeeping.

### MAP_SHARED + refcounted frames

Shared mappings require multiple processes to map the same physical frame.
This needs:

1. **Frame refcount table** — a global array (or hash map) from
   `PhysAddr → u16` tracking how many PTEs reference each frame.
   `mmap(MAP_SHARED)` increments, `munmap`/exit decrements, frame is freed
   when refcount reaches 0.
2. **Shared file cache** — a global `BTreeMap<(inode, page_offset) → PhysAddr>`
   so that two processes mapping the same file page get the same frame.
   (Requires the VFS to expose inode identifiers, which it does not today.)
3. **Dirty tracking** — `msync` or process exit writes dirty shared pages
   back to the file.  Deferred initially — start with read-only shared
   mappings.

### Eager vs demand paging

**Decision:** eager paging for all phases.  Demand paging (mapping pages
as not-present and faulting them in) requires a page-fault handler that can
resolve VMA lookups and perform I/O — significant complexity.  Eager paging
is correct, simpler, and adequate for the current workload.  Demand paging
can be added later as an optimisation without changing the VMA/syscall API.

### Changes

| File | Change |
|---|---|
| `libkernel/src/syscall.rs` | Helper to read `per_cpu.user_r9` |
| `osl/src/dispatch.rs` | Pass fd/offset to `sys_mmap`, file read path |
| `libkernel/src/memory/` | Frame refcount table |
| `libkernel/src/process.rs` | VMA `fd` / `offset` fields (already in Phase 1 struct) |
| VFS layer | Inode identifiers for shared page cache (if MAP_SHARED for files) |

### Test

Map a file, read its contents from the mapped region — should match.
Two processes `MAP_SHARED` the same file, one writes — the other should see
the change (once writable shared mappings are supported).

---

## Dependency Graph

```
Phase 1 ─── VMA tracking + PROT flags
   │
   ├──▶ Phase 2 ─── Frame free list + munmap
   │       │
   │       └──▶ Phase 3 ─── mprotect + process cleanup
   │               │
   │               └──▶ Phase 4 ─── MAP_FIXED + gap finding
   │
   └──────────────▶ Phase 5 ─── File-backed mmap + MAP_SHARED
                        │
                   (requires Phase 2 for munmap/refcount free,
                    Phase 3 for cleanup on exit)
```

Phase 1 is a prerequisite for everything — VMAs are the foundation.

Phase 2 (frame freeing) and Phase 3 (mprotect + cleanup) are sequential
because cleanup reuses the unmap/free primitives from Phase 2.

Phase 4 (MAP_FIXED + gap finding) requires munmap (Phase 2) for the implicit
unmap-on-overlap behaviour.

Phase 5 (file-backed) requires Phase 1 (VMA with fd/offset), Phase 2 (frame
freeing for refcount release), and Phase 3 (cleanup on exit to decrement
refcounts).

---

## Key Decisions

### Eager vs demand paging

All phases use **eager paging** — frames are allocated and mapped immediately
in `sys_mmap`.  Demand paging (lazy fault-in) is a future optimisation that
does not affect the syscall interface.

### 6th syscall argument for mmap

The `offset` parameter (6th arg, user r9) will be read from `PerCpuData`
rather than changing the dispatch function signature.  This avoids adding
overhead to every syscall for a parameter only mmap uses.

### Frame allocator: free-list overlay

Freed frames go onto a singly-linked free list stored in the pages themselves
(using the physical memory map for access).  The existing boot-time allocator
remains for initial allocation; the free list is consulted first.

### VMA storage: BTreeMap

A `BTreeMap<u64, Vma>` keyed by start address provides O(log n) lookup,
ordered iteration for gap-finding, and natural support for range queries.
Adequate for the expected number of VMAs per process (tens to low hundreds).
