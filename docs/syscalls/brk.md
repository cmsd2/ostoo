# brk (nr 12)

## Linux Signature

```c
int brk(void *addr);
```

Note: The raw syscall returns the new program break on success (not 0 like the glibc wrapper).

## Description

Sets the end of the process's data segment (the "program break"). Increasing the break allocates memory; decreasing it deallocates.

## Current Implementation

- **`brk(0)`** or **`brk(addr < brk_base)`**: Returns the current program break without modification. This is how musl queries the initial break.
- **`brk(addr <= brk_current)`**: Shrinks the break. Updates `brk_current` but does **not** unmap or free any pages.
- **`brk(addr > brk_current)`**: Grows the break. The requested address is page-aligned up. For each new page:
  - A physical frame is allocated via `alloc_dma_pages(1)`.
  - The frame is zeroed.
  - The frame is mapped into the process's page table with `PRESENT | WRITABLE | USER_ACCESSIBLE | NO_EXECUTE`.
  - `brk_current` is updated to the new page-aligned address.
- On allocation failure, returns the old `brk_current` (Linux convention: failure = unchanged break).

**Initial state:** `brk_base` and `brk_current` are set to the page-aligned end of the highest `PT_LOAD` ELF segment when the process is spawned.

**Lock ordering:** Process table lock is acquired/released to read state, then memory lock for allocation, then process table lock again to write the update.

**Source:** `osl/src/syscalls/mem.rs` — `sys_brk`

## Future Work

- Free physical frames and unmap pages when the break is decreased.
- Guard against growing the break into other mapped regions (stack, mmap area).
