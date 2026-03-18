# close (nr 3)

## Linux Signature

```c
int close(int fd);
```

## Description

Closes a file descriptor so that it no longer refers to any file and may be reused.

## Current Implementation

Always returns 0 (success). No file descriptor table exists, so there is nothing to close.

**Source:** `libkernel/src/syscall.rs` — inline in `syscall_dispatch`

## Future Work

- Implement a per-process file descriptor table and actually free the entry on close.
- Return `-EBADF` for invalid file descriptors.
