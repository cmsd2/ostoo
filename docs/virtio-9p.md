# VirtIO 9P Host Directory Sharing

## Overview

The kernel includes a VirtIO 9P (9P2000.L) driver that shares a host directory
directly into the guest via QEMU's `-fsdev` mechanism.  This provides a
Docker-volume-like workflow: edit files on the host, they appear instantly in
the guest — no disk image rebuild needed.

The driver uses the `VirtIO9p` device from the `virtio-drivers` crate (v0.13)
and implements a minimal read-only 9P2000.L client on top.

---

## Architecture

```
Host directory (./user)
        │
  QEMU virtio-9p-pci device
        │
        │  PciTransport (virtio-drivers)
        ▼
  VirtIO9p<KernelHal, PciTransport>   ← virtio device, raw request/response
        │
  spin::Mutex (synchronous access)
        │
  P9Client                             ← 9P2000.L protocol client
        │
  Plan9Vfs                             ← VFS adapter
        │
  devices::vfs mount table             ← /host and optionally /
```

---

## Components

### `devices/src/virtio/p9_proto.rs` — Wire Protocol

Minimal 9P2000.L message encoding/decoding.  All messages use little-endian
wire format with a 7-byte header: `size[4] type[1] tag[2]`.

#### Message pairs implemented

| T-message | R-message | Type codes | Purpose |
|-----------|-----------|------------|---------|
| Tversion  | Rversion  | 100 / 101  | Protocol handshake (negotiates msize) |
| Tattach   | Rattach   | 104 / 105  | Mount filesystem, get root fid |
| Twalk     | Rwalk     | 110 / 111  | Traverse path components |
| Tlopen    | Rlopen    | 12 / 13    | Open a fid for reading |
| Tread     | Rread     | 116 / 117  | Read file data |
| Treaddir  | Rreaddir  | 40 / 41    | Read directory entries |
| Tgetattr  | Rgetattr  | 24 / 25    | Get file attributes (mode, size) |
| Tclunk    | Rclunk    | 120 / 121  | Release a fid |

Error responses use Rlerror (type 7) with a Linux errno code.

#### Key types

```rust
pub struct Qid { pub qid_type: u8, pub version: u32, pub path: u64 }
pub struct DirEntry9p { pub qid: Qid, pub offset: u64, pub dtype: u8, pub name: String }
pub struct Stat9p { pub mode: u32, pub size: u64, pub qid: Qid }
```

---

### `devices/src/virtio/p9.rs` — P9Client

High-level client wrapping `VirtIO9p`.  The device is accessed synchronously
through a `spin::Mutex` — no actor pattern needed since 9P access happens from
syscall context via `osl::blocking::blocking()`.

```rust
pub struct P9Client {
    device: Mutex<VirtIO9p<KernelHal, PciTransport>>,
    msize:  u32,       // negotiated max message size (typically 8192)
    next_fid: Mutex<u32>,
}
```

#### Construction

`P9Client::new(transport)` performs the handshake:
1. **Tversion** — negotiates protocol version ("9P2000.L") and max message size
2. **Tattach** — attaches root fid (fid 0) to the shared directory

#### Public methods

| Method | Flow |
|--------|------|
| `list_dir(path)` | walk → lopen → readdir (loop) → clunk |
| `read_file(path)` | walk → getattr (size) → lopen → read (loop) → clunk |
| `stat(path)` | walk → getattr → clunk |

Each method walks from the root fid, allocating a temporary fid that is clunked
after the operation completes.  The readdir and read loops consume data in
chunks of `msize - 64` bytes.

`list_dir` filters out `.` and `..` entries automatically.

---

### `devices/src/vfs/plan9_vfs.rs` — VFS Adapter

Follows the `ExfatVfs` pattern.  Wraps an `Arc<P9Client>` and maps:
- `DirEntry9p` → `VfsDirEntry` (dtype 4 or qid type 0x80 → `is_dir`)
- `P9Error` → `VfsError`

The P9Client methods are synchronous but the VFS interface is async.  Since the
virtio-9p device uses polling (no IRQ), blocking in an async context is
acceptable for MVP.

---

## QEMU Configuration

In `scripts/run.sh`:

```bash
-fsdev local,id=fsdev0,path=./user,security_model=none \
-device virtio-9p-pci,fsdev=fsdev0,mount_tag=hostfs
```

This shares the `./user` directory (where userspace binaries are built) into
the guest.  `security_model=none` disables host permission mapping, which is
appropriate since the guest is read-only.

---

## Boot Sequence

```
run_kernel()
  1. PCI probe: find_devices(0x1AF4, 0x1049)   ← modern virtio-9p
                find_devices(0x1AF4, 0x1009)   ← legacy
  2. create_pci_transport(bus, dev, func)
  3. P9Client::new(transport)
       └─ Tversion + Tattach handshake
  4. Arc::new(client)
  5. vfs::mount("/host", Plan9(Plan9Vfs::new(Arc::clone(&client))))
  6. If no virtio-blk:
       vfs::mount("/", Plan9(Plan9Vfs::new(client)))
```

---

## PCI Device IDs

| Device ID | Variant |
|---|---|
| `0x1AF4:0x1049` | Modern virtio-9p (probed first) |
| `0x1AF4:0x1009` | Legacy virtio-9p |

---

## Key Files

| File | Role |
|---|---|
| `devices/src/virtio/p9_proto.rs` | 9P2000.L wire protocol encode/decode |
| `devices/src/virtio/p9.rs` | `P9Client` — high-level 9P client |
| `devices/src/vfs/plan9_vfs.rs` | `Plan9Vfs` — VFS adapter |
| `devices/src/virtio/mod.rs` | `KernelHal`, `create_pci_transport` (shared with blk) |
| `kernel/src/main.rs` | 9P probe, mount at `/host` and fallback `/` |
| `scripts/run.sh` | QEMU `-fsdev` and `-device virtio-9p-pci` flags |

---

## Limitations

### Read-only

The 9P client only implements read operations (walk, lopen, read, readdir,
getattr).  Write, create, mkdir, remove, and rename are not supported.

### No fid recycling

Fid numbers are allocated monotonically and never reused.  With 32-bit fids
this is unlikely to be a problem in practice, but a long-running system
performing many file operations would eventually exhaust the fid space.

### Directory entry sizes

`list_dir` reports `size: 0` for all entries because `readdir` does not return
file sizes.  A per-entry `getattr` could be added but would increase the number
of 9P round-trips.

### Single device

Only one virtio-9p device is probed.  Multiple shared directories would require
iterating over all matching PCI devices and mounting each at a different path.

### Synchronous I/O

All 9P operations block the calling scheduler thread.  This is acceptable when
called from syscall context via `osl::blocking::blocking()`, but direct use
from async tasks would stall the executor.
