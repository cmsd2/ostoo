# Project Status

`ostoo` is a hobby x86-64 kernel written in Rust, following the
[Writing an OS in Rust](https://os.phil-opp.com/) blog series by Philipp Oppermann.
All twelve tutorial chapters have been completed and the project has gone
significantly beyond the tutorial.

## Workspace Layout

| Crate | Purpose |
|---|---|
| `kernel/` | Top-level kernel binary — entry point, ties everything together |
| `libkernel/` | Core kernel library — all subsystems including APIC live here |
| `osl/` | "OS Subsystem for Linux" — syscall dispatch + VFS bridge |
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
- `libkernel/src/vga_buffer/mod.rs` — a `Writer` behind an `IrqMutex`.
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
- Kernel heap mapped at `0xFFFF_8000_0000_0000`, size 512 KiB
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

### User Space and Process Isolation
- Full ring-3 process support with per-process page tables, SYSCALL/SYSRET,
  and preemptive scheduling.  Process exit and `execve` properly free user-half
  page tables and data frames (with refcount-aware shared frame handling).
- 35+ Linux-compatible syscalls in `osl/src/syscalls/`.
- Per-process FD table, CWD tracking, parent/child relationships, zombie
  lifecycle with `wait4`/`reap`.
- ELF loader for static x86-64 binaries; initial stack with `argc/argv/auxv`.
- IPC channels with fd-passing (capability transfer) — syscalls 505–507.
  See [`docs/ipc-channels.md`](ipc-channels.md).
- Shared memory via `shmem_create` (syscall 508) + `mmap(MAP_SHARED)` —
  anonymous shared memory backed by reference-counted physical frames.
  See [`docs/mmap-design.md`](mmap-design.md) Phase 5b.
- Notification fds via `notify_create` (509) + `notify` (510) — general-
  purpose inter-process signaling through completion ports (`OP_RING_WAIT`).
  See [`docs/completion-port-design.md`](completion-port-design.md) Phase 4.
- Console input buffer with foreground PID routing and blocking `read(0)`.
- Async-to-sync bridge (`osl/src/blocking.rs`) for VFS calls from syscall
  context.
- See [`docs/userspace-plan.md`](userspace-plan.md) for the full roadmap
  (Phases 0–6 complete; Phase 7 signals not yet started).

### Userspace Libraries (`user/include/ostoo.h`, `user-rs/rt/`)
- **C library** (`libostoo.a`): shared header `user/include/ostoo.h` with struct
  definitions, syscall numbers, opcodes, and flags.  Static library
  `user/lib/libostoo.a` provides typed syscall wrappers for all 12 custom
  syscalls (501–512), output helpers (`puts_stdout`, `put_num`, `put_hex`),
  conversion helpers (`itoa_buf`, `simple_atoi`), and ring buffer access
  helpers (`sq_entry`, `cq_entry`).  All 21 demo programs have been migrated
  to use the shared library, eliminating per-file boilerplate.
- **Rust library** (`ostoo-rt` crate): two modules added to the existing
  `user-rs/rt/` runtime crate.  `sys` module provides raw syscall wrappers
  and `repr(C)` struct definitions matching the kernel ABI.  `ostoo` module
  provides safe RAII types (`CompletionPort`, `IpcSend`/`IpcRecv`,
  `SharedMem`, `NotifyFd`, `IrqFd`, `IoRing`) with automatic fd cleanup on
  drop, plus builder methods on `IoSubmission` for each opcode.

### Userspace Shell (`user/src/shell.c`)
- Primary user interface: musl-linked C binary, auto-launched on boot from
  `/bin/shell` via `kernel/src/main.rs`.
- Line editing: read char-by-char, echo, backspace, Ctrl+C (cancel), Ctrl+D
  (exit on empty line).
- Built-in commands: `echo`, `pwd`, `cd`, `ls`, `cat`, `pid`, `export`, `env`,
  `unset`, `exit`, `help`.
- Environment variables: shell maintains an env table, passes it to children.
  Kernel provides defaults: `PATH=/host/bin`, `HOME=/`, `TERM=dumb`,
  `SHELL=/bin/shell`.
- External programs: `posix_spawn(path)` + `waitpid`.
- Built with Docker-based musl cross-compiler (`scripts/user-build.sh`).
- Sources in `user/src/`, binaries output to `user/bin/`.
- See [`docs/userspace-shell.md`](userspace-shell.md) for full design.

### Kernel Shell (`kernel/src/shell.rs`) — fallback
- `#[actor]`-based shell actor, active when no userspace shell is running.
- Prompt includes CWD: `ostoo:/path> `.
- Commands: `help`, `echo`, `driver <start|stop|info>`, `blk <info|read|ls|cat>`,
  `ls`, `cat`, `pwd`, `cd`, `mount`, `exec`, `test`.
- Info commands (cpuinfo, meminfo, etc.) migrated to `/proc`; accessible via
  `cat /proc/<file>`.

### Keyboard Actor (`kernel/src/keyboard_actor.rs`)
- `#[actor]` + `#[on_stream(key_stream)]`; registered as `"keyboard"`.
- Foreground routing: when a user process is foreground, raw keypresses are
  delivered to `console::push_input()` for userspace `read(0)`.
- When kernel is foreground: full readline-style line editing:
  - Cursor movement: ← → / Ctrl+B/F, Home/End / Ctrl+A/E
  - Editing: Backspace, Delete, Ctrl+K (kill to end), Ctrl+U (kill to start),
    Ctrl+W (delete word)
  - History: ↑↓ / Ctrl+P/N, 50-entry `VecDeque`, live-buffer save/restore
  - Ctrl+C clears the line; Ctrl+L clears the screen
- Dispatches complete lines to the kernel shell via `ShellMsg::KeyLine`.

### virtio-blk Block Device (`devices/src/virtio/`)
- `virtio-drivers` 0.13 crate provides the virtio protocol; the kernel supplies
  `KernelHal` implementing `Hal` for DMA allocation, MMIO mapping, and
  virtual→physical address translation.
- QEMU Q35 machine; PCIe ECAM at physical `0xB000_0000` mapped at boot via
  `MemoryServices::map_mmio_region`.  `PciRoot` is generic over `MmioCam<'static>`.
- `VirtioBlkActor` actor: handles `Read` and `Write` messages using the
  non-blocking virtio-drivers API (`read_blocks_nb` / `complete_read_blocks`)
  with a busy-poll `CompletionFuture` for MVP.
- `KernelHal::share` performs a full page-table walk (`translate_virt`) so that
  heap-allocated `BlkReq`/`BlkResp`/data buffers produce correct physical
  addresses for the device.
- Shell commands: `blk info`, `blk read <sector>`.
- See [`docs/virtio-blk.md`](virtio-blk.md) for full details.

### VirtIO 9P Host Directory Sharing (`devices/src/virtio/p9*.rs`)
- VirtIO 9P (9P2000.L) driver for sharing a host directory into the guest,
  providing a Docker-volume-like workflow: edit files on the host, they appear
  instantly in the guest.
- `p9_proto.rs` — minimal 9P2000.L wire protocol: 8 message pairs (version,
  attach, walk, lopen, read, readdir, getattr, clunk).
- `p9.rs` — `P9Client` high-level client wrapping `VirtIO9p<KernelHal, PciTransport>`.
  Synchronous API behind `SpinMutex`; performs version handshake + attach on
  construction.  Public methods: `list_dir`, `read_file`, `stat`.
- QEMU shares `./user` directory via `-fsdev local,...,security_model=none`
  + `-device virtio-9p-pci,...,mount_tag=hostfs`.
- Mounted at `/host` (always) and at `/` as fallback when no virtio-blk disk is
  present, so `/bin/shell` auto-launch works without a disk image.
- PCI device IDs: `0x1AF4:0x1049` (modern), `0x1AF4:0x1009` (legacy).
- Read-only for MVP; no write/create/delete support.
- See [`docs/virtio-9p.md`](virtio-9p.md) for full details.

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
- Mount table (`MOUNTS`: `SpinMutex<Vec<(String, Arc<AnyVfs>)>>`) sorted
  longest-mountpoint-first; the `Arc` is cloned out before any `.await` so
  the lock is never held across a suspension point.
- `ExfatVfs` — wraps a `BlkInbox` and delegates to the exFAT driver.
- `Plan9Vfs` — wraps an `Arc<P9Client>` and delegates to the 9P client.
  Maps `P9Error` to `VfsError` (ENOENT→NotFound, ENOTDIR→NotADirectory, etc.).
- `ProcVfs` — synthetic filesystem; no block I/O.  All system info commands
  have been migrated from the shell to `/proc` virtual files:
  - `/proc/tasks` — ready / waiting task counts from the executor.
  - `/proc/uptime` — seconds since boot from the LAPIC tick counter.
  - `/proc/drivers` — name and state of every registered driver.
  - `/proc/threads` — current thread index and context-switch count.
  - `/proc/meminfo` — heap usage, frame allocator stats, known virtual regions.
  - `/proc/memmap` — physical memory regions from the bootloader memory map.
  - `/proc/cpuinfo` — CPU vendor, family/model/stepping, CR0/CR4/EFER/RFLAGS.
  - `/proc/pmap` — page table walk with coalesced contiguous regions.
  - `/proc/idt` — IDT vector assignments (exceptions, PIC, LAPIC, dynamic).
  - `/proc/pci` — enumerated PCI devices.
  - `/proc/lapic` — Local APIC state and timer configuration.
  - `/proc/ioapic` — I/O APIC redirection table entries.
  - `/proc/irq_stats` — per-slot IRQ counters (total, delivered, buffered, spurious).
- Shell commands: `ls`, `cat`, `cd` use the VFS API; `mount` manages the
  mount table at runtime (`mount`, `mount proc <mp>`, `mount blk <mp>`).
- `/proc` is always mounted at boot; exFAT `/` is mounted if virtio-blk is
  present; 9p `/host` is mounted if virtio-9p is present (and 9p falls back
  to `/` when no disk image exists).
- See [`docs/vfs.md`](vfs.md) for full design notes.

### Completion Port Async I/O (`osl/src/io_port.rs`)
- io_uring-style completion-based async I/O subsystem.
- Kernel object: `CompletionPort` (`libkernel/src/completion_port.rs`) — bounded
  queue of completions with single-waiter blocking semantics.
- `FdObject` enum in `libkernel/src/file.rs` provides type-safe polymorphism
  for the fd table (`File` | `Port`), replacing the previous trait-object
  downcast approach.
- `IrqMutex` protects the `CompletionPort` for ISR-safe `post()` from
  interrupt context.
- Syscalls: `io_create` (501), `io_submit` (502), `io_wait` (503),
  `io_setup_rings` (511), `io_ring_enter` (512).
- Supported operations: `OP_NOP` (immediate), `OP_TIMEOUT` (async timer via
  executor), `OP_READ` / `OP_WRITE` (async — user buffers are copied to/from
  kernel memory during `io_submit`/`io_wait`; the actual I/O runs on executor
  tasks so `io_submit` returns immediately), `OP_IRQ_WAIT` (hardware interrupt
  delivery — ISR masks GSI and posts completion; rearm via another submit unmasks).
- Shared-memory SQ/CQ rings (Phase 5): `io_setup_rings` allocates ring pages
  as shmem fds; userspace writes SQEs to the SQ ring and reads CQEs from the
  CQ ring.  `io_ring_enter` kicks the kernel and/or blocks for completions.
- `FileHandle` trait has `poll_read` / `poll_write` methods (default impls
  delegate to sync `read`/`write`).  `PipeReader` and `ConsoleHandle`
  override `poll_read` with waker-based async semantics so completion port
  reads never block executor threads.
- Userspace demo programs: `io_demo.c` (smoke test), `io_pingpong.c` /
  `io_pong.c` (parent-child IPC via completion port).
- See [`docs/completion-port-design.md`](completion-port-design.md) for the
  full phased roadmap (all phases complete).

### IRQ File Descriptors (`libkernel/src/irq_handle.rs`, `osl/src/irq.rs`)
- Userspace interrupt delivery via `irq_create(gsi)` syscall (504).
- `IrqInner` tracks GSI, vector, slot, and saved IO APIC redirection entry.
- ISR handler (`irq_fd_dispatch`) masks the GSI via `libkernel::apic::mask_gsi`
  and posts a completion to the associated `CompletionPort`. For keyboard
  (GSI 1) and mouse (GSI 12), the ISR reads port 0x60 and drains all
  available bytes per interrupt (up to 16 per ISR invocation).
- 64-entry scancode ring buffer per slot prevents lost scancodes between
  rearms. `arm_irq` bulk-drains the entire buffer into completions.
- Per-slot atomic IRQ counters (total, delivered, buffered, spurious,
  wrong_source) visible via `/proc/irq_stats`.
- On close, the original IO APIC entry is restored.
- Demo: `user/irq_demo.c` — keyboard scancode display via OP_IRQ_WAIT.

### IPC Channels (`libkernel/src/channel.rs`, `osl/src/ipc.rs`)
- Capability-based IPC channels for structured message passing between processes.
- Unidirectional with configurable buffer capacity: capacity=0 for synchronous
  rendezvous (seL4-style), capacity>0 for async buffered.
- Fixed 48-byte messages: `tag` (u64) + `data[3]` (u64) + `fds[4]` (i32).
- **fd-passing** (capability transfer): sender's fds are extracted at send time,
  kernel objects are stored in the channel, and new fds are allocated in the
  receiver's fd table at recv time. Cleanup on drop for undelivered messages.
- Completion port integration: `OP_IPC_SEND` (5) and `OP_IPC_RECV` (6) for
  multiplexing IPC with timers, IRQs, and file I/O.
- Syscalls: `ipc_create` (505), `ipc_send` (506), `ipc_recv` (507).
- Demos: `ipc_sync.c`, `ipc_async.c`, `ipc_port.c`, `ipc_fdpass.c`.
- See [`docs/ipc-channels.md`](ipc-channels.md) for full design.

### Deadlock Detection (`libkernel/src/spin_mutex.rs`)
- All `spin::Mutex` locks replaced with `SpinMutex` — a drop-in wrapper that
  counts spin iterations and panics after a threshold, turning silent hangs
  into actionable diagnostics with serial output.
- `SpinMutex`: 100M iteration limit (~100 ms) — allows for legitimate
  preemption contention on a single-core scheduler.
- `IrqMutex`: 10M iteration limit (~10 ms) — interrupts disabled means no
  preemption, so any contention indicates a true deadlock.
- `deadlock_panic()` writes directly to COM1 (0x3F8) bypassing `SERIAL1`'s
  lock, then panics.

### POSIX Signals (`libkernel/src/signal.rs`, `osl/src/signal.rs`)
- Phases 1–2: signal infrastructure, delivery on SYSCALL return, Ctrl+C/SIGINT.
- `rt_sigaction` (13): install/query signal handlers (SA_SIGINFO, SA_RESTORER).
- `rt_sigprocmask` (14): SIG_BLOCK/UNBLOCK/SETMASK for the signal mask.
- `kill` (62): send a signal to a specific pid.
- `rt_sigreturn` (15): restore context from rt_sigframe after handler returns.
- Signal delivery via `check_pending_signals` in the SYSCALL return path:
  constructs a Linux-ABI-compatible `rt_sigframe` on the user stack, rewrites
  the saved register frame so `sysretq` "returns" into the handler.
- Ctrl+C: keyboard actor queues SIGINT on `foreground_pid()`, wakes blocked
  console reader.
- Default actions: SIG_DFL terminate (SIGKILL, SIGTERM, etc.) or ignore (SIGCHLD).
- Demos: `user/sig_demo.c` (SIGUSR1 self-signal), userspace shell handles
  SIGINT via `sigaction`.
- See [`docs/signals.md`](signals.md) for full design.

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

### APIC Module (`libkernel/src/apic/`)
- APIC code lives in `libkernel::apic`, mapped at `0xFFFF_8001_0000_0000`.
- `libkernel/src/apic/local_apic/` — Local APIC register access via MMIO and MSR.
- `libkernel/src/apic/io_apic/` — I/O APIC register access via MMIO.
- `libkernel::apic::init()` maps the Local APIC and all I/O APICs from the ACPI table,
  routes ISA IRQs 0 (timer) and 1 (keyboard) through the I/O APIC to IDT
  vectors 0x20 and 0x21, then disables the 8259 PIC.
- `libkernel::apic::calibrate_and_start_lapic_timer()` uses the PIT as a reference to
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
The heap is a fixed 1 MiB at `0xFFFF_8000_0000_0000`. This accommodates two
64 KiB thread stacks (threads 0 and 1), per-process 64 KiB kernel stacks, plus
driver and task allocations. Zombie processes are reaped via `wait4` + `reap()`,
but concurrent processes will still pressure the heap. The
`DumbVmemAllocator` has no reclamation path, so virtual address space for
MMIO/ACPI mappings is also consumed monotonically.

### virtio-blk Single-sector I/O
Block I/O uses IRQ-driven completion via `AtomicWaker`, but is still limited
to one 512-byte sector per request.

### exFAT Write Support
The exFAT driver is read-only. All filesystem state changes (create, write,
delete) are unsupported.

### ProcVfs File Sizes Reported as Zero
`VfsDirEntry::size` is 0 for all `/proc` entries because the content length
is not known until the data is serialised. This is cosmetically wrong in `ls`
output but functionally harmless.

---

## Possible Next Steps

### Completion Port — All Phases Complete

- Phases 1–4 (core, read/write, OP_IRQ_WAIT, OP_RING_WAIT) — see sections above.
- **Phase 5: Shared-memory SQ/CQ rings** — implemented.  `io_setup_rings` (511)
  allocates shared SQ/CQ ring pages exposed as shmem fds.  `io_ring_enter` (512)
  processes SQ entries and waits for CQ completions.  Dual-mode `post()` writes
  simple CQEs directly to the shared CQ ring; deferred completions (OP_READ,
  OP_IPC_RECV) are flushed in syscall context.  Test: `ring_sq_test`.

### Memory Management

1. **Larger / growable heap** — demand-paged heap that grows on fault, or a
   larger static allocation.  1 MiB is tight with concurrent processes.

2. **Reclaiming virtual address space** — replace `DumbVmemAllocator` with a
   proper free-list allocator so MMIO mappings can be released.

3. **File-backed `MAP_SHARED`** — anonymous shared memory (via
   `shmem_create`) is complete; file-backed `MAP_SHARED` with inode page
   cache remains future work.  See [`docs/mmap-design.md`](mmap-design.md)
   Phase 5c.

### Process Model

4. **Signals Phase 3+** — Phases 1–2 (basic signal delivery + Ctrl+C/SIGINT)
   are complete: `rt_sigaction`, `rt_sigprocmask`, `kill`, signal delivery on
   SYSCALL return, `rt_sigreturn`, Ctrl+C → SIGINT to foreground process.
   Remaining: exception-generated signals (SIGSEGV, SIGILL), SIGCHLD on child
   exit.  See [`docs/signals.md`](signals.md).

5. **`fork` + CoW page faults** — standard POSIX `fork`.  `clone(CLONE_VM|CLONE_VFORK)`
   and `execve` are now implemented, enabling unpatched musl `posix_spawn` and
   Rust `std::process::Command`.  Full `fork` with CoW still requires a page
   fault handler and frame reference counting.

### Drivers & I/O

6. **Multi-sector DMA** — batch multiple sectors per virtio request to reduce
   queue round-trips for directory scans and file reads.

7. **exFAT write support** — directory entry creation, FAT chain allocation,
   and sector writes to enable `touch`, `mkdir`, `cp`, `rm`.

### Compositor & Window Management

The userspace compositor (`/bin/compositor`) is a Wayland-style display server
with full input routing and window management.

- **Display**: Takes exclusive ownership of the BGA framebuffer via
  `framebuffer_open` (515). Double-buffered compositing with painter's
  algorithm. Cursor-only rendering optimization patches small rectangles
  for mouse movement instead of full recomposite.
- **Input**: Connects to `/bin/kbd` (keyboard) and `/bin/mouse` (mouse)
  services via the service registry (`svc_register` 513, `svc_lookup` 514).
  Key events forwarded to focused window. Mouse events drive cursor, focus,
  drag, and resize.
- **CDE-style decorations**: Server-side window decorations inspired by
  CDE/Motif — 3D beveled borders (BORDER_W=4, BEVEL=2), 24px title bar
  with centered title, CDE-style close button, sunken inner bevel around
  client area. Blue-grey color palette.
- **Window management**: Click-to-focus with Z-order raise. Title bar drag
  to move. Edge/corner drag to resize with context-sensitive cursor icons
  (diagonal, horizontal, vertical double-arrows). Close button removes
  window.
- **Resize protocol**: On resize completion, compositor allocates a new
  shared buffer and sends `MSG_WINDOW_RESIZED` (tag 7) with the new
  buffer fd. Terminal emulator remaps buffer, recalculates dimensions,
  and redraws.
- **Terminal emulator** (`/bin/term`): Compositor client that spawns
  `/bin/shell` with pipe-connected stdin/stdout. VT100 parser with
  color support. Handles window resize.
- See [`docs/compositor-design.md`](compositor-design.md) and
  [`docs/display-input-ownership.md`](display-input-ownership.md).

### Microkernel Path

8. **Microkernel Phase B** — kernel primitives for userspace drivers:
   device MMIO mapping, DMA syscalls.  IRQ fd (syscall 504 + OP_IRQ_WAIT)
   and `MAP_SHARED` (via `shmem_create` 508) are complete.  Remaining items
   unblock userspace NIC driver.
   See [`docs/microkernel-design.md`](microkernel-design.md).

9. **Networking** — virtio-net driver + smoltcp TCP/IP stack.  The
   completion port is ready to back it once the NIC driver lands.
   See [`docs/networking-design.md`](networking-design.md).
