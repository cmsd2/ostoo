# Project Status

`ostoo` is a hobby x86-64 kernel written in Rust, following the
[Writing an OS in Rust](https://os.phil-opp.com/) blog series by Philipp Oppermann.
All twelve tutorial chapters have been completed and the project has started going
beyond the tutorial into APIC/ACPI territory.

## Workspace Layout

| Crate | Purpose |
|---|---|
| `kernel/` | Top-level kernel binary — entry point, ties everything together |
| `libkernel/` | Core kernel library — all subsystems live here |
| `apic/` | Advanced Programmable Interrupt Controller support (in progress) |

Target triple: `x86_64-os` (custom JSON target, bare-metal, no std).
Build tooling: `cargo-xbuild` + `bootimage` (BIOS bootloader).

---

## Completed Tutorial Chapters

### 1–2. Freestanding Binary / Minimal Kernel
- `#![no_std]`, `#![no_main]`, custom panic handler.
- `bootloader` crate provides the BIOS boot stage and passes a `BootInfo` struct.
- Entry point via `entry_point!` macro (`libkernel_main` in `kernel/src/main.rs`).

### 3. VGA Text Mode
- `libkernel/src/vga_buffer.rs` — a `Writer` behind a `spin::Mutex`.
- `print!` / `println!` macros available globally.
- Volatile writes to avoid compiler optimisation of MMIO.

### 4. Testing
- Custom test framework (`custom_test_frameworks` feature).
- Integration tests in `kernel/tests/`: `basic_boot`, `heap_allocation`,
  `should_panic`, `stack_overflow`.
- QEMU `isa-debug-exit` device used to signal pass/fail to the host.
- Serial port (`libkernel/src/serial.rs`) used for test output.

### 5–6. CPU Exceptions / Double Faults
- IDT set up in `libkernel/src/interrupts.rs` via `lazy_static`.
- Handlers: breakpoint, page fault (panics), double fault (panics).
- Double fault uses a dedicated IST stack (GDT TSS entry).
- GDT + TSS initialised in `libkernel/src/gdt.rs`.

### 7. Hardware Interrupts
- 8259 PIC (chained) initialised via the `pic8259_simple` crate.
- PIC remapped to IRQ vectors 32–47 to avoid conflict with CPU exceptions.
- Timer interrupt handler (IRQ 0): sends EOI, no other action yet.
- Keyboard interrupt handler (IRQ 1): reads scancode from port 0x60,
  pushes it into the async scancode queue (see Async/Await below).

### 8–9. Paging / Paging Implementation
- `libkernel/src/memory/mod.rs` — `OffsetPageTable` wrapping the active
  level-4 page table; physical memory fully mapped at a fixed offset
  supplied by the bootloader (`map_physical_memory` feature).
- `libkernel/src/memory/frame_allocator.rs` — `BootInfoFrameAllocator`
  walks the bootloader memory map to hand out usable physical frames.
- `libkernel/src/memory/vmem_allocator.rs` — `DumbVmemAllocator` hands
  out a sequential range of virtual addresses (no reclamation); used by
  the ACPI mapper.

### 10. Heap Allocation
- Kernel heap mapped at `0x4444_4444_0000`, size 100 KiB
  (`libkernel/src/allocator/mod.rs`).
- Global allocator: `linked_list_allocator::LockedHeap`.
- `extern crate alloc` available; `Box`, `Vec`, `Rc`, `BTreeMap`, etc. all work.

### 11. Allocator Designs
- Bump allocator implemented in `libkernel/src/allocator/bump.rs`
  (O(1) alloc, no free).
- `linked_list_allocator` is the active global allocator (can be swapped
  by changing the `static ALLOCATOR` line in `libkernel/src/lib.rs`).

### 12. Async/Await
- Task abstraction in `libkernel/src/task/mod.rs` — pinned boxed futures
  with atomic `TaskId`.
- Simple round-robin executor in `task/simple_executor.rs`.
- Full waker-based executor in `task/executor.rs`:
  - Ready tasks in a `VecDeque`, waiting tasks in a `BTreeMap`.
  - Wake queue (`crossbeam_queue::ArrayQueue`) for interrupt-safe wakeups.
  - `sleep_if_idle` uses `sti; hlt` to avoid busy-waiting.
- Async keyboard task in `task/keyboard.rs`:
  - `OnceCell<ArrayQueue<u8>>` scancode queue filled from the IRQ handler.
  - `ScancodeStream` implements `Future`/`Stream`.
  - `print_keypresses()` async fn decodes scancodes via `pc-keyboard` and
    prints characters to the VGA buffer.
- Main executor loop runs `example_task` and `print_keypresses` on boot.

---

## Beyond the Tutorial

### ACPI Parsing
- `kernel/src/kernel_acpi.rs` implements an `AcpiHandler` that maps
  physical ACPI regions into virtual memory via the `DumbVmemAllocator`.
- ACPI virtual address space: `0x6666_6666_0000`, up to 200 pages.
- Calls `acpi::search_for_rsdp_bios` to locate and parse ACPI tables.
- On boot the interrupt model is printed; Apic vs legacy PIC is detected.

### APIC Crate (`apic/`)
- A separate crate for APIC initialisation, mapped at `0x5555_5555_0000`.
- `apic/src/local_apic/` — Local APIC register access via MMIO and MSR.
- `apic/src/io_apic/` — I/O APIC register access via MMIO.
- `apic::init()` maps both the local APIC and all I/O APICs from the ACPI
  table into virtual memory, then calls `init()` on each.
- **Status: written but not yet wired in** — the call in `main.rs:88` is
  commented out. The 8259 PIC is still active.

### Logging
- `libkernel/src/logger.rs` wraps the VGA `println!` macro as a `log::Log`
  implementation.
- `log::{debug, info, warn, error}` macros usable throughout the kernel.
- Initialised early in `libkernel_main`.

### CPUID
- `libkernel/src/cpuid.rs` — thin wrapper around `raw-cpuid`; `init()`
  called during kernel init.

---

## Known Issues / Technical Debt

### Stale Toolchain
The pinned toolchain (`rust-toolchain`) is `nightly-2020-04-10` — approximately
six years old. Several language features used were unstable at the time and have
since been stabilised or changed:

| Feature | Current status |
|---|---|
| `wake_trait` | Stabilised; no longer needs a feature gate |
| `alloc_error_handler` | Stabilised in recent nightly |
| `abi_x86_interrupt` | Still unstable but the attribute form changed |

### Stale Dependencies
All dependencies are pinned to 2019–2020 versions. Key crates with significant
breaking changes since:

| Crate | Pinned | Current | Notes |
|---|---|---|---|
| `bootloader` | 0.8.2 | ~0.11 | Completely new API in 0.9+ |
| `x86_64` | 0.9.6 (libkernel) / 0.7.5 (apic) | ~0.15 | `UnusedPhysFrame` removed in 0.12; Mapper API changed |
| `acpi` | 0.4.0 | ~5.x | Completely rewritten API |
| `pic8259_simple` | git (personal fork) | (superseded by `pic8259`) | |
| `linked_list_allocator` | 0.8.2 | ~0.10 | |
| `crossbeam-queue` | 0.2.1 | ~0.3 | |

### `UnusedPhysFrame` Usage
`libkernel/src/memory/mod.rs:61` and `kernel/src/kernel_acpi.rs:37` use
`UnusedPhysFrame::new(frame)` which was removed from `x86_64` after 0.11.
Any dependency update must address this.

### APIC Not Wired In
The `apic::init()` call is commented out in `kernel/src/main.rs:88`. Before
enabling it:
- The 8259 PIC should be masked/disabled after APIC is initialised.
- IRQ routing via the I/O APIC redirection table needs to be configured.
- The IDT needs entries for APIC interrupt vectors (≥ 32 are already used by
  the PIC mapping; APIC typically uses vectors 0x20–0xFF differently).

### Two Versions of x86_64
`libkernel` uses `x86_64 = "0.9.6"` and `apic` uses `x86_64 = "0.7.5"`. Cargo
will resolve these separately but it is a source of confusion. They should be
unified.

### Heap Size
The heap is 100 KiB. This is sufficient for the tutorial workload but would need
to grow for any real subsystem work.

---

## Possible Next Steps

1. **Finish APIC initialisation** — uncomment `apic::init`, configure I/O APIC
   redirection table entries for keyboard (IRQ 1) and timer (IRQ 0), then disable
   the 8259 PIC.

2. **Update the toolchain and dependencies** — this is a significant but
   worthwhile undertaking. The `bootloader` 0.9+ series and the current `x86_64`
   crate are much cleaner. The tutorial second-edition branch may be worth
   consulting as a reference.

3. **PIT / timer abstraction** — the timer IRQ currently does nothing. A simple
   tick counter would enable sleep/timeout primitives for the async executor.

4. **Process / scheduling** — the async executor is a cooperative multitasking
   primitive. A preemptive scheduler on top of the timer interrupt would be the
   natural next step.

5. **Filesystem / block device** — QEMU's `virtio-blk` or an ATA PIO driver
   would enable persistent storage.
