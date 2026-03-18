# fstat (nr 5)

## Linux Signature

```c
int fstat(int fd, struct stat *statbuf);
```

## Description

Returns information about a file referred to by the file descriptor `fd`, writing it into the `stat` structure at `statbuf`.

## Current Implementation

- Zero-fills the 144-byte `struct stat` buffer.
- Sets `st_mode` at offset 24 to `S_IFCHR | 0666` (character device, read/write for all), regardless of which fd is queried.
- Always returns 0 (success).

This is sufficient for musl's stdio initialisation, which calls `fstat` on stdout to determine whether it is a terminal.

**Source:** `libkernel/src/syscall.rs` — `sys_fstat`

## Future Work

- Return different `st_mode` values depending on the fd (e.g., regular file vs. character device).
- Populate other stat fields (`st_size`, `st_ino`, `st_dev`, timestamps, etc.).
- Return `-EBADF` for invalid file descriptors.
- Validate that `statbuf` is a writable user-space address.
