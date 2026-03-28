# Microkernel Design

## Overview

This document explores evolving ostoo towards a microkernel architecture where
device drivers run as userspace processes rather than in the kernel.  It covers
the motivation, the kernel primitives required, how other systems solve this,
and a migration path from the current monolithic design.

See also: [networking-design.md](networking-design.md) for how networking
specifically fits into either a monolithic or microkernel architecture.

---

## Why Consider a Microkernel

- **Fault isolation.**  A buggy NIC or filesystem driver crashes its own
  process, not the kernel.  The system can restart it.
- **Reduced TCB.**  The trusted computing base shrinks to just the kernel
  primitives.  Less kernel code = fewer exploitable bugs.
- **Hot-swappable drivers.**  Replace or upgrade a driver without rebooting.
- **Security boundaries.**  A compromised driver only has access to the
  specific device it manages, not all of kernel memory.

---

## How Other Systems Do It

### Redox OS — Scheme-Based IPC

Redox uses **schemes** as both a namespace and IPC channel.  A scheme is a
named resource (like `tcp:`, `udp:`, `disk:`) backed by a userspace daemon.
Standard file operations (`open`/`read`/`write`/`close`) become IPC messages
routed through the kernel to the scheme daemon.

- `smolnetd` daemon implements `tcp:`/`udp:` schemes using smoltcp
- NIC drivers (e.g. `e1000d`) are separate userspace processes
- The kernel provides `irq:N` and `memory:` schemes for hardware access
- Recent work adds **io_uring**-style shared-memory rings for high-throughput
  driver-to-driver communication, bypassing the kernel data path

Strengths: elegant "everything is a file" model, reuses POSIX-like ops for
IPC.  Weaknesses: every cross-component operation involves at least one
context switch through the kernel (mitigated by io_uring for data paths).

### seL4 — Capabilities + Shared Memory Rings

seL4 provides minimal kernel primitives and lets userspace build everything
else:

- **Synchronous endpoints** for RPC (~0.2us round-trip on ARM64).  Small
  messages transfer entirely in CPU registers (zero copy).  The kernel has a
  **fastpath** for `seL4_Call`/`seL4_ReplyRecv` with direct process switch
  (sender switches directly to receiver without full scheduler invocation).
- **Notifications** for async signaling (interrupt delivery, ring wakeups).
  A notification word acts as a bitmask of binary semaphores — different
  signalers use different bits, so one notification object can multiplex
  multiple event sources.
- **Capability system** — every kernel object (endpoints, frames, interrupts,
  page tables) is accessed through unforgeable capability tokens stored in
  per-thread CSpaces.  Capabilities can be derived with reduced rights,
  transferred via IPC, or revoked.
- **sDDF** (seL4 Device Driver Framework) uses SPSC shared-memory ring buffers
  for zero-copy packet passing between NIC driver → multiplexer → application.

sDDF on an iMX8 ARM board with a 1 Gb/s NIC **saturates the link at ~95% CPU**
while Linux on the same hardware maxes out at ~600 Mb/s.  The shared-memory
ring design avoids Linux's sk_buff allocation/copy overhead.

### Minix 3 — Classic Microkernel

Each driver is a separate user-mode process.  The kernel provides:

- `sys_irqsetpolicy()` — subscribe to hardware interrupts
- `HARD_INT` notification messages — delivered on next `receive()`
- `SYS_DEVIO` — read/write I/O ports from userspace
- `SYS_PRIVCTL` — per-driver access control (which ports, IRQs, memory
  regions are permitted, declared in `/etc/system.conf.d/`)
- `SYS_UMAP`/`SYS_VUMAP` — virtual-to-physical translation for DMA setup
- Fixed-length synchronous IPC: `send()`/`receive()`/`sendrec()` + `notify()`

Networking uses lwIP in a separate server process.  A received packet
traverses: NIC driver → lwIP server → VFS → application (~4 IPC hops per
direction).

### Fuchsia (Zircon) — Driver Hosts + FIDL

Drivers are shared libraries loaded into **driver host** processes.  Multiple
drivers can be **colocated** in the same host for zero-overhead communication.

- **FIDL** (Fuchsia Interface Definition Language) for typed IPC across
  process boundaries via Zircon channels
