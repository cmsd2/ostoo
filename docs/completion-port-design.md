# Completion Port Design

## Overview

This document describes a unified completion-based async I/O primitive
(CompletionPort) for ostoo.  The design supports both io_uring-style and
Windows IOCP-style patterns through a single kernel object, accessed as an
ordinary file descriptor.

The CompletionPort is motivated by the microkernel migration path
([microkernel-design.md](microkernel-design.md), Phases B-E) where userspace
drivers need to wait on multiple event sources — IRQs, shared-memory ring
wakeups, timers — through a single blocking wait point, without polling or
managing multiple threads.

See also: [mmap-design.md](mmap-design.md) for the shared memory primitives
that enable the zero-syscall ring optimisation (Phase 5 of this design).

---

## Motivation

### The Problem

A userspace NIC driver in the microkernel architecture must simultaneously
wait for:

1. **IRQ events** — the device raised an interrupt
2. **Ring wakeups** — the TCP/IP server posted new transmit descriptors
3. **Timers** — a retransmit or watchdog timer expired

With the current kernel, each of these is a separate blocking `read()` on a
separate fd.  A driver would need one thread per event source, or a `poll()`/
`select()` readiness multiplexer — neither of which exists yet.

### Why Completion-Based

A completion-based model inverts the usual readiness pattern:

- **Readiness** (epoll/poll/select): "tell me when fd X is ready, then I'll
  do the I/O myself."  Two syscalls per operation (wait + read/write).
- **Completion** (io_uring/IOCP): "do this I/O for me and tell me when it's
  done."  One syscall to submit, one to reap — or zero with shared-memory
  rings.

Completion-based I/O is a better fit for ostoo because:

- **Simpler driver loops.**  Submit work, wait for completions.  No edge-
  triggered vs level-triggered subtlety.
- **Naturally batched.**  Multiple operations submitted and reaped per
  syscall.
- **Unifies heterogeneous events.**  IRQs, timers, and file I/O all produce
  the same `IoCompletion` struct.
- **Shared-memory fast path.**  The submission/completion queues can be
  mapped into userspace for zero-syscall operation under load (Phase 5).
- **Matches the microkernel data plane.**  Drivers post work and reap
  completions — the same pattern as managing hardware descriptor rings.

---

## How Other Systems Do It

### Linux io_uring

Introduced in 5.1.  Submission Queue (SQ) and Completion Queue (CQ) are
shared-memory ring buffers mapped into userspace.  The kernel polls the SQ
for new entries; completions appear in the CQ.  `io_uring_enter()` is the
single syscall (submit + wait).

- Supports 60+ operation types (read, write, accept, timeout, etc.)
- SQEs carry a `user_data` field returned verbatim in CQEs for demux
- `IORING_SETUP_SQPOLL` mode: kernel thread polls the SQ — truly zero-
  syscall submission under load
- Fixed-file and fixed-buffer registration to avoid per-op fd/buffer lookup

### Windows IOCP (I/O Completion Ports)

The original completion-based API (NT 3.5, 1994).  A completion port is a
kernel object that aggregates completions from multiple file handles.

- `CreateIoCompletionPort()` creates the port and associates handles
- Async operations (ReadFile, WriteFile with OVERLAPPED) post completions
  to the associated port
- `GetQueuedCompletionStatus()` dequeues one completion (blocking)
- `PostQueuedCompletionStatus()` manually posts a completion (for app-level
  signaling)
- The kernel limits concurrent threads to the port's concurrency value

### Fuchsia zx_port

Zircon ports are the unified event aggregation primitive:

- `zx_port_create()` creates a port
- `zx_object_wait_async()` registers interest in an object's signals
  (channels, interrupts, timers, processes) — when the signal fires, a
  packet is queued to the port
- `zx_port_wait()` dequeues a packet (blocking with optional timeout)
- `zx_port_queue()` manually enqueues a user packet
- Packets carry a `key` field for demux (equivalent to `user_data`)

### seL4 Notifications

seL4 uses a minimal signaling primitive:

- A **notification** is a word-sized bitmask of binary semaphores
- `seL4_Signal()` OR-sets bits; `seL4_Wait()` atomically reads and clears
- Multiple event sources (IRQs, IPC completions) signal different bits in
  the same notification
