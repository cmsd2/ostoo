# read (nr 0)

## Linux Signature

```c
ssize_t read(int fd, void *buf, size_t count);
```

## Description

Reads up to `count` bytes from file descriptor `fd` into `buf`.

## Current Implementation

Looks up `fd` in the current process's per-process file descriptor table and calls `FileHandle::read()` on the handle.

- **fd 0 (stdin) — `ConsoleHandle`:** Reads raw bytes from the console input buffer (`libkernel/src/console.rs`). If the buffer is empty, blocks the current scheduler thread via `block_current_thread()` until the keyboard ISR delivers input via `push_input()`. Returns at least 1 byte per call.
- **VFS file fds — `VfsHandle`:** Reads from an in-memory buffer loaded at `open()` time. Maintains a per-handle read position. Returns 0 at EOF.
- **Directory fds — `DirHandle`:** Returns `-EISDIR` (-21).
- **Invalid fds:** Returns `-EBADF` (-9).

Validates that `buf` falls within user address space (`< 0x0000_8000_0000_0000`). Returns `-EFAULT` (-14) on invalid pointers. Returns 0 immediately if `count` is 0.

**Source:** `osl/src/dispatch.rs` — `sys_read`

## Future Work

- Support partial reads and proper error handling for VFS files.
- SMAP enforcement for user buffer validation.
