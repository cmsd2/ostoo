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

- **fd 1 (stdout) and fd 2 (stderr):** Iterates through `iovcnt` iovec entries (each 16 bytes: `iov_base: u64, iov_len: u64`). For each non-empty buffer:
  - Attempts UTF-8 decode; if valid, prints via `print!()`.
  - If not valid UTF-8, falls back to printing only printable ASCII characters (0x20..0x7F), plus `\n`, `\r`, `\t`.
- **All other fds:** Returns `-EBADF` (-9).
- Returns the total number of bytes across all iovec entries on success.

**Source:** `libkernel/src/syscall.rs` — `sys_writev`

## Future Work

- Validate that `iov` and all `iov_base` pointers are valid user-space addresses.
- Support writing to VFS-backed file descriptors.
- Handle partial writes (currently assumes all bytes are always written).
- Cap `iovcnt` at `UIO_MAXIOV` (1024) per Linux convention.
