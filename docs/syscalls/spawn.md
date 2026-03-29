# spawn (nr 500) — custom syscall

## Signature

```c
long spawn(const char *path, size_t path_len,
           const char **argv, size_t argc,
           const char **envp, size_t envc);
```

**Note:** This is a custom ostoo syscall (number 500), not a standard Linux syscall. It combines the functionality of `fork` + `execve` into a single call, since ostoo does not implement `fork`.

The `envp` and `envc` parameters (arguments 5 and 6) are optional. If `envp` is NULL (0), the child receives an empty environment. This is backwards compatible with callers that only pass 4 arguments.

## Description

Spawns a new process from an ELF binary on the VFS. The child process inherits no state from the parent — it gets a fresh address space, file descriptor table, and stack. The child becomes the console foreground process.

## Current Implementation

1. **Read path:** Reads `path_len` bytes from `path` pointer. Validates the buffer is in user space. Resolves relative to the process's `cwd`.
2. **Read argv:** If `argc > 0`, reads `argc` pointers from `argv` array, then reads each null-terminated string. Each string becomes an element of the child's `argv`.
3. **Read envp:** If `envp != 0`, reads `envc` pointers from `envp` array (6th argument via r9), then reads each null-terminated `KEY=VALUE` string. If `envp` is 0, the child gets an empty environment.
4. **Load ELF:** Calls `devices::vfs::read_file()` (through `osl::blocking::blocking()`) to load the ELF binary from the VFS. This blocks the calling thread.
5. **Spawn process:** Calls `osl::spawn::spawn_process_full(elf_data, argv, envp, parent_pid)` directly:
   - Parses the ELF binary.
   - Creates a new user PML4 page table.
   - Maps PT_LOAD segments into the new address space.
   - Allocates an 8-page (32 KiB) user stack at `0x0000_7FFF_F000_0000`.
   - Builds the initial stack with `argc`, `argv` pointers, `envp` pointers, and auxiliary vector (`AT_PHDR`, `AT_PHENT`, `AT_PHNUM`, `AT_PAGESZ`, `AT_ENTRY`, `AT_UID`, `AT_RANDOM`).
   - Creates a `Process` with `parent_pid` set to the caller.
   - Spawns a scheduler thread targeting `process_trampoline`.
6. **Set foreground:** Calls `console::set_foreground(child_pid)` so keyboard input goes to the child.
7. **Returns** the child's PID on success.

**Source:** `osl/src/dispatch.rs` — `sys_spawn`, `osl/src/spawn.rs` — `spawn_process_full`

## Usage from C (musl)

```c
#include <sys/syscall.h>
#include <string.h>

#define SYS_SPAWN 500

/* Spawn /bin/hello with argv and envp */
const char *path = "/bin/hello";
const char *argv[] = { "/bin/hello", "arg1", "arg2" };
const char *envp[] = { "PATH=/host/bin", "HOME=/" };
long pid = syscall(SYS_SPAWN, path, strlen(path), argv, 3, envp, 2);
if (pid < 0) {
    /* error: file not found or invalid ELF */
}

/* Then wait for the child */
int status;
syscall(SYS_wait4, pid, &status, 0, 0);
```

## Errors

| Errno | Condition |
|-------|-----------|
| `-EFAULT` (-14) | Invalid path or argv pointer |
| `-EINVAL` (-22) | Path is not valid UTF-8 |
| `-ENOENT` (-2) | File not found on VFS |
| `-ENOENT` (-2) | ELF load or spawn failed |

## Alternatives

Standard Linux process creation is now available via `clone(CLONE_VM|CLONE_VFORK)` + `execve` (see [clone.md](clone.md) and [execve.md](execve.md)). musl's `posix_spawn` and Rust's `std::process::Command` use these standard syscalls and do not require this custom syscall. The userspace shell has been updated to use `posix_spawn` instead of `SYS_SPAWN`.

## Future Work

- Inherit or selectively copy file descriptors (e.g. stdin/stdout redirection for pipes).
- May be deprecated in favour of the standard `clone` + `execve` path.
