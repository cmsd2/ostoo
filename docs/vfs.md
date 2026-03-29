# Virtual Filesystem (VFS) Layer

## Overview

The VFS layer provides a uniform path namespace over multiple filesystems.
Before its introduction, the shell called the exFAT driver directly; adding a
second filesystem would have required invasive shell changes.  The VFS
decouples path resolution and filesystem dispatch so that new drivers slot in
without touching the shell.

Key properties:

- **Enum dispatch** — no heap-allocating `Pin<Box<dyn Future>>` trait objects.
- **Mount table** — filesystems are attached at arbitrary absolute paths.
- **Lock safety** — the mount-table lock is never held across an `await` point.
- **No new Cargo dependencies** — everything already present in the workspace.

---

## Source layout

```
devices/src/
  vfs/
    mod.rs          — public API, mount table, path resolution
    exfat_vfs.rs    — ExfatVfs: wraps virtio-blk + exFAT driver
    plan9_vfs.rs    — Plan9Vfs: wraps virtio-9p P9Client
    proc_vfs/       — ProcVfs: synthetic kernel-info filesystem (mod.rs + generator submodules)
```

---

## Public API (`devices::vfs`)

```rust
// Types
pub struct VfsDirEntry { pub name: String, pub is_dir: bool, pub size: u64 }

pub enum VfsError {
    IoError, NotFound, NotAFile, NotADirectory, FileTooLarge, NoFilesystem,
}

pub enum AnyVfs { Exfat(ExfatVfs), Plan9(Plan9Vfs), Proc(ProcVfs) }

// Functions
pub fn  mount(mountpoint: &str, fs: AnyVfs);
pub async fn list_dir(path: &str)  -> Result<Vec<VfsDirEntry>, VfsError>;
pub async fn read_file(path: &str) -> Result<Vec<u8>,          VfsError>;
pub fn  with_mounts<F: FnOnce(&[(String, Arc<AnyVfs>)])>(f: F);
```

All paths supplied to `list_dir` and `read_file` must be absolute (the shell's
`resolve_path` runs first and normalises `.` / `..`).

---

## Enum dispatch

Async methods on trait objects require `Pin<Box<dyn Future>>` — allocating and
verbose in `no_std`.  Instead, `AnyVfs` is a plain enum:

```rust
pub enum AnyVfs {
    Exfat(ExfatVfs),
    Plan9(Plan9Vfs),
    Proc(ProcVfs),
}

impl AnyVfs {
    pub async fn list_dir(&self, path: &str) -> Result<Vec<VfsDirEntry>, VfsError> {
        match self {
            AnyVfs::Exfat(fs) => fs.list_dir(path).await,
            AnyVfs::Plan9(fs) => fs.list_dir(path).await,
            AnyVfs::Proc(fs)  => fs.list_dir(path).await,
        }
    }
    // read_file, fs_type likewise
}
```

Adding a new filesystem = add one variant + three match arms (`list_dir`,
`read_file`, `fs_type`).

---

## Mount table

```rust
lazy_static! {
    static ref MOUNTS: spin::Mutex<Vec<(String, Arc<AnyVfs>)>> = ...;
}
```

Entries are kept **sorted longest-mountpoint-first** so resolution is a simple
linear scan — the first match wins without any backtracking.

`mount()` replaces an existing entry at the same mountpoint, then re-sorts.

`Arc<AnyVfs>` is cloned out of the lock before any `.await`; the spinlock is
never held across a suspension point.

### Path resolution rules

| Situation | Mountpoint | Request path | Rel path passed to driver |
|-----------|-----------|-------------|--------------------------|
| Exact match | `/proc` | `/proc` | `/` |
| Prefix match | `/proc` | `/proc/tasks` | `/tasks` |
| Root pass-through | `/` | `/docs/foo` | `/docs/foo` |
| No match | — | `/missing` | `VfsError::NoFilesystem` |

```rust
fn resolve(path: &str) -> Option<(Arc<AnyVfs>, String)> {
    for (mp, fs) in MOUNTS.lock().iter() {
        if mp == "/"          { return Some((clone(fs), path.into())); }
        if path == mp         { return Some((clone(fs), "/".into())); }
        if path.starts_with(mp) && path[mp.len()..].starts_with('/') {
            return Some((clone(fs), path[mp.len()..].into()));
        }
    }
    None
}
```

---

## ExfatVfs

