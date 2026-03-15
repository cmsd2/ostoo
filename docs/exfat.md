# exFAT Read-Only Filesystem

## Overview

The kernel includes a read-only exFAT filesystem driver that sits on top of the
virtio-blk block device.  It auto-detects bare exFAT volumes, MBR-partitioned
disks, and GPT-partitioned disks, then exposes simple directory-listing and
file-read operations through the shell.

The driver is implemented entirely in `devices/src/virtio/exfat.rs` with no
external dependencies.

---

## Architecture

```
Shell (ls / cat / cd / pwd)
        │
        │  open_exfat / list_dir / read_file
        ▼
  ExfatVol  ──── async sector reads ────▶  BlkInbox
        │                                     │
  Partition detection                   VirtioBlkActor
  Boot sector parse                     (virtio-blk driver)
  FAT traversal
  Dir entry parse
  Path walk
```

All filesystem I/O is done one 512-byte sector at a time via the `ask` pattern
on the virtio-blk actor's mailbox (`VirtioBlkMsg::Read`).

---

## Partition Auto-Detection

`open_exfat` reads sector 0 and applies the following decision tree:

```
sector0[3..11] == "EXFAT   "
  → bare exFAT (no partition table); volume starts at LBA 0

sector0[510..512] == [0x55, 0xAA]
  read sector 1
  sector1[0..8] == "EFI PART"
    → GPT: scan partition entries starting at the LBA stored in the header
      look for type GUID = EBD0A0A2-B9E5-4433-87C0-68B6B72699C7
      (on-disk mixed-endian: A2 A0 D0 EB E5 B9 33 44 87 C0 68 B6 B7 26 99 C7)
      read StartingLBA of the matching entry, verify "EXFAT   " there
  else
    → MBR: scan partition table at sector0[446..510]
      look for entry with type byte 0x07
      read LBA start (bytes 8–11 LE u32), verify "EXFAT   " there

else
  → ExfatError::UnknownPartitionLayout
```

Type 0x07 is shared by exFAT and NTFS.  The driver always verifies the OEM
name at the candidate partition's first sector before accepting it as exFAT.

---

## On-Disk Layout

### Boot Sector

| Offset | Size | Field | Notes |
|--------|------|-------|-------|
| 3 | 8 | `FileSystemName` | Must equal `"EXFAT   "` (with trailing space) |
| 80 | 4 | `FatOffset` | Sectors from volume start to FAT |
| 88 | 4 | `ClusterHeapOffset` | Sectors from volume start to data region |
| 96 | 4 | `FirstClusterOfRootDirectory` | Cluster number of root dir |
| 109 | 1 | `SectorsPerClusterShift` | `sectors_per_cluster = 1 << shift` |
| 510 | 2 | `BootSignature` | Must equal `[0x55, 0xAA]` |

### FAT (File Allocation Table)

An array of `u32` little-endian values.  Entry `N` holds the next cluster in
the chain for cluster `N`, or `0xFFFFFFFF` for end-of-chain.

```
fat_lba          = volume_lba + FatOffset
sector_of_entry  = fat_lba + (N * 4) / 512
byte_in_sector   = (N * 4) % 512
```

### Cluster Heap

```
cluster_lba(N) = cluster_heap_lba + (N − 2) * sectors_per_cluster
```

Cluster numbers start at 2; clusters 0 and 1 are reserved.

### Directory Entry Sets

Each entry is 32 bytes.  A file or directory is represented by a consecutive
set of three or more entries:

| Type byte | Name | Key fields |
|-----------|------|-----------|
| `0x85` | File | `[1]` SecondaryCount; `[4..6]` FileAttributes (bit 4 = directory) |
| `0xC0` | Stream Extension | `[8..16]` DataLength (u64 LE); `[20..24]` FirstCluster (u32 LE) |
| `0xC1`+ | File Name | `[2..32]` up to 15 UTF-16LE code units per entry |

Type `0x00` marks the end of directory; scanning stops immediately.
Any type byte with bit 7 clear (< `0x80`) is an unused or deleted entry and
is skipped.

---

## ExfatVol State

```rust
pub struct ExfatVol {
    lba_base:            u64,  // absolute LBA of the exFAT boot sector
    sectors_per_cluster: u64,
    fat_lba:             u64,  // absolute LBA of the FAT
    cluster_heap_lba:    u64,  // absolute LBA of the cluster heap
    root_cluster:        u32,
}
```

This is returned by `open_exfat` and passed to every subsequent call.  The
shell calls `open_exfat` fresh on each command (stateless).

---

## Public API

```rust
/// Auto-detect layout and open the exFAT volume.
pub async fn open_exfat(inbox: &BlkInbox) -> Result<ExfatVol, ExfatError>;

/// List directory at `path` (e.g. "/" or "/docs").
pub async fn list_dir(vol: &ExfatVol, inbox: &BlkInbox, path: &str)
    -> Result<Vec<DirEntry>, ExfatError>;

/// Read a file into memory.  Capped at 16 KiB.
pub async fn read_file(vol: &ExfatVol, inbox: &BlkInbox, path: &str)
    -> Result<Vec<u8>, ExfatError>;
```

