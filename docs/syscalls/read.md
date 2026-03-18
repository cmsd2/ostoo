# read (nr 0)

## Linux Signature

```c
ssize_t read(int fd, void *buf, size_t count);
```

## Description

Reads up to `count` bytes from file descriptor `fd` into `buf`.

## Current Implementation

- **fd 0 (stdin):** Always returns 0 (EOF). No keyboard/serial input is wired up.
- **All other fds:** Returns `-EBADF` (-9).

The `buf` and `count` arguments are currently ignored for fd 0.

**Source:** `libkernel/src/syscall.rs` — `sys_read`

## Future Work

- Wire fd 0 to the serial/keyboard input buffer so user processes can read stdin.
- Implement a proper file descriptor table per process, supporting reads from VFS-backed files.
- Validate that `buf` is a valid, writable user-space address (SMAP enforcement).