- **DriverTransport** for in-process communication between colocated drivers
  (no kernel involvement — can invoke handler directly in the same stack frame)
- **VMO** (Virtual Memory Objects) for shared memory and device MMIO.  Created
  by the bus driver via `zx_vmo_create_physical()`, passed as handles to device
  drivers, mapped into their address space
- **BTI** (Bus Transaction Initiators) for DMA with IOMMU control.
  `zx_bti_pin()` pins a VMO and returns device-physical addresses
- **Interrupt objects** — created by bus drivers, delivered via
  `zx_interrupt_wait()` (sync) or port binding (async)
- Control/data plane split: FIDL messages for setup, pre-allocated shared VMOs
  for bulk data transfer

Key insight: colocation lets Fuchsia avoid the microkernel IPC tax for
tightly-coupled drivers while still getting process isolation for less
trusted ones.

---

## Kernel Primitives Needed

To support userspace drivers, ostoo must provide these minimal primitives.

### 1. Physical Memory Mapping

Map device MMIO BARs into a userspace process's address space.

**ostoo today:** `mmio_phys_to_virt()` maps BARs into kernel space only.
`sys_mmap` only supports anonymous private pages (returns `-ENOSYS` for
non-anonymous).

**Options:**

- Extend `sys_mmap` with `MAP_SHARED` + a device fd (Linux-like `/dev/mem`)
- New `sys_mmap_device(phys_addr, size, perms)` syscall (simpler)
- Capability-based: kernel creates a "device memory" handle, process maps it
  via `mmap` on that handle's fd

The capability-based approach (device memory as an fd) fits ostoo's existing
fd_table model well and avoids giving processes a raw
"map any physical address" primitive.

### 2. IRQ Delivery to Userspace

Deliver hardware interrupts as events to a userspace driver process.

**ostoo today:** Interrupts handled entirely in kernel (APIC/IOAPIC routing
to kernel ISRs).  No mechanism to notify userspace of IRQs.

**Options:**

| Approach | Description | Complexity |
|---|---|---|
| IRQ fd | `open("/dev/irq/N")`, `read()` blocks until IRQ fires | Low — reuses fd/FileHandle |
| eventfd | New `eventfd()` syscall, kernel writes on IRQ | Medium |
| Signal | Deliver `SIGIO` to driver process on IRQ | Medium — requires signals |
| Notification object | Dedicated kernel object (seL4-style) | High — new primitive |

**Recommendation: IRQ fd.**  Create an `IrqHandle` implementing `FileHandle`
where `read()` blocks until the interrupt fires and returns a count.  The
kernel ISR masks the interrupt and calls `unblock()` on the waiting thread.
After handling, the driver writes to the fd to re-enable the interrupt.  This
fits the existing scheduler block/unblock pattern and the fd_table model.

### 3. Fast IPC

Efficient communication between driver processes and between drivers and
applications.

**ostoo today:** Only pipes (byte streams, no message boundaries, kernel-
buffered copy).

**Progressive options:**

1. **Pipes** (have now) — sufficient for prototyping, ~2 copies per message
2. **Unix domain sockets** — adds message boundaries (`SOCK_DGRAM`),
   ancillary data for fd passing (needed to transfer device handles between
   processes)
3. **Shared memory regions** — `mmap(MAP_SHARED)` or `shmget`/`shmat` for
   zero-copy ring buffers between cooperating processes
4. **io_uring-style rings** — lock-free SPSC queues in shared memory, kernel
   only involved for wakeups when rings transition empty → non-empty

For initial microkernel work, **pipes + shared memory** is sufficient.  The
performance-critical path (packet data) goes through shared memory rings;
the control path (setup, teardown) goes through pipes or sockets.

The long-term goal is a pattern where the control plane uses message-based
IPC and the data plane uses shared-memory rings, matching seL4 sDDF and
Fuchsia's architecture.

### 4. Shared Memory

Map the same physical pages into multiple process address spaces.

**ostoo today:** Each process gets a private PML4.  `create_user_page_table`
copies kernel entries (256-510) but there is no mechanism for two user
processes to share pages.

**What's needed:**