- One `seL4_Wait()` multiplexes all sources — the returned word tells which
  bits fired
- Limitation: carries no payload beyond the bitmask.  Data transfer requires
  a separate shared-memory protocol

### Comparison

| Aspect | io_uring | IOCP | zx_port | seL4 notify | **ostoo (proposed)** |
|---|---|---|---|---|---|
| Model | Completion | Completion | Completion | Signal | Completion |
| Queue location | Shared memory | Kernel | Kernel | Kernel (1 word) | Kernel (Phase 5: shared mem) |
| Payload | Full SQE/CQE | Bytes + key | Packet union | Bitmask only | IoCompletion struct |
| Demux field | user_data | CompletionKey | key | Bit position | user_data |
| Event sources | Files, sockets, timers | File handles | Objects + signals | Capabilities | Fds, IRQs, timers, rings |
| Zero-syscall path | SQPOLL mode | No | No | No | Phase 5 (shared rings) |
| Submit + wait | Single syscall | Separate | Separate | Separate | Single syscall (io_wait) |

---

## Core Abstraction

A **CompletionPort** is a kernel object consisting of a FIFO completion queue
and a waiter slot.  It is accessed through a file descriptor, like any other
ostoo resource.

```
                   ┌─────────────────────────────┐
                   │       CompletionPort         │
                   │                              │
  io_submit ──────▶│  ┌─────────────────────────┐ │
                   │  │   Completion Queue       │ │
  IRQ ISR ────────▶│  │  ┌────┬────┬────┬───┐   │ │
                   │  │  │ C0 │ C1 │ C2 │...│   │ │──────▶ io_wait
  Timer expire ───▶│  │  └────┴────┴────┴───┘   │ │        (blocks until
                   │  └─────────────────────────┘ │         non-empty)
  Ring wakeup ────▶│                              │
                   │  waiter: Option<thread_idx>  │
                   └─────────────────────────────┘
```

Key properties:

- **Single consumer.**  Only one thread may call `io_wait` on a port at a
  time.  This avoids thundering-herd complexity and matches the single-
  threaded driver loop model.
- **Multiple producers.**  Any context — syscall path, ISR, timer callback —
  can post a completion to the queue.
- **User_data demux.**  Every submission carries a `u64 user_data` field that
  is returned verbatim in the completion, allowing the caller to identify
  which operation completed without inspecting the payload.
- **Port as fd.**  The port lives in the process's `fd_table` and can be
  closed, passed across `execve` (unless `FD_CLOEXEC`), or used with `dup2`.

---

## Syscall Interface

Three new syscalls using custom numbers (continuing from the existing `spawn`
at 500):

| Nr | Name | Signature |
|---|---|---|
| 501 | `io_create` | `io_create(flags: u32) → fd` |
| 502 | `io_submit` | `io_submit(port_fd: i32, entries: *const IoSubmission, count: u32) → i64` |
| 503 | `io_wait` | `io_wait(port_fd: i32, completions: *mut IoCompletion, max: u32, min: u32, timeout_ns: u64) → i64` |

### io_create (501)

Creates a new CompletionPort and returns its file descriptor.

- `flags`: reserved, must be 0.  Future: `IO_CLOEXEC`.
- Returns: fd on success, negative errno on failure.

### io_submit (502)

Submits one or more I/O operations to the port.

- `port_fd`: fd returned by `io_create`.
- `entries`: pointer to an array of `IoSubmission` structs in user memory.
- `count`: number of entries to submit (0 < count ≤ 64).
- Returns: number of entries successfully submitted, or negative errno.

Submissions that reference invalid fds or unsupported operations fail
individually — the return value indicates how many of the leading entries
were accepted.

### io_wait (503)

Waits for completions and copies them to user memory.

- `port_fd`: fd returned by `io_create`.
- `completions`: pointer to an array of `IoCompletion` structs in user memory.
- `max`: maximum number of completions to return.
- `min`: minimum number to wait for before returning (0 = non-blocking poll).
- `timeout_ns`: maximum wait time in nanoseconds.  0 = no timeout (wait
  indefinitely for `min` completions).  With `min=0`, returns immediately.
