# Paging Design

## Virtual Address Layout

x86-64 canonical addresses split into two halves:

```
0x0000_0000_0000_0000 ┐
       ...            │  lower canonical half — user process address space
0x0000_7FFF_FFFF_FFFF ┘
                        (non-canonical gap — any access faults)
0xFFFF_8000_0000_0000   kernel heap        (HEAP_START, 256 KiB)
0xFFFF_8001_0000_0000   Local APIC MMIO   (APIC_BASE, 4 KiB)
0xFFFF_8001_0001_0000   IO APIC(s)        (4 KiB × n, relative to APIC_BASE)
0xFFFF_8002_0000_0000   MMIO window       (MMIO_VIRT_BASE, 512 GiB)
  ↑ PCIe ECAM, virtio BARs, future driver MMIO allocated here
0xFFFF_FF80_0000_0000   recursive PT window (for index 511, see below)
0xFFFF_FFFF_FFFF_F000   PML4 self-mapping   (recursive index 511)
phys_mem_offset         bootloader physical identity map (stays put)
  + all physical RAM
```

All three kernel allocation regions (heap, APIC, MMIO) share **PML4 index 256**
(`0xFFFF_8000_*` through `0xFFFF_80FF_*`), keeping the kernel footprint in a
single top-level page-table entry — easy to share across per-process page tables
without marking it `USER_ACCESSIBLE`.

---

## Page Table Implementation: RecursivePageTable

### Why recursive instead of OffsetPageTable

`OffsetPageTable` walks page-table frames by computing
`phys_mem_offset + frame_phys_address`.  This creates a permanent dependency on
the bootloader's physical-identity map (which lives in the lower canonical half).
For user-space isolation we want the lower half to be entirely process-owned.

`RecursivePageTable` eliminates this dependency: the CPU's own hardware page
walker is used to reach PT frames, so no identity map is needed for page-table
operations.

### How recursive mapping works

One PML4 slot (index 511) is pointed at the PML4's own physical frame.  When the
CPU walks this entry it re-enters the same PML4 as if it were a PDPT.  Repeating
four times (P4→511, P3→511, P2→511, P1→511) exposes the PML4's own 4 KiB page
at virtual address `0xFFFF_FFFF_FFFF_F000`.

The full recursive window for index R (R=511) maps every page-table frame at a
computable virtual address:

| Depth | Virtual base (R=511) | What is mapped there |
|-------|---------------------|---------------------|
| PML4  | `0xFFFF_FFFF_FFFF_F000` | the PML4 itself |
| PDs   | `0xFFFF_FFFF_FFE0_0000`+ | all 512 PDPTs |
| PTs   | `0xFFFF_FFFF_C000_0000`+ | all 512 × 512 PDs |
| Pages | `0xFFFF_FF80_0000_0000`+ | all PT frames |

The x86_64 crate's `RecursivePageTable` type uses these computable addresses to
implement `Mapper` and `Translate` without any identity-map knowledge.

### Setup sequence (in `libkernel::memory::init`)

```
1. Read CR3 → PML4 physical frame
2. Access PML4 via bootloader identity map: virt = phys_mem_offset + pml4_phys
3. Write PML4[511] = (pml4_phys_frame, PRESENT | WRITABLE)
4. flush_all()   ← new mapping is now active
5. Compute recursive PML4 address: 0xFFFF_FFFF_FFFF_F000
6. Obtain &'static mut PageTable at that address
7. RecursivePageTable::new(pml4_at_recursive_addr)
```

After step 7 the identity map is still live (bootloader mapping is never
removed), but `RecursivePageTable` does not use it for page-table walks.

---

## MMIO Virtual Address Allocator

### Problem with the old approach

The old `map_mmio_region` mapped MMIO at `phys_mem_offset + phys_addr` — the
same virtual address the identity map uses for regular RAM.  This worked but:

- It placed MMIO in the lower canonical half (future user space).
- It gave MMIO a fixed virtual address tied to `phys_mem_offset`, which varies
  per boot and can change if the bootloader is swapped.

### New design: bump allocator + cache

