# open (nr 2)

## Linux Signature

```c
int open(const char *pathname, int flags, mode_t mode);
```

## Description

Opens a file or directory at `pathname` and returns a file descriptor.

## Current Implementation

1. Reads a null-terminated path string from user space (max 4096 bytes). Returns `-EFAULT` if the pointer is invalid.
2. Resolves the path relative to the process's current working directory (`cwd`). Normalises `.` and `..` components.
3. Unless `O_DIRECTORY` (0o200000) is set, first attempts to open as a file via `devices::vfs::read_file()` (through `osl::blocking::blocking()`). On success, the entire file content is loaded into a `VfsHandle` (buffered in kernel memory) and a new fd is allocated.
4. If the file open fails with `VfsError::NotFound` or `VfsError::NotAFile`, or `O_DIRECTORY` was requested, falls back to opening as a directory via `devices::vfs::list_dir()`. On success, creates a `DirHandle` with the directory listing and allocates a new fd.
5. Returns the new fd number on success, or a negative errno.

The VFS operations use `osl::blocking::blocking()` which spawns the async VFS call as a kernel task and blocks the calling user thread until it completes.

**Flags supported:** `O_DIRECTORY` (to explicitly request directory). `O_RDONLY` is implied for all opens. Other flags are accepted but ignored.

**Source:** `osl/src/syscalls/fs.rs` — `sys_open`

## Errors

| Errno | Condition |
|-------|-----------|
| `-EFAULT` (-14) | Invalid pathname pointer |
| `-ENOENT` (-2) | File or directory not found |
| `-ENOTDIR` (-20) | Path is not a directory (when `O_DIRECTORY` used) |
| `-EMFILE` (-24) | Per-process fd limit reached (64) |
| `-EIO` (-5) | VFS I/O error |

## Future Work

- Support `O_WRONLY`, `O_CREAT`, `O_TRUNC` for writable files.
- Streaming reads instead of loading entire file into memory at open time.
- Proper `mode` handling.
