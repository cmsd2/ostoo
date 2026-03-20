# fcntl (nr 72)

## Linux Signature

```c
int fcntl(int fd, int cmd, ... /* arg */);
```

## Description

Performs operations on file descriptors. Only fd-level flag operations are supported.

## Current Implementation

| Command | Value | Behaviour |
|---------|-------|-----------|
| `F_GETFD` | 1 | Returns the fd flags (currently only `FD_CLOEXEC`) |
| `F_SETFD` | 2 | Sets the fd flags to `arg` |
| `F_GETFL` | 3 | Returns 0 (no file status flags tracked) |
| Other | — | Returns `-EINVAL` |

**Source:** `osl/src/dispatch.rs` — `sys_fcntl`

## Errors

| Errno | Condition |
|-------|-----------|
| `-EBADF` (-9) | `fd` is not a valid open fd |
| `-EINVAL` (-22) | Unknown `cmd` |