```rust
pub struct DirEntry {
    pub name:   String,
    pub is_dir: bool,
    pub size:   u64,
}

pub enum ExfatError {
    NoDevice, IoError, NotExfat, UnknownPartitionLayout,
    PathNotFound, NotAFile, NotADirectory, FileTooLarge,
}
```

`BlkInbox` is a type alias for the virtio-blk actor's mailbox:

```rust
pub type BlkInbox = Arc<Mailbox<ActorMsg<VirtioBlkMsg, VirtioBlkInfo>>>;
```

---

## Path Resolution

The shell maintains a current working directory (CWD) in `Shell::cwd`
(`spin::Mutex<String>`, default `"/"`).

`resolve_path(cwd, path)` in `kernel/src/shell.rs` handles relative and
absolute paths, then `normalize_path` collapses `.` and `..` components:

```
cwd = "/a/b"
resolve("../c")  →  normalize("/a/b/../c")  →  "/a/c"
resolve("/foo")  →  "/foo"
resolve("")      →  "/a/b"   (defaults to CWD)
```

Path component matching in the driver is **case-insensitive ASCII**
(`str::eq_ignore_ascii_case`).  Non-ASCII filename characters are replaced
with `?` in the decoded string.

---

## Shell Commands

| Command | Description |
|---------|-------------|
| `ls [path]` | List directory; defaults to CWD |
| `cat <path>` | Print file as text; non-printable bytes shown as `.` |
| `pwd` | Print current working directory |
| `cd [path]` | Change CWD; verifies the target exists; defaults to `/` |
| `blk ls [path]` | Alias for `ls` |
| `blk cat <path>` | Alias for `cat` |

`cd` calls `list_dir` on the target path before updating the CWD, so invalid
paths are rejected with an error rather than silently accepted.

---

## Memory Budget

Peak heap usage during `ls`:

| Item | Size |
|------|------|
| Boot sector | 512 B |
| FAT sector (per entry lookup) | 512 B |
| Cluster data (typical 4 KiB cluster) | 4 KiB |
| `Vec<DirEntry>` | small |
| **Total** | **~5 KiB** |

`read_file` caps output at **16 KiB**.  The kernel heap is 100 KiB; both
operations are well within budget.

---

## Limitations

### Read-only
Write support is not implemented.  `VirtioBlkMsg::Write` exists in the block
driver but the exFAT layer has no write path.

### Entry sets crossing cluster boundaries
`scan_dir_cluster` collects all sectors of a cluster into a flat buffer before
parsing entries.  An entry set whose 0x85 primary entry is in one cluster and
whose secondary entries start in the next cluster will be silently skipped.
This situation does not arise on normally-formatted volumes where directories
start empty.

### ASCII-only filenames
UTF-16LE code points above U+007F are replaced with `?`.  Files can still be
opened by name if the shell command uses the same replacement — but in
practice, test images should use ASCII filenames.

### Fresh volume open per command
`open_exfat` reads the boot sector (and up to ~32 GPT entry sectors) on every
shell command.  A cached `ExfatVol` stored in the shell actor would reduce
overhead, but is unnecessary given the current workload.

### 16 KiB file cap
`read_file` returns `ExfatError::FileTooLarge` for files exceeding 16 KiB.
The limit exists to protect the 100 KiB heap; it can be raised if the heap is
grown.

---

## Key Files

| File | Role |
|------|------|
| `devices/src/virtio/exfat.rs` | Partition detection, boot parse, FAT traversal, dir scan, path walk, public API |
| `devices/src/virtio/mod.rs` | Re-exports `BlkInbox`, `DirEntry`, `ExfatError`, `ExfatVol`, public functions |
| `kernel/src/shell.rs` | `cmd_blk_ls`, `cmd_blk_cat`, `cmd_cd`, `cmd_pwd`, `resolve_path`, `normalize_path` |

---

## Creating Test Images

### GPT (macOS default)

```sh
hdiutil create -size 32m -fs ExFAT -volname TEST test-gpt.dmg
hdiutil attach test-gpt.dmg
cp hello.txt /Volumes/TEST/
mkdir /Volumes/TEST/subdir
cp nested.txt /Volumes/TEST/subdir/
hdiutil detach /Volumes/TEST
hdiutil convert test-gpt.dmg -format UDRO -o test-gpt-ro.dmg
```

### MBR-partitioned

```sh
diskutil eraseDisk ExFAT TEST MBRFormat /dev/diskN
```

### Bare exFAT (no partition table)

```sh
diskutil eraseVolume ExFAT TEST /dev/diskN
```

### Running in QEMU

```sh
qemu-system-x86_64 ... \
  -drive file=test-gpt.img,format=raw,if=none,id=hd0 \
  -device virtio-blk-pci,drive=hd0
```

Then in the shell:

```
ostoo:/> ls
  [DIR]        subdir
  [FILE    13] hello.txt
ostoo:/> cat /hello.txt
Hello, kernel!
ostoo:/> cd subdir
ostoo:/subdir> ls
  [FILE    11] nested.txt
ostoo:/subdir> cat nested.txt
Hello again!
ostoo:/subdir> cd /
ostoo:/> pwd
/
```
