# write (nr 1)

## Linux Signature

```c
ssize_t write(int fd, const void *buf, size_t count);
```

## Description

Writes up to `count` bytes from `buf` to file descriptor `fd`.

## Current Implementation

Looks up `fd` in the current process's per-process file descriptor table and calls `FileHandle::write()` on the handle.

- **fd 1 (stdout) and fd 2 (stderr) — `ConsoleHandle`:** Interprets `buf` as UTF-8 and prints to the VGA text buffer via `print!()`. If not valid UTF-8, falls back to printing printable ASCII (0x20..0x7F) plus `\n`, `\r`, `\t`. Returns `count` on success.
- **VFS file fds — `VfsHandle`:** Returns `-EBADF` (-9) — files are read-only.
- **Invalid fds:** Returns `-EBADF` (-9).

Validates that `buf` falls within user address space (`< 0x0000_8000_0000_0000`). Returns `-EFAULT` (-14) on invalid pointers.

**Source:** `osl/src/dispatch.rs` — `sys_write`

## Future Work

- Support writable VFS files.
- Handle partial writes.
