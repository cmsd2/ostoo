# lseek (nr 8)

## Linux Signature

```c
off_t lseek(int fd, off_t offset, int whence);
```

## Description

Repositions the file offset of the open file descriptor `fd` to the given `offset`
according to `whence` (`SEEK_SET`, `SEEK_CUR`, `SEEK_END`).

## Current Implementation

Always returns `-ESPIPE` (illegal seek). The only file descriptors currently in use
are stdin/stdout/stderr, which behave as non-seekable character devices (serial console).

**Source:** `osl/src/dispatch.rs` — inline in `syscall_dispatch`

## Future Work

- Implement proper seek for regular file descriptors once the VFS exposes them to
  user-space via `open`.