- Returns: number of completions written, or negative errno.

### IoSubmission struct

```rust
#[repr(C)]
pub struct IoSubmission {
    pub user_data: u64,   // returned in completion, opaque to kernel
    pub opcode: u32,      // OP_NOP, OP_READ, etc.
    pub flags: u32,       // per-op flags, reserved
    pub fd: i32,          // target fd (for OP_READ, OP_WRITE)
    pub _pad: i32,
    pub buf_addr: u64,    // user buffer pointer
    pub buf_len: u32,     // buffer length
    pub offset: u32,      // file offset (low 32 bits, sufficient initially)
    pub timeout_ns: u64,  // for OP_TIMEOUT
}
```

Total size: 48 bytes.

### IoCompletion struct

```rust
#[repr(C)]
pub struct IoCompletion {
    pub user_data: u64,   // copied from submission
    pub result: i64,      // bytes transferred, or negative errno
    pub flags: u32,       // completion flags (reserved)
    pub opcode: u32,      // echoed from submission
}
```

Total size: 24 bytes.

---

## Operations

| Opcode | Name | Description |
|---|---|---|
| 0 | `OP_NOP` | No operation.  Completes immediately.  Useful for testing. |
| 1 | `OP_TIMEOUT` | Completes after `timeout_ns` nanoseconds. |
| 2 | `OP_READ` | Read from `fd` into `buf_addr`. |
| 3 | `OP_WRITE` | Write to `fd` from `buf_addr`. |
| 4 | `OP_IRQ_WAIT` | Wait for interrupt on IRQ fd. |
| 5 | `OP_RING_WAIT` | Wait for shared-memory ring wakeup. |

### OP_NOP (0)

Immediately posts a completion with `result = 0`.  No side effects.  Used for
round-trip latency testing and as a wake-up mechanism (submit a NOP from
another thread to unblock `io_wait`).

### OP_TIMEOUT (1)

Registers a one-shot timer.  Completes after `timeout_ns` nanoseconds with
`result = 0`, or `result = -ETIME` if cancelled (future).

Implementation: uses the existing `libkernel::task::timer` (Delay/Sleep)
infrastructure.  The submission spawns an async delay task that posts the
completion when the timer fires.

### OP_READ (2)

Reads up to `buf_len` bytes from `fd` at `offset` into `buf_addr`.

- `result` = number of bytes read, or negative errno.
- For console/pipe fds (no meaningful offset), `offset` is ignored.

Implementation: see "Sync fallback worker" below.

### OP_WRITE (3)

Writes up to `buf_len` bytes from `buf_addr` to `fd` at `offset`.

- `result` = number of bytes written, or negative errno.

Implementation: same sync fallback pattern as OP_READ.

### OP_IRQ_WAIT (4)

Waits for a hardware interrupt on an IRQ fd (from
[microkernel-design.md](microkernel-design.md), Phase B).

- `fd` must be an IRQ fd (IrqHandle).
- `result` = interrupt count since last wait, or negative errno.

Implementation: the IRQ fd's ISR-safe notification calls `port.post()` when
the interrupt fires.  No worker thread needed — the ISR posts directly.

### OP_RING_WAIT (5)

Waits for a shared-memory ring buffer to transition from empty to non-empty.

- `fd` must be a shared-memory ring fd.
- `result` = 0 on wakeup, or negative errno.

Implementation: the ring's producer-side syscall (`io_ring_notify` or a
futex-like mechanism) calls `port.post()`.  No worker thread needed.

Requires shared memory support ([mmap-design.md](mmap-design.md), Phase 5).

---

## Kernel Implementation Sketch

### CompletionPort struct

```rust
use alloc::collections::VecDeque;

pub struct CompletionPort {
    queue: VecDeque<IoCompletion>,
    waiter: Option<usize>,       // thread index blocked in io_wait
    max_queued: usize,           // backpressure limit (default 256)
}
```

The `CompletionPort` is wrapped in a `Mutex` and stored inside a
`CompletionPortHandle` that implements `FileHandle`.

### CompletionPortHandle

```rust
pub struct CompletionPortHandle {
    port: Arc<Mutex<CompletionPort>>,
}
```