`ExfatVfs` wraps a `BlkInbox` (the virtio-blk actor's mailbox) and delegates
to the existing `devices::virtio::exfat` functions.  It calls `open_exfat`
fresh on every request — identical to the pre-VFS shell behaviour.

```
ExfatVfs::list_dir / read_file
    └─ exfat::open_exfat  (detects bare/MBR/GPT layout)
    └─ exfat::list_dir / read_file
```

`ExfatError` → `VfsError` mapping:

| ExfatError | VfsError |
|---|---|
| NoDevice / IoError / NotExfat / UnknownPartitionLayout | IoError |
| PathNotFound | NotFound |
| NotAFile | NotAFile |
| NotADirectory | NotADirectory |
| FileTooLarge | FileTooLarge |

---

## Plan9Vfs

`Plan9Vfs` wraps an `Arc<P9Client>` and delegates to the 9P2000.L client.
Unlike `ExfatVfs` (which goes through the actor/mailbox path), the P9 client
performs synchronous virtio-9p device I/O directly under a `spin::Mutex`.

```
Plan9Vfs::list_dir / read_file
    └─ P9Client::list_dir / read_file
        └─ VirtIO9p::request (virtio-drivers)
```

`P9Error` → `VfsError` mapping:

| P9Error | VfsError |
|---|---|
| ServerError(2) (ENOENT) | NotFound |
| ServerError(20) (ENOTDIR) | NotADirectory |
| ServerError(21) (EISDIR) | NotAFile |
| ServerError(_) / DeviceError | IoError |
| BufferTooSmall / InvalidResponse / Utf8Error | IoError |

The `list_dir` result sets `is_dir` from the dirent's `dtype` field (4 = DT_DIR)
or the qid type bit (0x80 = directory).  The `size` field is 0 since `readdir`
does not report file sizes — a follow-up `stat` per entry could be added later.

See [`docs/virtio-9p.md`](virtio-9p.md) for the full 9P driver documentation.

---

## ProcVfs

A synthetic filesystem with no block I/O.  All content is computed on demand.

| VFS path | Relative path seen by driver | Content |
|----------|------------------------------|---------|
| `/proc` | `/` | directory listing |
| `/proc/tasks` | `/tasks` | `ready: N  waiting: M\n` |
| `/proc/uptime` | `/uptime` | `Ns\n` |
| `/proc/drivers` | `/drivers` | one `name  State` line per driver |

Data sources:
- `executor::ready_count()` / `executor::wait_count()` — task queue depths
- `timer::ticks() / TICKS_PER_SECOND` — seconds since boot
- `driver::with_drivers()` — registered driver names and states

---

## Kernel initialisation (`kernel/src/main.rs`)

```rust
// Probe virtio-9p and create a shared P9Client.
let p9_client = probe_9p();  // returns Option<Arc<P9Client>>

// If 9p is available, always mount at /host.
if let Some(ref client) = p9_client {
    devices::vfs::mount("/host", AnyVfs::Plan9(Plan9Vfs::new(Arc::clone(client))));
}

// Always mount /proc — available without a block device.
devices::vfs::mount("/proc", AnyVfs::Proc(ProcVfs));

// Mount exFAT at / if virtio-blk was probed successfully.
let have_blk = if let Some(inbox) = registry::get::<..>("virtio-blk") {
    devices::vfs::mount("/", AnyVfs::Exfat(ExfatVfs::new(inbox)));
    true
} else { false };

// Fallback: mount 9p at / if no disk image is present.
if !have_blk {
    if let Some(client) = p9_client {
        devices::vfs::mount("/", AnyVfs::Plan9(Plan9Vfs::new(client)));
    }
}
```

This runs after both the virtio-blk and virtio-9p probe blocks and before task
spawning.  When both are present, exFAT owns `/` and 9p is at `/host`.  When
only 9p is present, it is mounted at both `/host` and `/` so that `/shell`
auto-launch works without a disk image.

---

## Shell integration (`kernel/src/shell.rs`)

The shell commands `ls`, `cat`, and `cd` now call the VFS API instead of the
exFAT driver directly:

```
ls [path]   →  devices::vfs::list_dir(&path).await
cat <path>  →  devices::vfs::read_file(&path).await
cd [path]   →  devices::vfs::list_dir(&target).await  (directory check)
```

A new `mount` command manages the mount table at runtime:

```
mount                   — list all mounts
mount proc <mountpoint> — attach a ProcVfs instance
mount blk  <mountpoint> — attach an ExfatVfs instance (requires virtio-blk)
```

---

## Example session

```
# Boot with 9p only (no disk image)
ostoo:/> mount
  /       9p
  /host   9p
  /proc   proc
ostoo:/> ls /
         shell
ostoo:/> ls /host
         shell
ostoo:/> cat /proc/uptime
42s

# Boot with both disk image and 9p
ostoo:/> mount
  /       exfat
  /host   9p
  /proc   proc
ostoo:/> ls /
  [DIR]        subdir
  [FILE    13]  hello.txt
ostoo:/> ls /host
         shell
ostoo:/> cat /host/shell | head
(binary ELF data)
```

---

## Extending the VFS

To add a new filesystem type:

1. Create `devices/src/vfs/<name>_vfs.rs` implementing `list_dir` and
   `read_file` as plain `async fn`.
2. Add a variant to `AnyVfs` in `mod.rs` and two match arms in `list_dir` /
   `read_file`.
3. Re-export the new type from `mod.rs`.
4. Mount it from `main.rs` or the shell's `mount` command.

No changes to the shell dispatch loop or path-resolution logic are required.
