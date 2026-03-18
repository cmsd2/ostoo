# Unsafe Code Audit & Refactoring Opportunities

An audit of `unsafe` usage across the codebase, prioritised by density and
refactoring payoff.

---

## 1. `libkernel/src/vga_buffer.rs` — Raw pointer to MMIO buffer ✅ DONE

~~`Writer` stores a raw `*mut Buffer` pointer and dereferences it with
`unsafe { &mut *self.buffer }` in **7 separate places**.  There is also a
manual `unsafe impl Send` to paper over the raw pointer.~~

**Completed** (commit `75de8c4`):

- Introduced a `VgaBuffer` safe wrapper that encapsulates the raw pointer
  with `unsafe` confined to construction only.  Safe `read_cell` /
  `write_cell` / `set_hw_cursor` methods replaced all interior `unsafe`
  blocks in `Writer` methods and free functions.
- `unsafe impl Send` moved from `Writer` to `VgaBuffer` with documented
  invariant.
- `set_hw_cursor` is now a safe method on `VgaBuffer` (was a standalone
  `unsafe fn`).
- `core::mem::transmute` in tests replaced with a new `Color::from_u8()`
  constructor.
- `timeline_append` refactored: ISR now pushes to a lock-free `ArrayQueue`
  instead of writing directly to VGA RAM with raw pointers.  A new
  `TimelineActor` (stream-driven, using `#[on_stream]`) drains the queue
  and writes to VGA row 1 through the safe `WRITER` / `VgaBuffer` interface.
  Eliminates the last `unsafe` block and removes the `VGA_BASE` atomic.

---

## 2. `libkernel/src/task/scheduler.rs` — Raw stack frame construction & inline asm ✅ DONE

~~`spawn_thread` and `spawn_user_thread` both manually write 20 `u64` values
to raw stack pointers to construct fake iretq frames.  `preempt_tick` reads
raw pointers at computed offsets for sanity checks.  `process_trampoline`
contains a large `unsafe` asm block.~~

**Completed** (commit `ac60740`):

- Introduced `#[repr(C)] SwitchFrame` with named fields matching the
  `lapic_timer_stub` push/pop order.  Constructors `new_kernel()` and
  `new_user_trampoline()` replace magic-number `frame.add(N).write(...)` in
  both `spawn_thread` and `spawn_user_thread`.
- `preempt_tick` sanity check reads `frame.rip` / `frame.rsp` through the
  typed struct instead of raw pointer arithmetic.
- Extracted `drop_to_ring3()` unsafe helper from `process_trampoline`:
  GS MSR writes + CR3 switch + iretq in one well-documented `unsafe fn`,
  making the safety boundary explicit.

---

## 3. `libkernel/src/syscall.rs` — `static mut` per-CPU data ✅ DONE

~~`PER_CPU` and `SYSCALL_STACK` are `static mut`, accessed with bare
`unsafe` throughout.  `sys_write` creates a slice from a raw user-space
pointer without any validation.~~

**Completed** (commit `1c28010`):

- Replaced `static mut PER_CPU` with an `UnsafeCell` wrapper (`PerCpuCell`)
  with documented safety invariant (single CPU, interrupts disabled).
- Replaced `static mut SYSCALL_STACK` with a safe `#[repr(align(16))]`
  static.  `kernel_stack_top()` is now fully safe.
- `sys_write` now validates that the user buffer falls entirely within user
  address space (< `0x0000_8000_0000_0000`), returning `EFAULT` for invalid
  pointers.
- `init()`, `set_kernel_rsp()`, `per_cpu_addr()` updated to use new
  accessors — no more `&raw const` on `static mut`.

---

## 4. `apic/src/local_apic/mapped.rs` — Every method is `unsafe` ✅ DONE

~~`MappedLocalApic` has **15 public `unsafe` methods**.  The unsafety stems
from MMIO access via raw pointers in `read_reg_32` / `write_reg_32`, but
the actual invariant is in *construction* (providing a valid base address),
not in each register read/write.~~

**Completed** (commit `24a421d`):

- `MappedLocalApic::new()` is now the sole `unsafe` boundary with documented
  safety invariants.
- All 15 public methods are now safe; `read_reg_32` / `write_reg_32` trait
  impl uses `core::ptr::read_volatile` / `write_volatile`.
- Callers in `apic/src/lib.rs` and `devices/src/vfs/proc_vfs.rs` updated —
  dozens of `unsafe` blocks removed.

---

## 5. `apic/src/io_apic/mapped.rs` — Same pattern as local APIC ✅ DONE

~~Same issue — every public method is `unsafe`, and register access helpers
use raw pointer dereferences without `read_volatile` / `write_volatile`.~~

**Completed** (commit `24a421d`):

- `MappedIoApic::new()` is now the sole `unsafe` boundary with documented
  safety invariants.  `base_addr` field made private with `base_addr()` getter.