Implements `FileHandle`:

- `read()` → returns `Err(FileError::BadFd)` (use `io_wait` instead)
- `write()` → returns `Err(FileError::BadFd)` (use `io_submit` instead)
- `close()` → drop the Arc.  Pending operations are cancelled (completions
  with `-ECANCELED` are discarded).
- `kind()` → a new `FileKind::CompletionPort` variant

### ISR-safe post()

The `post()` method must be callable from interrupt context (e.g., an IRQ
handler posting OP_IRQ_WAIT completions).

```rust
impl CompletionPort {
    /// Post a completion.  Safe to call from ISR context.
    pub fn post(&mut self, completion: IoCompletion) {
        if self.queue.len() < self.max_queued {
            self.queue.push_back(completion);
        }
        // Wake the blocked waiter, if any
        if let Some(thread_idx) = self.waiter.take() {
            scheduler::unblock(thread_idx);
        }
    }
}
```

The `Mutex` around `CompletionPort` must disable interrupts while held (this
matches the existing `Mutex` in libkernel which wraps `spin::Mutex` with
interrupt disable).

### io_wait blocking pattern

```
sys_io_wait(port_fd, completions_ptr, max, min, timeout_ns):
    port = lookup_fd(port_fd) as CompletionPortHandle
    loop:
        lock port
        n = drain up to max completions from queue
        if n >= min:
            copy n completions to user memory
            return n
        register current thread as waiter
        unlock port
        block_current_thread()       // scheduler marks Blocked, yields
        // ... woken by post() or timeout ...
    if timeout expired:
        return completions drained so far (may be 0)
```

This reuses the existing `scheduler::block_current_thread()` /
`scheduler::unblock()` pattern from the pipe and waitpid implementations.

### Sync fallback worker for OP_READ / OP_WRITE

Existing `FileHandle` implementations (VfsHandle, ConsoleHandle, PipeReader)
are synchronous and blocking.  To integrate them with the CompletionPort
without rewriting every handle:

1. `io_submit` for OP_READ/OP_WRITE spawns an async task (via the existing
   executor).
2. The task calls `osl::blocking::blocking()` which blocks a scheduler
   thread on the synchronous `FileHandle::read()` or `FileHandle::write()`.
3. When the blocking call returns, the task posts a completion to the port.

This means each in-flight OP_READ/OP_WRITE consumes one scheduler thread
while blocked.  Acceptable for the initial implementation; a true async
FileHandle path can be added later.

```
io_submit(OP_READ, fd, buf, len):
    spawn async {
        let result = blocking(|| {
            file_handle.read(buf, len)
        });
        port.lock().post(IoCompletion {
            user_data,
            result: result as i64,
            opcode: OP_READ,
            flags: 0,
        });
    }
```

---

## Integration with Existing Infrastructure

### FileHandle trait

No changes to the `FileHandle` trait in Phase 1.  The sync fallback worker
bridges existing handles.

In a future phase, an optional `submit_async()` method could be added to
`FileHandle` for handles that can natively post completions (e.g., a future
async virtio-blk driver):

```rust
pub trait FileHandle: Send + Sync {
    // ... existing methods ...

    /// Submit an async operation.  Default: not supported (use sync fallback).
    fn submit_async(&self, _op: &IoSubmission, _port: &Arc<Mutex<CompletionPort>>)
        -> Result<(), FileError>
    {
        Err(FileError::NotSupported)
    }
}
```

### Executor and timer reuse

- **Executor:** the existing async task executor spawns fallback worker tasks
  and timeout tasks.  No changes needed.
- **Timer:** `OP_TIMEOUT` uses the existing `libkernel::task::timer::Delay`
  (which builds on the LAPIC timer tick).  No new timer infrastructure.

### fd_table

The CompletionPort is stored in the process `fd_table` as a
`CompletionPortHandle`.  This means:

- `close(port_fd)` cleans up the port.
- `dup2` works (two fds alias the same port via Arc).
- `FD_CLOEXEC` / `close_cloexec_fds()` works for execve.
- No new kernel data structures outside the existing fd model.

### Syscall dispatch wiring

Add to `osl/src/dispatch.rs`:

```rust
501 => sys_io_create(a1 as u32),
502 => sys_io_submit(a1 as i32, a2 as *const IoSubmission, a3 as u32),
503 => sys_io_wait(a1 as i32, a2 as *mut IoCompletion, a3 as u32, a4 as u32, a5 as u64),
```

The `IoSubmission` and `IoCompletion` structs live in `osl/src/io_port.rs`
(new file).  The CompletionPort and CompletionPortHandle implementations live
in `libkernel/src/file.rs` alongside the existing handle types.

---

## Integration with Microkernel Primitives

The CompletionPort replaces the need for two separate primitives described
in [microkernel-design.md](microkernel-design.md):

- **IRQ fd** (Phase B) — instead of a standalone `IrqHandle` where `read()`
  blocks, the IRQ fd posts completions to a port via `OP_IRQ_WAIT`.  The
  driver submits an `OP_IRQ_WAIT` and reaps it alongside other completions.

- **Notification objects** (seL4-style) — unnecessary.  A port with manual
  `post()` (exposed as a future `io_post` syscall or via OP_NOP with
  user_data tagging) serves the same role.

### NIC driver example loop

```c
int port = io_create(0);
int irq_fd = open("/dev/irq/11", O_RDONLY);
int ring_fd = open("/dev/shm/txring", O_RDWR);

// Submit initial waits
IoSubmission subs[2] = {
    { .user_data = TAG_IRQ,  .opcode = OP_IRQ_WAIT,  .fd = irq_fd  },
    { .user_data = TAG_RING, .opcode = OP_RING_WAIT, .fd = ring_fd },
};
io_submit(port, subs, 2);

for (;;) {
    IoCompletion comp[8];
    int n = io_wait(port, comp, 8, /*min=*/1, /*timeout=*/0);

    for (int i = 0; i < n; i++) {
        switch (comp[i].user_data) {
        case TAG_IRQ:
            handle_interrupt();
            // Resubmit IRQ wait
            io_submit(port, &(IoSubmission){
                .user_data = TAG_IRQ, .opcode = OP_IRQ_WAIT, .fd = irq_fd
            }, 1);
            break;

        case TAG_RING:
            drain_tx_ring();
            // Resubmit ring wait
            io_submit(port, &(IoSubmission){
                .user_data = TAG_RING, .opcode = OP_RING_WAIT, .fd = ring_fd
            }, 1);
            break;
        }
    }
}
```

This single-threaded loop handles both IRQs and ring wakeups through one
blocking wait point — exactly the pattern needed for microkernel drivers.

---

## Shared-Memory Ring Optimisation (Future Phase 5)

Under high throughput, even one syscall per batch can become a bottleneck.
The ultimate optimisation is to map the submission and completion queues into
userspace as shared-memory ring buffers, eliminating syscalls entirely on the
hot path.

### io_setup_rings (future syscall 504)

```
io_setup_rings(port_fd, sq_size, cq_size) → (sq_mmap_offset, cq_mmap_offset)
```

After this call, the process `mmap`s the SQ and CQ regions from the port fd:

```c
void *sq = mmap(NULL, sq_size, PROT_READ|PROT_WRITE, MAP_SHARED, port_fd, sq_offset);
void *cq = mmap(NULL, cq_size, PROT_READ,            MAP_SHARED, port_fd, cq_offset);
```

The kernel and userspace communicate through atomic head/tail pointers in the
rings.  The kernel only needs to be notified (via `io_submit` with count=0 or
a dedicated `io_ring_enter` syscall) when:

- The SQ transitions from empty to non-empty (userspace submitted work while
  the kernel was idle).
- The waiter needs to block (no completions available, equivalent to
  `io_wait`).

This matches Linux io_uring's model.  It requires `MAP_SHARED` from
[mmap-design.md](mmap-design.md) Phase 5.

---

## Phased Implementation

### Phase 1: Core + OP_NOP + OP_TIMEOUT

**Goal:** Establish the CompletionPort kernel object, syscall interface, and
basic operations.

