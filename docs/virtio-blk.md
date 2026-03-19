# virtio-blk Block Device Driver

## Overview

The kernel includes a PCI virtio-blk driver that provides read/write access to
a QEMU virtual disk.  The driver is implemented using the `virtio-drivers` crate
(v0.13) and integrates with the existing actor/driver framework.

The driver is started automatically at boot if a virtio-blk PCI device is
found.  It is accessible from the shell via the `blk` commands.

---

## Architecture

```
QEMU virtio-blk device (PCIe, Q35 ECAM)
        │
        │  PciTransport (virtio-drivers)
        ▼
  VirtIOBlk<KernelHal, PciTransport>   ← virtio protocol implementation
        │
  spin::Mutex (actor + ISR safe)
        │
  VirtioBlkActor                        ← actor framework wrapper
        │
  Mailbox<ActorMsg<VirtioBlkMsg, VirtioBlkInfo>>
        │
  Shell / other actors                  ← consumers
```

---

## Components

### `devices/src/virtio/mod.rs` — HAL and transport

#### `KernelHal`

Implements the `virtio_drivers::Hal` unsafe trait, bridging the virtio-drivers
crate into the kernel memory model:

| Method | Implementation |
|---|---|
| `dma_alloc(pages)` | Allocates contiguous physical frames via `MemoryServices::alloc_dma_pages`; returns `(paddr, virt)` where virt is in the linear physical-memory window (`phys_mem_offset + paddr`). Pages are zeroed. |
| `dma_dealloc` | No-op. The frame allocator has no free operation; allocations are leaked (acceptable for MVP). |
| `mmio_phys_to_virt(paddr, size)` | Calls `MemoryServices::map_mmio_region` to ensure the physical range is mapped, then returns the linear-window virtual address. |
| `share(buffer)` | Performs a **page-table walk** via `MemoryServices::translate_virt` to find the physical address of any buffer (heap or DMA window). A plain `vaddr - phys_mem_offset` would be wrong for heap buffers. |
| `unshare` | No-op on x86 (cache-coherent). |

#### ECAM / `PciRoot`

The Q35 machine exposes a PCIe Extended Configuration Access Mechanism (ECAM)
region at physical address `0xB000_0000` (1 MiB, covering bus 0).

```
Physical 0xB000_0000  →  Virtual phys_mem_offset + 0xB000_0000
```

The mapping is created once during `libkernel_main` by calling
`MemoryServices::map_mmio_region`.  The resulting virtual base is stored in the
`ECAM_VIRT_BASE` atomic and used by `create_pci_root()` which constructs a
`PciRoot<MmioCam<'static>>` for the `virtio-drivers` transport layer.
(In virtio-drivers 0.13, `PciRoot` is generic over a `ConfigurationAccess`
implementation; `MmioCam` wraps the raw MMIO pointer with a `Cam::Ecam` mode.)

#### `create_pci_transport` (formerly `create_blk_transport`)

```rust
pub fn create_pci_transport(bus: u8, device: u8, function: u8) -> Option<PciTransport>
```

Wraps `PciTransport::new::<KernelHal, _>`, isolating `virtio-drivers` from the
kernel binary — the kernel crate does not depend on `virtio-drivers` directly.
Works for any virtio-pci device (blk, 9p, etc.), not just block devices.
`create_blk_transport` is kept as a legacy alias.

#### `register_blk_irq`

```rust
pub fn register_blk_irq(handler: fn()) -> Option<u8>
```

