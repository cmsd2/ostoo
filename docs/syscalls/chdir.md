# chdir (nr 80)

## Linux Signature

```c
int chdir(const char *path);
```

## Description

Changes the current working directory to `path`.

## Current Implementation

1. Reads a null-terminated path string from user space (max 4096 bytes). Returns `-EFAULT` (-14) if the pointer is invalid.
2. Resolves the path relative to the process's current `cwd`. Normalises `.` and `..` components.
3. Validates that the resolved path is an existing directory by calling `devices::vfs::list_dir()` (through `osl::blocking::blocking()`). This blocks the calling thread while the async VFS operation completes.
4. On success, updates the process's `cwd` field to the resolved path and returns 0.
5. On failure, returns the error from the VFS (typically `-ENOENT` or `-ENOTDIR`).

**Source:** `osl/src/dispatch.rs` — `sys_chdir`

## Usage from C (musl)

```c
#include <unistd.h>

if (chdir("/some/path") < 0) {
    /* error */
}
```

## Errors

| Errno | Condition |
|-------|-----------|
| `-EFAULT` (-14) | Invalid path pointer |
| `-ENOENT` (-2) | Path does not exist |
| `-ENOTDIR` (-20) | A component of the path is not a directory |
| `-EIO` (-5) | VFS I/O error |

## Future Work

- Support `fchdir(fd)` to change directory via an open directory fd.