| Item | Detail |
|---|---|
| **Files** | `libkernel/src/file.rs` (CompletionPortHandle), `osl/src/io_port.rs` (new: structs + sys_io_*), `osl/src/dispatch.rs` (wire 501-503) |
| **Dependencies** | None — uses existing scheduler, timer, fd_table |
| **Delivers** | `io_create`, `io_submit`, `io_wait`; OP_NOP and OP_TIMEOUT |
| **Test** | Userspace program: create port, submit OP_NOP, io_wait returns immediately.  Submit OP_TIMEOUT(100ms), io_wait blocks ~100ms then returns. |

### Phase 2: OP_READ + OP_WRITE with Sync Fallback

**Goal:** Bridge existing FileHandle implementations into the completion
model.

| Item | Detail |
|---|---|
| **Files** | `osl/src/io_port.rs` (add fallback worker logic) |
| **Dependencies** | Phase 1; existing `osl::blocking::blocking()` |
| **Delivers** | OP_READ and OP_WRITE on console, pipe, and VFS file fds |
| **Test** | Submit OP_WRITE to stdout + OP_READ from a file fd, reap both completions.  Verify data matches. |

### Phase 3: OP_IRQ_WAIT

**Goal:** Hardware interrupt delivery through the completion port.

| Item | Detail |
|---|---|
| **Files** | `libkernel/src/file.rs` (IrqHandle with port integration), `osl/src/io_port.rs` (OP_IRQ_WAIT handler) |
| **Dependencies** | Phase 1; [microkernel-design.md](microkernel-design.md) Phase B (IRQ fd infrastructure) |
| **Delivers** | Submit OP_IRQ_WAIT on an IRQ fd, ISR posts completion to port |
| **Test** | Register IRQ fd for a timer interrupt, submit OP_IRQ_WAIT, verify completion arrives after interrupt fires. |

### Phase 4: OP_RING_WAIT

**Goal:** Shared-memory ring buffer wakeup through the completion port.

| Item | Detail |
|---|---|
| **Files** | `osl/src/io_port.rs` (OP_RING_WAIT handler), new ring wakeup primitive |
| **Dependencies** | Phase 1; shared memory ([mmap-design.md](mmap-design.md) Phase 5); ring buffer primitive |
| **Delivers** | Submit OP_RING_WAIT, producer-side wakeup posts completion |
| **Test** | Two processes sharing a ring buffer.  Producer writes + signals, consumer reaps OP_RING_WAIT completion. |

### Phase 5: Shared-Memory SQ/CQ Rings

**Goal:** Zero-syscall submission and completion for high-throughput paths.

| Item | Detail |
|---|---|
| **Files** | `osl/src/io_port.rs` (io_setup_rings), `libkernel/src/file.rs` (mmap support on CompletionPortHandle) |
| **Dependencies** | Phase 1; `MAP_SHARED` from [mmap-design.md](mmap-design.md) Phase 5 |
| **Delivers** | Userspace-mapped SQ/CQ rings, kernel polls SQ on io_ring_enter |
| **Test** | Submit OP_NOP via shared-memory SQ (no io_submit syscall), reap from CQ, verify completion. |

---

## Dependency Graph

```
                     ┌────────────────────────┐
                     │  Phase 1               │
                     │  Core + NOP + TIMEOUT   │
                     │  (no external deps)     │
                     └──┬──────────┬───────┬──┘
                        │          │       │
               ┌────────▼──┐   ┌──▼────┐  │
               │  Phase 2   │  │       │  │
               │  READ/WRITE│  │       │  │
               │  sync fbk  │  │       │  │
               └────────────┘  │       │  │
                               │       │  │
          ┌────────────────────┘       │  │
          │                            │  │
  ┌───────▼────────┐    ┌─────────────▼──▼───────────────┐
  │  Phase 3        │    │  Phase 5                       │
  │  OP_IRQ_WAIT    │    │  Shared-memory SQ/CQ rings     │
  │                 │    │                                 │
  │  requires:      │    │  requires:                      │
  │  microkernel    │    │  mmap Phase 5 (MAP_SHARED)      │
  │  Phase B        │    └─────────────────────────────────┘
  └─────────────────┘
                        ┌──────────────────────────────────┐
                        │  Phase 4                         │
                        │  OP_RING_WAIT                    │
                        │                                  │
                        │  requires:                       │
                        │  mmap Phase 5 (MAP_SHARED)       │
                        │  + ring buffer primitive          │
                        └──────────────────────────────────┘

Cross-dependencies:
  microkernel Phase B ──────▶ CompletionPort Phase 3
  mmap Phase 5 (MAP_SHARED) ──▶ CompletionPort Phases 4, 5
```

