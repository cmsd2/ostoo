# ioctl (nr 16)

## Linux Signature

```c
int ioctl(int fd, unsigned long request, ...);
```

## Description

Manipulates the underlying device parameters of special files. Commonly used to query terminal attributes (`TCGETS`, `TIOCGWINSZ`, etc.).

## Current Implementation

Always returns `-ENOTTY` (-25), indicating the file descriptor does not refer to a terminal. All arguments are ignored.

This is sufficient for musl's stdio, which calls `ioctl(fd, TIOCGWINSZ, ...)` to check if stdout is a terminal for line buffering decisions. Receiving `-ENOTTY` causes musl to treat the fd as a non-terminal and use full buffering.

**Source:** `osl/src/dispatch.rs` — inline in `syscall_dispatch`

## Future Work

- Return `TIOCGWINSZ` data for the VGA console (80x25) so musl recognises it as a terminal.
- Implement `TCGETS`/`TCSETS` for basic terminal attribute support.
- Dispatch based on fd to different device drivers.
