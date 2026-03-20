# clone (nr 56)

## Linux Signature

```c
long clone(unsigned long flags, void *child_stack, int *ptid, int *ctid, unsigned long tls);
```

## Description

Creates a new process. ostoo supports the specific flag combination used by musl's `posix_spawn`: `CLONE_VM | CLONE_VFORK | SIGCHLD` (0x4111). The child shares the parent's address space and the parent blocks until the child calls `execve` or `_exit`.

## Current Implementation

Only the flag combination `CLONE_VM | CLONE_VFORK | SIGCHLD` is accepted. Other flag combinations return `-ENOSYS`.

1. **Validate arguments:** `child_stack` must be non-zero. Unsupported flags return `-ENOSYS`.
2. **Read parent state:** Copies `pml4_phys`, `cwd`, `fd_table` (Arc clones), `brk_*`, `mmap_*` from the parent process.
3. **Capture user registers:** Reads `user_rip`, `user_rflags`, and `user_r9` from `PerCpuData` (saved by the SYSCALL entry stub). These are needed so the child can "return from syscall" at the same instruction as the parent.
4. **Create child process:** New PID, same `pml4_phys` as parent (CLONE_VM), inherited fd table and cwd. Sets `vfork_parent_thread` to the parent's scheduler thread index.
5. **Spawn clone thread:** Creates a scheduler thread via `spawn_clone_thread` that enters `clone_trampoline`. The trampoline sets up kernel state and drops to ring 3 at `user_rip` with `RAX=0` (child return value) and `R9=user_r9` (musl's `__clone` fn pointer).
6. **Block parent:** Calls `block_current_thread()` (CLONE_VFORK semantics). The parent is unblocked when the child calls `execve` or `_exit`.
7. **Return:** After unblocking, returns the child's PID to the parent.

**Source:** `osl/src/clone.rs` — `sys_clone`, `libkernel/src/task/scheduler.rs` — `spawn_clone_thread`, `clone_trampoline`

## Usage from C (musl)

Not called directly — musl's `posix_spawn` uses it internally:

```c
#include <spawn.h>
#include <sys/wait.h>

pid_t child;
int err = posix_spawn(&child, "/hello", NULL, NULL, argv, envp);
if (err == 0) {
    int status;
    waitpid(child, &status, 0);
}
```

## Errors

| Errno | Condition |
|-------|-----------|
| `-ENOSYS` (-38) | Unsupported flag combination |
| `-EINVAL` (-22) | `child_stack` is NULL |

## Design Notes

- musl's `__clone` assembly stores the child function pointer in R9 before `syscall`. The entry stub saves R9 to `PerCpuData.user_r9` (offset 32), and `clone_trampoline` restores it via `jump_to_userspace(rax=0, r9=user_r9)`.
- The child shares the parent's PML4 (CLONE_VM). After `execve`, the child gets a fresh PML4. The old shared PML4 continues to be used by the parent.