Phase 1 is self-contained.  Phase 2 depends only on Phase 1.  Phases 3, 4,
and 5 depend on Phase 1 plus external primitives from the microkernel and
mmap designs.  Phases 3, 4, and 5 are independent of each other.

---

## Key Design Decisions

### Syscall-first, not ring-first

Phase 1 uses traditional syscalls (`io_submit`/`io_wait`).  Shared-memory
rings are deferred to Phase 5.  This avoids coupling the initial
implementation to `MAP_SHARED` (which is not yet implemented) and keeps the
kernel-side logic simple.

### Port as fd

The CompletionPort is an fd in the process's `fd_table`, not a special kernel
handle type.  This reuses existing infrastructure (close, dup2, CLOEXEC,
fd_table cleanup on exit) and avoids inventing a parallel handle namespace.

### Eager posting

Completions are pushed to the port's queue immediately when the operation
finishes (or the ISR fires).  There is no lazy/deferred completion model.
This is simpler and matches the existing block/unblock scheduling model.

### Single-threaded wait

Only one thread may block in `io_wait` per port.  This is a deliberate
constraint matching the single-threaded driver loop model.  Multi-threaded
consumers can use multiple ports.  This avoids thundering-herd wake-up logic
and lock contention on the completion queue.

### user_data for demux

Every submission carries a `u64 user_data` returned verbatim in the
completion.  The kernel never inspects this field.  The caller uses it to
identify which logical operation completed (e.g., `TAG_IRQ`, `TAG_RING`,
a pointer to a request struct).  This is the same pattern used by io_uring,
IOCP, and Fuchsia ports.

### Custom syscall numbers (501-503)

Continuing the custom numbering from `spawn` (500).  These are not Linux
syscall numbers — ostoo already uses custom numbers for OS-specific
functionality.  If Linux compatibility is needed later, a shim layer can map
Linux's `io_uring_setup`/`io_uring_enter` numbers to the ostoo equivalents.

### Kernel-buffered queue

The completion queue lives in kernel memory (`VecDeque<IoCompletion>`), not
shared memory.  `io_wait` copies completions to user buffers.  Simple,
correct, and sufficient until Phase 5 adds the zero-copy shared-memory path.

### Sync fallback for existing FileHandles

Rather than rewriting ConsoleHandle, PipeReader, VfsHandle, etc. to be
async-aware, OP_READ/OP_WRITE spawn a blocking worker that calls the
existing synchronous `FileHandle::read()`/`FileHandle::write()` and posts the
completion when done.  This trades a scheduler thread per in-flight op for
zero changes to existing handle implementations.

### Timer via Delay

OP_TIMEOUT reuses the existing `libkernel::task::timer::Delay` rather than
introducing a new timer subsystem.  The LAPIC timer already provides 10ms
ticks; Delay builds on this.

### ISR-safe posting

`CompletionPort::post()` must work from interrupt context.  The Mutex around
the port disables interrupts while held (matching the existing
`spin::Mutex`-based Mutex in libkernel).  The `scheduler::unblock()` call is
already ISR-safe (it just pushes to the ready queue).

### No cancellation in Phase 1

Submitted operations cannot be cancelled.  This avoids the complexity of
cancellation tokens, in-progress state tracking, and partial-completion
semantics.  Cancellation support can be added later as an `io_cancel` syscall
(504) once the basic model is proven.

### Completion-oriented, not readiness-oriented

The port reports "operation X is done" (completion), not "fd Y is readable"
(readiness).  This is a deliberate choice:

- Completion avoids the double-syscall problem (wait for ready, then do I/O).
- Completion naturally supports heterogeneous event sources (timers, IRQs)
  that don't have a "ready" state.
- Readiness can be emulated on top of completion (submit a zero-length
  OP_READ as a readiness probe) but not vice versa.
