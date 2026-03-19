# getcwd (nr 79)

## Linux Signature

```c
char *getcwd(char *buf, size_t size);
```

## Description

Copies the absolute pathname of the current working directory into `buf`. On success, returns `buf`. On failure, returns `-1` and sets `errno`.

## Current Implementation

1. Validates that `buf` is within user address space. Returns `-EFAULT` (-14) if not.
2. Reads the `cwd` field from the current process's `Process` struct.
3. Checks that `size` is large enough to hold the cwd string plus a null terminator. Returns `-ERANGE` (-34) if too small.
4. Copies the cwd string and null terminator into the user buffer.
5. Returns `buf` (the pointer value) on success — matching Linux's behaviour where the return value is the buffer address.

Each process has its own `cwd` field (default `"/"`), updated by `chdir`.

**Source:** `osl/src/dispatch.rs` — `sys_getcwd`

## Usage from C (musl)

```c
#include <unistd.h>

char buf[256];
if (getcwd(buf, sizeof(buf)) != NULL) {
    /* buf contains the current working directory */
}
```

Or via raw syscall:

```c
#include <sys/syscall.h>

char buf[256];
long ret = syscall(SYS_getcwd, buf, sizeof(buf));
/* ret > 0 on success (pointer to buf) */
```

## Future Work

- Support `getcwd(NULL, 0)` which auto-allocates a buffer (musl handles this in userspace).
