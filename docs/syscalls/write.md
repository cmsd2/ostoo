# write (nr 1)

## Linux Signature

```c
ssize_t write(int fd, const void *buf, size_t count);
```

## Description

Writes up to `count` bytes from `buf` to file descriptor `fd`.

## Current Implementation

- **fd 1 (stdout) and fd 2 (stderr):** Interprets `buf` as a UTF-8 string and prints it to the VGA text buffer via `print!()`. Non-UTF-8 data is silently ignored.
- **All other fds:** Returns `-EBADF` (-9).
- Always returns `count` on success (assumes all bytes were written).

**Source:** `libkernel/src/syscall.rs` — `sys_write`

## Future Work

- Handle non-UTF-8 data gracefully (fallback to printable ASCII, matching `writev` behaviour).
- Implement a file descriptor table so writes can target VFS files.
- Validate that `buf` points to readable user-space memory (SMAP enforcement).
- Support partial writes and proper error handling.
