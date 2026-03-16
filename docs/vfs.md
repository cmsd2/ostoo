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
    proc_vfs.rs     — ProcVfs: synthetic kernel-info filesystem
```

---

## Public API (`devices::vfs`)

```rust
// Types
pub struct VfsDirEntry { pub name: String, pub is_dir: bool, pub size: u64 }

pub enum VfsError {
    IoError, NotFound, NotAFile, NotADirectory, FileTooLarge, NoFilesystem,
}

pub enum AnyVfs { Exfat(ExfatVfs), Proc(ProcVfs) }

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
    Proc(ProcVfs),
}

impl AnyVfs {
    pub async fn list_dir(&self, path: &str) -> Result<Vec<VfsDirEntry>, VfsError> {
        match self {
            AnyVfs::Exfat(fs) => fs.list_dir(path).await,
            AnyVfs::Proc(fs)  => fs.list_dir(path).await,
        }
    }
    // read_file likewise
}
```

Adding a new filesystem = add one variant + two match arms.

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
// Always mount /proc — available without a block device.
devices::vfs::mount("/proc", AnyVfs::Proc(ProcVfs));

// Mount exFAT at / only if virtio-blk was probed successfully.
if let Some(inbox) = registry::get::<VirtioBlkMsg, VirtioBlkInfo>("virtio-blk") {
    devices::vfs::mount("/", AnyVfs::Exfat(ExfatVfs::new(inbox)));
}
```

This runs after the virtio-blk probe block and before task spawning.

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
# Boot without a disk image
ostoo:/> mount
  /proc  proc
ostoo:/> ls /proc
  [FILE     0]  tasks
  [FILE     0]  uptime
  [FILE     0]  drivers
ostoo:/> cat /proc/uptime
42s
ostoo:/> cat /proc/drivers
virtio-blk  Running
shell       Running
keyboard    Running

# Boot with an exFAT disk image
ostoo:/> mount
  /       exfat
  /proc   proc
ostoo:/> ls /
  [DIR]        subdir
  [FILE    13]  hello.txt
ostoo:/> cat /hello.txt
Hello, kernel!
ostoo:/> cd /proc && ls
  [FILE     0]  tasks
  [FILE     0]  uptime
  [FILE     0]  drivers
ostoo:/proc> mount proc /proc2
mounted proc at /proc2
ostoo:/proc> ls /proc2
  [FILE     0]  tasks
  [FILE     0]  uptime
  [FILE     0]  drivers
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