Registers a dynamic IDT handler for the virtio-blk interrupt (delegating to
`libkernel::interrupts::register_handler`).  Returns the allocated IDT vector,
which must be programmed into the device's MSI or IO APIC routing table.
IRQ-driven completion is not yet wired up (see [Limitations](#limitations)).

---

### `devices/src/virtio/blk.rs` — the actor

#### Messages

```rust
pub enum VirtioBlkMsg {
    Read(u64, Reply<Result<Vec<u8>, ()>>),   // sector, reply
    Write(u64, Vec<u8>, Reply<Result<(), ()>>), // sector, data, reply
}
```

#### Info

```rust
#[derive(Debug)]
pub struct VirtioBlkInfo {
    pub capacity_sectors: u64,
    pub reads:  u64,
    pub writes: u64,
}
```

Returned by `driver info virtio-blk` and `blk info`.

#### `VirtioBlkActor`

Owns a `spin::Mutex<VirtIOBlk<KernelHal, PciTransport>>`.  The mutex is needed
because both the actor task and (future) interrupt handler may access the device.

`unsafe impl Send + Sync` are required because `VirtIOBlk` contains raw DMA
buffer pointers, which are not auto-Send.  Access is always serialised through
the `spin::Mutex`.

#### Read/write flow

```
on_read(sector, reply):
  1. lock device → read_blocks_nb(sector, &mut req, buf, &mut resp) → token
  2. unlock device
  3. CompletionFuture.await  (busy-polls peek_used until the device signals done)
  4. lock device → complete_read_blocks(token, &req, buf, &resp)
  5. unlock device
  6. reply.send(Ok(buf))
```

Write is symmetric with `write_blocks_nb` / `complete_write_blocks`.

All of `read_blocks_nb`, `write_blocks_nb`, `complete_read_blocks`, and
`complete_write_blocks` are `unsafe fn` in `virtio-drivers` — the safety
contract is that the buffers remain valid and unpinned for the duration of the
I/O.  Because `buf`, `req`, and `resp` all live in the `async` state machine
on the heap, they are not moved or dropped between submit and complete.

#### `CompletionFuture`

```rust
struct CompletionFuture<'a> {
    device: &'a spin::Mutex<VirtIOBlk<KernelHal, PciTransport>>,
}

impl Future for CompletionFuture<'_> {
    type Output = ();
    fn poll(...) -> Poll<()> {
        if device.lock().peek_used().is_some() {
            Poll::Ready(())
        } else {
            cx.waker().wake_by_ref();   // reschedule immediately (busy-poll)
            Poll::Pending
        }
    }
}
```

This is a **busy-poll** future for MVP.  It re-schedules itself every executor
turn until the virtqueue returns a used buffer.  See [Limitations](#limitations)
for the planned IRQ-driven replacement.

---

### `libkernel/src/memory/mod.rs` — supporting APIs

Three methods were added to `MemoryServices` for virtio support:

#### `map_mmio_region(phys_start, size) -> VirtAddr`

Maps a physical MMIO range into the linear physical-memory window
(`phys_mem_offset + phys_start`) using 4 KiB pages with `PRESENT | WRITABLE |
NO_CACHE` flags.

Pages already mapped as 4 KiB pages are skipped silently (`Ok(_)`).  Pages
inside a 2 MiB or 1 GiB huge-page entry are also skipped
(`Err(TranslateError::ParentEntryHugePage)`) — they are already accessible
because the bootloader maps all physical RAM using 2 MiB huge pages.

This huge-page check was the fix for the `map_to failed: ParentEntryHugePage`
panic that occurred when mapping the ECAM region.

#### `alloc_dma_pages(pages) -> Option<PhysAddr>`

Allocates `pages` physically-contiguous 4 KiB frames from the
`BootInfoFrameAllocator`.  Panics if frames are not contiguous (very unlikely
with the sequential allocator).

#### `translate_virt(virt) -> Option<PhysAddr>`

Walks the active `RecursivePageTable` to find the physical address for any virtual
address, regardless of page size (4 KiB, 2 MiB, or 1 GiB).

This is used by `KernelHal::share` to convert heap buffer addresses to physical
addresses.  A simple `vaddr - phys_mem_offset` subtraction would be wrong for
heap buffers (which live at `HEAP_START`, not in the linear physical window),
producing garbage physical addresses and causing QEMU to report
`virtio: zero sized buffers are not allowed`.

---

## Boot Sequence

```
libkernel_main()
  1. memory::init_services(mapper, frame_allocator, phys_mem_offset, map)
  2. map_mmio_region(0xB000_0000, 1 MiB)   ← ECAM
     virtio::set_ecam_base(ecam_virt)
  3. devices::pci::init()                   ← scan CF8/CFC config space
  4. find_devices(0x1AF4, 0x1042)           ← probe modern-transitional first
     find_devices(0x1AF4, 0x1001)           ← then legacy
  5. virtio::create_pci_transport(bus, dev, func)
       └─ PciRoot::new(MmioCam::new(ECAM_VIRT_BASE, Cam::Ecam))
          PciTransport::new::<KernelHal, _>(&mut root, df)
  6. VirtioBlkActor::new(transport)
  7. VirtioBlkActorDriver::new(actor)
  8. driver::register + registry::register("virtio-blk", inbox)
  9. driver::start_driver("virtio-blk")
     → "[kernel] virtio-blk registered"
```

---

## Shell Commands

| Command | Description |
|---|---|
| `blk info` | Print capacity, read count, and write count |
| `blk read <sector>` | Read 512 bytes from sector N; hex-dump first 64 bytes |
| `blk ls [path]` | List exFAT directory (see [exfat.md](exfat.md)) |
| `blk cat <path>` | Print exFAT file as text (see [exfat.md](exfat.md)) |
| `ls [path]` | Alias for `blk ls` |
| `cat <path>` | Alias for `blk cat` |
| `driver info virtio-blk` | Same info via the generic driver info command |
| `driver stop virtio-blk` | Stop the actor (mailbox closed; no further I/O) |
| `driver start virtio-blk` | Restart the actor |

---

## Running with a Disk

```sh
# Create a blank 64 MiB disk image (once):
make disk

# Build and run with the disk attached:
make run
```

The `run` target adds:
```
-drive file=disk.img,format=raw,if=none,id=hd0
-device virtio-blk-pci,drive=hd0
```

The kernel uses a Q35 machine (`-machine q35`) which provides native PCIe and
ECAM support.

To run without a disk (e.g. for quick boot tests):
```sh
make run-nodisk
```

---

## PCI Device IDs

| Device ID | Variant |
|---|---|
| `0x1AF4:0x1042` | Modern-transitional virtio-blk (QEMU default) |
| `0x1AF4:0x1001` | Legacy virtio-blk |

Both are probed at boot; modern-transitional is tried first.

---

## Key Files

| File | Role |
|---|---|
| `devices/src/virtio/mod.rs` | `KernelHal`, ECAM state, `create_pci_transport`, `register_blk_irq` |
| `devices/src/virtio/blk.rs` | `VirtioBlkActor`, `VirtioBlkMsg`, `VirtioBlkInfo`, `CompletionFuture` |
| `devices/src/virtio/p9_proto.rs` | 9P2000.L wire protocol encode/decode |
| `devices/src/virtio/p9.rs` | `P9Client` — high-level 9P client wrapping `VirtIO9p` |
| `kernel/src/main.rs` | ECAM mapping, PCI probe (blk + 9p), actor registration |
| `devices/src/virtio/exfat.rs` | exFAT partition detection, filesystem, path walk |
| `kernel/src/shell.rs` | `blk info`, `blk read`, `blk ls`, `blk cat`, `ls`, `cat`, `cd`, `pwd` |
| `libkernel/src/memory/mod.rs` | `map_mmio_region`, `alloc_dma_pages`, `translate_virt` |
| `Makefile` | `disk`, `run`, `run-nodisk` targets |

---

## Limitations

### Busy-poll completion

`CompletionFuture` re-schedules itself every executor turn, consuming CPU until
the device completes I/O.  The intended replacement is an `AtomicWaker`-based
future that sleeps until the IRQ handler calls `wake()`:

```rust
static IRQ_WAKER: AtomicWaker = AtomicWaker::new();

fn virtio_blk_irq_handler() {
    IRQ_PENDING.store(true, Ordering::Release);
    IRQ_WAKER.wake();
}
```

This requires programming the device's MSI capability or IO APIC routing with
the vector returned by `register_blk_irq`.  The infrastructure exists; wiring
is the remaining work.

### No DMA free

`dma_dealloc` is a no-op.  Freed DMA pages are leaked.  The `BootInfoFrameAllocator`
has no reclamation path.  Acceptable for MVP; a proper frame allocator with free
would be needed for a production kernel.

### Single device

The IRQ state (`IRQ_PENDING`) is a file-level static, supporting only one
virtio-blk device.  Multi-device support would require per-device state.

### Heap size

The kernel heap is 100 KiB.  DMA allocations come from the frame allocator (not
the heap), but `Vec<u8>` read buffers and `BlkReq`/`BlkResp` structs live on the
heap.  Sustained I/O workloads should remain well within the limit.