- All public methods (`mask_all`, `mask_entry`, `set_irq`,
  `max_redirect_entries`, `read_version_raw`, `read_redirect_entry`) are now
  safe.  Internal calls to the `IoApic` trait methods remain `unsafe` blocks.
- `IoApic` trait impl (`read_reg_32` / `write_reg_32` / `read_reg_64` /
  `write_reg_64`) now uses `core::ptr::read_volatile` / `write_volatile`
  instead of raw dereferences — correct for MMIO.
- Callers in `apic/src/lib.rs` and `devices/src/vfs/proc_vfs.rs` updated.

---

## 6. `kernel/src/kernel_acpi.rs` — Repetitive raw pointer reads/writes

The `acpi::Handler` impl has 8 nearly identical `read_uN` / `write_uN`
methods, each doing `unsafe { *(addr as *const T) }`.  No volatile access,
no alignment checks.

**Recommendations:**

- Create a generic `fn mmio_read<T>(addr: usize) -> T` /
  `fn mmio_write<T>(addr: usize, val: T)` helper using
  `read_volatile` / `write_volatile`, then call it from each trait method.
  Reduces 16 lines of unsafe to 2.
- Same for the IO port methods — a single `port_read::<T>(port)` /
  `port_write::<T>(port, val)` generic would collapse 6 methods.

---

## 7. `kernel/src/ring3.rs` — Scattered raw pointer copies

`spawn_blob` and `spawn_process` manually call `core::ptr::write_bytes`
and `core::ptr::copy_nonoverlapping` on physical-memory-mapped addresses.
The pattern `phys_off + phys_addr → as_mut_ptr → write_bytes` repeats
multiple times.

**Recommendations:**

- Add `zero_frame(phys: PhysAddr)` and
  `copy_to_frame(phys: PhysAddr, data: &[u8])` utilities on
  `MemoryServices` that encapsulate the offset arithmetic and unsafe ptr
  operations.  This would also clean up similar patterns in
  `libkernel/src/memory/mod.rs`.

---

## 8. `libkernel/src/gdt.rs` — Mutable cast of `static` TSS

`set_kernel_stack` casts `&*TSS` through `*const → *mut` to write `rsp0`.
This is technically UB (mutating through a shared reference to a
`lazy_static`).

**Recommendations:**

- Store the TSS in an `UnsafeCell` or `Mutex` so the mutation is sound.
  Since it is single-CPU and only called with interrupts off, an
  `UnsafeCell` wrapper with a documented invariant is sufficient.

---

## 9. `libkernel/src/interrupts.rs` — Crash-dump raw pointer reads

`double_fault_handler` and `invalid_opcode_handler` use
`core::ptr::read_volatile` on raw addresses for crash diagnostics, and the
inline-asm MSR reads are duplicated across fault handlers.

**Recommendations:**

- Extract a `fn dump_cpu_state(frame: &InterruptStackFrame) -> CpuState`
  helper that reads CR2/CR3/CR4/GS MSRs once and returns a struct,
  eliminating duplicated inline asm across fault handlers.
- A `fn dump_bytes_at(addr: u64, len: usize) -> [u8; 16]` helper would
  replace the raw pointer reads in both handlers.

---

## 10. `devices/src/vfs/proc_vfs.rs` — Manual page-table walking

`gen_pmap()` manually walks PML4 / PDPT / PD / PT levels using raw pointer
casts like `unsafe { &*((phys_off + addr) as *const PageTable) }`.

**Recommendations:**

- Add a `walk_page_tables` iterator or visitor on `MemoryServices` that
  safely provides `(virt_range, phys_base, flags)` entries.  Replaces 50+
  lines of raw pointer walks.

---

## Summary table

| Priority   | File                      | Unsafe count | Refactor                                       |
|------------|---------------------------|--------------|-------------------------------------------------|
| **High**   | `scheduler.rs`            | ~~12~~       | ✅ Done — `SwitchFrame` struct, `drop_to_ring3`  |
| **High**   | `syscall.rs`              | ~~8~~        | ✅ Done — `UnsafeCell`, user pointer validation  |
| **High**   | `local_apic/mapped.rs`    | ~~18~~       | ✅ Done — safe methods, unsafe-only construction |
| **High**   | `io_apic/mapped.rs`       | ~~12~~       | ✅ Done — same + `read_volatile` / `write_volatile` |
| **Medium** | `vga_buffer.rs`           | ~~14~~       | ✅ Done — `VgaBuffer` wrapper                   |
| **Medium** | `kernel_acpi.rs`          | ~16          | Generic volatile MMIO helpers                    |
| **Medium** | `ring3.rs`                | ~8           | `zero_frame` / `copy_to_frame` on MemoryServices |
| **Medium** | `gdt.rs`                  | 2            | `UnsafeCell` for TSS mutation                    |
| **Low**    | `interrupts.rs`           | ~10          | `dump_cpu_state` + `dump_bytes_at` helpers       |
| **Low**    | `proc_vfs.rs`             | ~5           | Page-table walk iterator                         |
