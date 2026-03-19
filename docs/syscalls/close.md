# close (nr 3)

## Linux Signature

```c
int close(int fd);
```

## Description

Closes a file descriptor so that it no longer refers to any file and may be reused.

## Current Implementation

Looks up `fd` in the current process's file descriptor table. If found, calls `FileHandle::close()` on the handle and sets the table slot to `None`, making the fd number available for reuse.

- Returns 0 on success.
- Returns `-EBADF` (-9) if the fd is not open or out of range.

**Source:** `osl/src/dispatch.rs` — `sys_close`

## Future Work

- Flush pending writes for writable file handles before closing.
- Free resources held by the handle (e.g. release VFS locks).
