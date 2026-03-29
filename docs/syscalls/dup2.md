# dup2 (nr 33)

## Linux Signature

```c
int dup2(int oldfd, int newfd);
```

## Description

Duplicates file descriptor `oldfd` to `newfd`. If `newfd` is already open, it is silently closed first.

## Current Implementation

1. If `oldfd == newfd`: validates that `oldfd` is open, returns `newfd`.
2. Reads the `FdEntry` (handle + flags) from `oldfd`.
3. Clones the `Arc<dyn FileHandle>` and installs it at `newfd`.
4. The new fd does **not** inherit `FD_CLOEXEC` from the old fd (per POSIX).
5. If `newfd` was previously open, its old handle is dropped (refcount decremented).
6. The fd table is extended if `newfd` exceeds the current length.

**Source:** `osl/src/syscalls/fs.rs` — `sys_dup2`

## Errors

| Errno | Condition |
|-------|-----------|
| `-EBADF` (-9) | `oldfd` is not a valid open fd |
