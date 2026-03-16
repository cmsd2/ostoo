# Project Status

`ostoo` is a hobby x86-64 kernel written in Rust, following the
[Writing an OS in Rust](https://os.phil-opp.com/) blog series by Philipp Oppermann.
All twelve tutorial chapters have been completed and the project has gone
significantly beyond the tutorial.

## Workspace Layout

| Crate | Purpose |
|---|---|
| `kernel/` | Top-level kernel binary — entry point, ties everything together |
| `libkernel/` | Core kernel library — all subsystems live here |
| `apic/` | Local APIC and I/O APIC initialisation and LAPIC timer |
| `devices/` | Driver framework — `DriverTask` trait, actor macro, built-in drivers, VFS |
| `devices-macros/` | Proc-macro crate: `#[actor]`, `#[on_message]`, `#[on_info]`, `#[on_tick]`, `#[on_stream]`, `#[on_start]` |

Target triple: `x86_64-os` (custom JSON target, bare-metal, no std).
Build tooling: `cargo-xbuild` + `bootimage` (BIOS bootloader).
Toolchain: current nightly (floating, `rust-toolchain.toml`).

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
- Hardware cursor (CRTC registers 0x3D4/0x3D5) kept in sync on every write.
- `redraw_line(start_col, buf, len, cursor)` for in-place line editing.
- Fixed status bar at row 0 (`status_bar!` macro, white-on-blue); updated by
  `status_task` every 250 ms with thread index, context-switch count, task
  queue depths, and uptime.
- Timeline strip at row 1: scrolling coloured blocks, one per context switch,
  colour-coded by thread index.

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
- 8259 PIC (chained) initialised via `pic8259`; remapped to IRQ vectors 32–47.
- PIC is later disabled once the APIC is configured.
- Timer interrupt handler (IRQ 0): increments tick counter, wakes timer futures.
- Keyboard interrupt handler (IRQ 1): reads scancode from port 0x60,
  pushes it into the async scancode queue.

### 8–9. Paging / Paging Implementation
- `libkernel/src/memory/mod.rs` — `RecursivePageTable` (PML4 slot 511
  self-referential); MMIO bump allocator at `0xFFFF_8002_0000_0000` with
  `BTreeMap` cache for idempotency; physical memory identity map kept for
  DMA address translation only (`phys_mem_offset` from bootloader).
- `libkernel/src/memory/frame_allocator.rs` — `BootInfoFrameAllocator`
  walks the bootloader memory map to hand out usable physical frames.
- `libkernel/src/memory/vmem_allocator.rs` — `DumbVmemAllocator` hands
  out a sequential range of virtual addresses (no reclamation); currently
  unused in production — the MMIO bump allocator in `MemoryServices` handles
  all virtual address allocation at runtime.

### 10. Heap Allocation
- Kernel heap mapped at `0xFFFF_8000_0000_0000`, size 256 KiB
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

---

## Beyond the Tutorial

### Timer
- `libkernel/src/task/timer.rs` — LAPIC tick counter; `TICKS_PER_SECOND = 1000`.
- `Delay` future: resolves after a given number of ticks.
- `Mailbox::recv_timeout(ticks)` races inbox against a `Delay`.

### Preemptive Multi-threaded Scheduler
- `libkernel/src/task/scheduler.rs` — round-robin preemptive scheduler driven
  by the LAPIC timer at 1000 Hz; 10 ms quantum (`QUANTUM_TICKS = 10`).
- Assembly stub `lapic_timer_stub` saves all 15 GPRs + iret frame on the
  current stack, then calls `preempt_tick(current_rsp) -> new_rsp` in Rust.
- `preempt_tick` advances the tick counter, acknowledges the LAPIC interrupt,
  decrements the quantum, and when it expires saves the old RSP, selects the
  next ready thread, and returns its `saved_rsp`.
- `scheduler::migrate_to_heap_stack(run_kernel)` allocates a 64 KiB heap stack
  and switches thread 0 off the bootloader's lower-half stack onto PML4
  entry 256 (high canonical half), so it survives CR3 switches into user page
  tables.
- `scheduler::init()` registers the boot context as thread 0.
- `scheduler::spawn_thread(entry)` allocates a 64 KiB stack, synthesises an
  iret frame, and enqueues the new thread.
- The kernel boots two executor threads (threads 0 and 1) that share the same
  async task queue; tasks are transparently dispatched across both.
- Shell command `threads` shows the current thread index and total context
  switches since boot.

### Actor System (`devices/`, `devices-macros/`)
- `DriverTask` trait: `name()`, `run(inbox, handle)`.
- `Mailbox<M>` / `Inbox<M>` MPSC queue; `ActorMsg<M,I>` envelope wraps
  inner messages, info queries, and erased-type info queries.
- Process registry (`libkernel/src/task/registry.rs`): actors register by name;
  `registry::get::<M,I>(name)` returns a typed sender handle.
- `ErasedInfo` registry: actors register a `Box<dyn Fn() -> ...>` so the shell
  can query any actor's info without knowing its concrete type.

#### Proc-macro attributes (used inside `#[actor]` blocks)
| Attribute | Effect |
|---|---|
| `#[on_start]` | Called once before the run loop |
| `#[on_message(Variant)]` | Handles one inner message enum variant |
| `#[on_info]` | Returns the actor's typed info struct |
| `#[on_tick]` | Called periodically; actor provides `tick_interval_ticks()` |
| `#[on_stream(factory)]` | Polls a `Stream + Unpin` in the unified event loop |

The macro generates a unified `poll_fn` loop when `#[on_tick]` or `#[on_stream]`
are present, racing all event sources in a single future.

### Shell (`kernel/src/shell.rs`)
- `#[actor]`-based shell actor with persistent CWD state (`spin::Mutex<String>`).
- Prompt includes CWD: `ostoo:/path> `.
- `resolve_path` / `normalize_path` handle relative paths and `..` components.
- Commands: `help`, `echo`, `driver <start|stop|info>`, `blk <info|read|ls|cat>`,
  `ls`, `cat`, `pwd`, `cd`, `mount`.

### Keyboard Actor (`kernel/src/keyboard_actor.rs`)
- `#[actor]` + `#[on_stream(key_stream)]`; registered as `"keyboard"`.
- Full readline-style line editing:
  - Cursor movement: ← → / Ctrl+B/F, Home/End / Ctrl+A/E
  - Editing: Backspace, Delete, Ctrl+K (kill to end), Ctrl+U (kill to start),
    Ctrl+W (delete word)
  - History: ↑↓ / Ctrl+P/N, 50-entry `VecDeque`, live-buffer save/restore
  - Ctrl+C clears the line; Ctrl+L clears the screen
- Dispatches complete lines to the shell via `ShellMsg::KeyLine`.

### virtio-blk Block Device (`devices/src/virtio/`)
- `virtio-drivers` 0.7 crate provides the virtio protocol; the kernel supplies
  `KernelHal` implementing `Hal` for DMA allocation, MMIO mapping, and
  virtual→physical address translation.
- QEMU Q35 machine; PCIe ECAM at physical `0xB000_0000` mapped at boot via
  `MemoryServices::map_mmio_region`.
- `VirtioBlkActor` actor: handles `Read` and `Write` messages using the
  non-blocking virtio-drivers API (`read_blocks_nb` / `complete_read_blocks`)
  with a busy-poll `CompletionFuture` for MVP.
- `KernelHal::share` performs a full page-table walk (`translate_virt`) so that
  heap-allocated `BlkReq`/`BlkResp`/data buffers produce correct physical
  addresses for the device.
- Shell commands: `blk info`, `blk read <sector>`.
- See [`docs/virtio-blk.md`](virtio-blk.md) for full details.

### exFAT Filesystem (`devices/src/virtio/exfat.rs`)
- Read-only exFAT driver with no external dependencies.
- Auto-detects bare exFAT, MBR-partitioned, and GPT-partitioned disk images.
- Implements: boot sector parsing, FAT chain traversal, directory entry set
  parsing (File / Stream Extension / File Name entries), and recursive path
  walking with case-insensitive ASCII matching.
- File reads capped at 16 KiB; peak heap usage during `ls` ≈ 5 KiB.
- See [`docs/exfat.md`](exfat.md) for full details.

### VFS Layer (`devices/src/vfs/`)
- Uniform path namespace over multiple filesystems; shell no longer calls
  filesystem drivers directly.
- Enum dispatch (`AnyVfs`) avoids `Pin<Box<dyn Future>>` trait objects.
- Mount table (`MOUNTS`: `spin::Mutex<Vec<(String, Arc<AnyVfs>)>>`) sorted
  longest-mountpoint-first; the `Arc` is cloned out before any `.await` so
  the lock is never held across a suspension point.
- `ExfatVfs` — wraps a `BlkInbox` and delegates to the exFAT driver.
- `ProcVfs` — synthetic filesystem; no block I/O:
  - `/proc/tasks` — ready / waiting task counts from the executor.
  - `/proc/uptime` — seconds since boot from the LAPIC tick counter.
  - `/proc/drivers` — name and state of every registered driver.
- Shell commands: `ls`, `cat`, `cd` use the VFS API; `mount` manages the
  mount table at runtime (`mount`, `mount proc <mp>`, `mount blk <mp>`).
- `/proc` is always mounted at boot; exFAT `/` is mounted if virtio-blk is present.
- See [`docs/vfs.md`](vfs.md) for full design notes.

### Dummy Driver (`devices/src/dummy.rs`)
- Example actor with `#[on_tick]` heartbeat, `#[on_message(SetInterval)]`,
  and `#[on_info]`.
- Demonstrates the full actor feature set.

### ACPI Parsing
- `kernel/src/kernel_acpi.rs` implements an `AcpiHandler` that accesses
  physical ACPI regions via the bootloader's identity map
  (`phys + physical_memory_offset`); no dynamic page mapping is required
  since all ACPI tables live in physical RAM.
- Calls `acpi::search_for_rsdp_bios` to locate and parse ACPI tables.
- On boot the interrupt model is printed; APIC vs legacy PIC is detected.

### APIC Crate (`apic/`)
- A separate crate for APIC initialisation, mapped at `0xFFFF_8001_0000_0000`.
- `apic/src/local_apic/` — Local APIC register access via MMIO and MSR.
- `apic/src/io_apic/` — I/O APIC register access via MMIO.
- `apic::init()` maps the Local APIC and all I/O APICs from the ACPI table,
  routes ISA IRQs 0 (timer) and 1 (keyboard) through the I/O APIC to IDT
  vectors 0x20 and 0x21, then disables the 8259 PIC.
- `apic::calibrate_and_start_lapic_timer()` uses the PIT as a reference to
  measure the LAPIC bus frequency, starts the LAPIC timer in periodic mode
  at 1000 Hz, then masks the PIT's I/O APIC entry so it no longer fires.

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

### Heap Size
The heap is a fixed 256 KiB at `0xFFFF_8000_0000_0000`. This accommodates two
64 KiB thread stacks (thread 0 and thread 1) plus driver and task allocations,
but will need to grow for any real subsystem work.  The `DumbVmemAllocator` has
no reclamation path, so virtual address space for MMIO/ACPI mappings is also
consumed monotonically.

### virtio-blk Busy Polling
`CompletionFuture` re-schedules itself immediately rather than sleeping on an
IRQ waker. This burns CPU on every block read. The IRQ handler sets
`IRQ_PENDING` but the executor does not yet have an `AtomicWaker` integration
to park the future until the device signals completion.

### Single-sector DMA Buffers
Block I/O is done one 512-byte sector at a time. For large directory scans or
file reads this results in many round-trips through the virtio queue.

### exFAT Write Support
The exFAT driver is read-only. All filesystem state changes (create, write,
delete) are unsupported.

### ProcVfs File Sizes Reported as Zero
`VfsDirEntry::size` is 0 for all `/proc` entries because the content length
is not known until the data is serialised. This is cosmetically wrong in `ls`
output but functionally harmless.

---

## Possible Next Steps

1. **IRQ-driven virtio-blk** — wire `IRQ_PENDING` to an `AtomicWaker` so
   `CompletionFuture` parks instead of busy-polling.

2. **Larger / growable heap** — demand-paged heap that grows on fault, or at
   minimum a larger static allocation.

3. **exFAT write support** — directory entry creation, FAT chain allocation,
   and sector writes to enable `touch`, `mkdir`, `cp`, `rm`.

4. **More ProcVfs entries** — `/proc/meminfo`, `/proc/cpuinfo`, `/proc/pci`,
   etc., surfacing existing shell command output as readable files.

5. **New filesystem drivers** — a RAM-based tmpfs (`TmpVfs`) or a simple
   flat-file initrd would exercise the VFS extension path without requiring
   block I/O.

6. **Multi-sector DMA** — batch multiple sectors per virtio request to reduce
   queue round-trips for directory scans and file reads.

7. **User space / process isolation** — ring-3 processes with their own page
   tables, system call interface, and per-process file descriptor tables built
   on top of the VFS.

8. **Reclaiming virtual address space** — replace `DumbVmemAllocator` with a
   proper free-list allocator so MMIO mappings can be released.
