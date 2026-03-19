# wait4 (nr 61)

## Linux Signature

```c
pid_t wait4(pid_t pid, int *wstatus, int options, struct rusage *rusage);
```

## Description

Waits for a child process to change state (typically exit). Returns the PID of the child whose state changed, and optionally writes the exit status.

## Current Implementation

Called as syscall number 61 (`wait4`). The `rusage` parameter is ignored.

1. Determines the calling process's PID (`parent_pid`).
2. Interprets `pid` argument:
   - `-1`: Wait for any child process.
   - `> 0`: Wait for the specific child with that PID.
3. Searches the process table for a zombie child matching the criteria via `find_zombie_child(parent_pid, target_pid)`.
4. **If a zombie child is found:**
   - Writes the exit status to the user-space `wstatus` pointer (if non-NULL), encoded as `(exit_code << 8)` matching Linux's `WEXITSTATUS` macro.
   - Reaps the child process (removes from process table, frees kernel stack).
   - Restores the console foreground to the parent process.
   - Returns the child's PID.
5. **If no zombie child exists but living children do:**
   - Registers the current scheduler thread index in the parent's `wait_thread` field.
   - Calls `block_current_thread()` to sleep.
   - When woken (by a child calling `sys_exit`), loops back to step 3.
6. **If no children exist at all:** Returns `-ECHILD` (-10).

**Source:** `osl/src/dispatch.rs` — `sys_wait4`

## Usage from C (musl)

```c
#include <sys/wait.h>
#include <sys/syscall.h>

/* Wait for specific child */
int status;
pid_t child = syscall(SYS_wait4, child_pid, &status, 0, 0);
int exit_code = WEXITSTATUS(status);  /* (status >> 8) & 0xFF */

/* Wait for any child */
pid_t any = syscall(SYS_wait4, -1, &status, 0, 0);
```

## Errors

| Errno | Condition |
|-------|-----------|
| `-ECHILD` (-10) | Calling process has no children |

## Future Work

- Support `WNOHANG` option (return immediately if no child has exited).
- Support `WUNTRACED` and `WCONTINUED` for stopped/continued children.
- Populate `struct rusage` with resource usage statistics.
- Handle the case where multiple children exit simultaneously.
