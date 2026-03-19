# writev (nr 20)

## Linux Signature

```c
ssize_t writev(int fd, const struct iovec *iov, int iovcnt);
```

Where `struct iovec` is:

```c
struct iovec {
    void  *iov_base;  // Starting address
    size_t iov_len;   // Number of bytes
};
```

## Description

Writes data from multiple buffers (a "scatter/gather" array) to a file descriptor in a single atomic operation. This is what musl's `printf` uses internally instead of plain `write`.

## Current Implementation

Looks up `fd` in the current process's per-process file descriptor table. Iterates through `iovcnt` iovec entries (each 16 bytes: `iov_base: u64, iov_len: u64`). For each non-empty buffer, calls `FileHandle::write()` on the handle.

- **Console fds (stdout/stderr):** Each buffer is printed via `ConsoleHandle::write()` (UTF-8 with ASCII fallback).
- **Invalid fds:** Returns `-EBADF` (-9).
- Returns the total number of bytes written across all iovec entries on success.
- Short-circuits on error from any individual write.

**Source:** `osl/src/dispatch.rs` — `sys_writev`

## Future Work

- Validate that `iov` and all `iov_base` pointers are valid user-space addresses.
- Handle partial writes.
- Cap `iovcnt` at `UIO_MAXIOV` (1024) per Linux convention.