- A shared memory object (named or anonymous) backed by physical frames
- `mmap()` to map it into each participating process
- Reference counting so pages are freed only when all mappers unmap
- Access control: processes must hold a handle/capability to map the region

**Design sketch:**

```
Process A                  Kernel                     Process B
    │                                                      │
    ├─ shmget(key, size) ──→ allocate frames ←── shmget(key, size) ─┤
    │                        ref_count = 2                 │
    ├─ shmat(id) ──────────→ map into A's PML4             │
    │                                                      │
    │                        map into B's PML4 ←─── shmat(id) ──────┤
    │                                                      │
    │  (A and B now read/write the same physical pages)    │
```

Alternative: fd-based approach where `mmap(fd, MAP_SHARED)` on a memfd works
like Linux.  This avoids inventing SysV-style APIs and reuses the fd model.

### 5. DMA Support

Userspace drivers need physical addresses for device DMA programming.

**ostoo today:** `KernelHal::dma_alloc()` allocates physically-contiguous
pages and returns `(paddr, NonNull<u8>)`.  `share()` translates virtual to
physical via page table walk.  Both are kernel-internal.

**What's needed:**

- A syscall to allocate DMA-capable memory: physically contiguous, pinned,
  and mapped into the calling process.  Returns both the virtual address and
  the physical address (the driver needs the physical address to program
  the device's DMA descriptors).
- Or: a two-step model where the kernel allocates DMA buffers and provides an
  fd.  The driver maps the fd and queries the physical address separately.

The fd-based model is safer (physical addresses are not exposed until the
driver proves it holds the right handle) and aligns with Fuchsia's BTI/PMT
pattern.

### 6. Access Control

Prevent arbitrary processes from mapping device memory or claiming IRQs.

**Options (increasing sophistication):**

| Approach | Description | Precedent |
|---|---|---|
| **Init-time grant** | Only the init process can spawn drivers with device access, configured at spawn time | Simple, sufficient for single-user OS |
| **Capability-based** | Kernel objects (device memory, IRQ handles) are capabilities obtained from a parent or resource manager | seL4, Fuchsia |
| **Policy-based** | Configuration file declares which programs may access which devices | Minix 3 |

**Recommendation: Init-time grant** for the first iteration.  The kernel
shell or init process spawns driver processes and passes them fds for their
device MMIO region and IRQ.  The driver inherits these fds across exec.  No
new kernel objects needed — just careful fd management.

Later, this can evolve towards a capability model where device handles are
kernel objects with typed permissions.

---

## What Stays in the Kernel

Even in a full microkernel, some things must remain:

- **CPU scheduling** — timer interrupts, context switching, thread states
- **Memory management** — page tables, frame allocation, address space setup
- **IPC primitives** — message passing, shared memory mapping, notifications
- **Interrupt routing** — top-half ISR that masks IRQ and notifies userspace
- **Capability/access control** — enforce which processes access which devices
- **Boot and early init** — PCI enumeration can eventually be delegated, but
  initial hardware discovery typically starts in kernel

Everything else — device drivers, filesystems, network stacks, even the
TCP/IP protocol processing — can live in userspace.

---

## Current ostoo Gaps

Summary of what exists vs what's needed:

| Primitive | Current State | Gap |
|---|---|---|
| Physical memory mapping | Kernel-only (`mmio_phys_to_virt`) | Need userspace mapping syscall |
| IRQ delivery | Kernel ISRs only | Need IRQ fd or notification |
| IPC | Pipes only (byte stream, ~2 copies) | Need shared memory + message boundaries |
| Shared memory | None (private PML4 per process) | Need cross-process page sharing |
| DMA | Kernel-only (`dma_alloc`/`share`) | Need userspace DMA allocation |
| Access control | None (all processes equal) | Need per-process device permissions |
| mmap | Anonymous private only | Need MAP_SHARED, device mapping |
| ioctl | Not implemented | Need for device control |

---

## Migration Path

A phased approach that starts monolithic and progressively moves towards
microkernel:

### Phase A — Monolithic Drivers (current)

All drivers (virtio-blk, virtio-9p, and eventually virtio-net) run in kernel
space via the `devices` crate.  This is the working baseline.  Networking
is implemented in kernel with smoltcp (see networking-design.md).

### Phase B — Add Kernel Primitives

Implement foundational primitives without yet moving drivers out.  These are
independently useful:

1. **`mmap(MAP_SHARED)`** — shared memory between processes (needed for
   efficient multi-process programs even without microkernel goals)
2. **IRQ fd** — `irq_create(gsi)` syscall (504) returns an fd backed by
   `FdObject::Irq`.  Used with `OP_IRQ_WAIT` on a completion port.
   **Implemented** (see `libkernel/src/irq_handle.rs`, `osl/src/irq.rs`)
3. **Device MMIO mapping** — map physical BAR regions to userspace via an fd
4. **DMA allocation syscall** — allocate pinned, physically-contiguous pages
   accessible from userspace

### Phase C — Userspace NIC Driver

Move the virtio-net driver to a userspace process as a proof-of-concept:

- Process receives device MMIO fd and IRQ fd from init
- Maps the virtio-net PCI BAR into its address space
- Opens IRQ fd and polls/blocks for interrupts
- Allocates DMA buffers for virtqueue descriptors
- Communicates with the in-kernel TCP/IP stack via shared memory ring buffers

The TCP/IP stack stays in kernel at this stage.  This tests the driver
primitive infrastructure with a single, well-understood device.

### Phase D — Userspace TCP/IP Stack

Move smoltcp to a separate userspace server process:

- Receives raw Ethernet frames from NIC driver via shared memory rings
- Processes TCP/IP/ARP/DHCP
- Delivers data to applications via shared memory or kernel-mediated IPC
- The kernel's socket syscall handlers become thin IPC stubs that route
  requests to this server (preserving POSIX compatibility for musl)

### Phase E — Generalize

Apply the same pattern to other drivers:

- **virtio-blk** → userspace block driver + userspace filesystem server
- **virtio-9p** → userspace 9P client
- **Console/keyboard** → userspace terminal driver

At this point the kernel is a true microkernel: scheduler, memory management,
IPC, and capability enforcement only.  The `devices` and `osl` crates either
become userspace libraries or are restructured into per-driver binaries.

---

## Performance Considerations

### Context Switches Per Packet

| Path | Monolithic | Microkernel |
|---|---|---|
| NIC IRQ → driver | 0 (in kernel) | 1 (kernel → driver) |
| Driver → TCP/IP | 0 (function call) | 1 (shared memory signal) |
| TCP/IP → application | 1 (return to userspace) | 1 (IPC or signal) |
| **Total** | **1** | **3** (naive) / **1-2** (batched) |

### Why This Is Acceptable

- With shared-memory ring buffers and batching, the kernel is only involved
  for wakeups when rings transition empty → non-empty.
- Under load, driver and TCP/IP server can poll their rings without any kernel
  involvement (similar to Linux NAPI busy-polling).
- seL4 IPC round-trip: ~0.2us.  Network I/O latency: 25-500+us.  Even 3 IPC
  hops are a small fraction of total latency.
- The historical Mach-era penalty (50-100% overhead) is now 5-10% for general
  workloads and near-zero for I/O-dominated workloads.

### Key Optimisations

1. **Shared memory data plane** — kernel only signals, never copies data
2. **Batching** — process N packets per wakeup, not 1
3. **Direct process switch** — IPC sender switches directly to receiver
   without full scheduler invocation (seL4 fastpath)
4. **Polling under load** — skip notifications entirely when rings are busy
5. **Pre-allocated buffer pools** — no per-packet allocation
6. **Driver colocation** (Fuchsia-style) — run tightly-coupled drivers in the
   same address space when isolation between them is not needed

---

## Comparison Summary

| Aspect | Monolithic (Phase A) | Full Microkernel (Phase E) |
|---|---|---|
| Kernel code size | Large (drivers + protocols) | Small (primitives only) |
| Driver crash | Kernel panic | Restart driver process |
| Attack surface | Entire kernel | Minimal kernel + IPC |
| Performance | Best (no IPC overhead) | Good (shared memory amortises cost) |
| Implementation effort | Low | High (needs IPC, shared mem, caps) |
| POSIX compat | Direct | Kernel-mediated IPC stubs |
| ostoo readiness | Ready now | Needs phases B-E |
