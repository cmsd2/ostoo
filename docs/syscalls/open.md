# open (nr 2)

## Linux Signature

```c
int open(const char *pathname, int flags, mode_t mode);
```

## Description

Opens a file at `pathname` and returns a file descriptor.

## Current Implementation

Always returns `-ENOENT`. musl's `__init_libc` attempts to open `/dev/null` during
startup; returning this error causes it to skip the open and continue normally.

**Source:** `libkernel/src/syscall.rs` — `sys_open`

## Future Work

- Integrate with the VFS layer to resolve paths and return real file descriptors.
- Implement a per-process file descriptor table.
- Support flags (`O_RDONLY`, `O_WRONLY`, `O_CREAT`, etc.).