A bump pointer starts at `MMIO_VIRT_BASE = 0xFFFF_8002_0000_0000` and advances
one region at a time.  A `BTreeMap<phys_base, virt_base>` cache ensures that
mapping the same physical address twice returns the same virtual address.

```
MMIO_VIRT_BASE  0xFFFF_8002_0000_0000
  + PCIe ECAM    1 MiB
  + virtio BAR0  varies
  + ...
  (grows upward; 512 GiB window — exhaustion is practically impossible)
```

Flags: `PRESENT | WRITABLE | NO_CACHE` (same as before).

### Cache key

The cache key is the **page-aligned** physical base address.  If the same
physical base is mapped twice with different sizes the second call returns the
cached mapping (the first mapping covers at least as many pages as were
originally requested; in practice PCI BAR sizes are fixed per device).

### Heap dependency

`BTreeMap::insert` allocates from the kernel heap.  `map_mmio_region` must not
be called before `init_heap` completes or from interrupt context.  All current
call sites (boot path in `main.rs`, `KernelHal::mmio_phys_to_virt`) satisfy
this constraint.

---

## Remaining Identity Map Dependency

After the switch to `RecursivePageTable`, the bootloader identity map is still
used in exactly two places:

| Use | Location | Notes |
|-----|----------|-------|
| DMA address translation | `KernelHal::dma_alloc` in `devices/src/virtio/mod.rs` | DMA frames are physical RAM; `phys_mem_offset + paddr` gives the kernel virtual address for CPU access |
| ACPI table access | `KernelAcpiHandler::phys_to_virt` in `kernel/src/kernel_acpi.rs` | ACPI tables are in physical RAM; same formula |

Both are kernel-private and never exposed to user space.  The bootloader identity
map entries do **not** have the `USER_ACCESSIBLE` flag, so they are invisible to
ring-3 processes regardless.

The restriction: every page table the kernel uses to walk page structures must
keep the bootloader's PML4 entries for the identity-map region.  For per-process
page tables this is easily satisfied by copying PML4 entries 0–255 (lower half)
from the kernel PML4 — without `USER_ACCESSIBLE` — at process creation time.

---

## Per-Process Page Tables

Each process gets its own PML4, created by `MemoryServices::create_user_page_table`:

- **Slot 511**: self-referential entry pointing to the process's own PML4 physical
  frame (required for `RecursivePageTable` to work per-process).
- **Slots 256–510**: shared kernel mappings (heap, APIC, MMIO window, physical
  memory direct map), copied verbatim from the active PML4.  These are high-half
  addresses, never accessible from ring-3.  Because the PML4 entries point to the
  same PDPT/PD/PT frames, changes to kernel page tables at levels below PML4 are
  automatically visible in all address spaces.
- **Slots 0–255**: process-private user-space mappings.  The process's code,
  stack, heap, and memory-mapped files live here.

Switching between processes requires only a `mov cr3, new_pml4_phys` — the
kernel's high-half mappings are identical in every page table so no TLB flush is
needed for kernel entries (on CPUs with PCID support).

### PML4 lifecycle

User PML4s and their lower-half page table frames are freed when a process
exits (`terminate_process`) or replaces its address space (`execve`).  The
kernel boot PML4 physical address is stored in `KERNEL_PML4_PHYS` (set during
`init_services`).  Before freeing a user PML4, the dying/exec'ing code
switches CR3 and the scheduler's thread record to the kernel PML4.  This is
critical because the frame allocator uses an intrusive free-list that
overwrites freed frames immediately — leaving CR3 pointing at a freed PML4
would cause a triple fault on the next TLB refill.

---

## Files Changed

| File | Change |
|------|--------|
| `libkernel/src/allocator/mod.rs` | `HEAP_START = 0xFFFF_8000_0000_0000` |
| `kernel/src/main.rs` | `APIC_BASE = 0xFFFF_8001_0000_0000` |
| `libkernel/src/memory/mod.rs` | `RecursivePageTable`; `MMIO_VIRT_BASE` bump allocator; `mmio_cache: BTreeMap` |
| `libkernel/src/memory/vmem_allocator.rs` | Test `BASE` constant updated (cosmetic) |
