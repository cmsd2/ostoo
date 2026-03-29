# Process Spawning

How user-space processes are created.

---

## Current Implementation

Process creation uses the standard Linux `clone(CLONE_VM|CLONE_VFORK)` +
`execve` path.  musl's `posix_spawn` and Rust's `std::process::Command`
work unmodified.

### clone (CLONE_VM | CLONE_VFORK | SIGCHLD)

`clone` creates a child process that shares the parent's address space.
The parent blocks until the child calls `execve` or `_exit`.

See [`syscalls/clone.md`](syscalls/clone.md) for full details.

### execve

`execve` replaces the current process's address space with a new ELF binary.
Reads the ELF from the VFS, creates a fresh PML4, maps segments, builds the
initial stack with `argc/argv/envp/auxv`, closes `FD_CLOEXEC` fds, unblocks
the vfork parent, and jumps to userspace.

See [`syscalls/execve.md`](syscalls/execve.md) for full details.

### Internal spawning (kernel-side)

For boot-time process creation (e.g. auto-launching the shell), the kernel
uses `osl::spawn::spawn_process_full(elf_data, argv, envp, parent_pid)`
which combines ELF loading and process creation in a single call.

`kernel/src/ring3.rs` provides `spawn_process` and `spawn_process_with_env`
wrappers that delegate to `spawn_process_full`.

---

## Process lifecycle

```
parent: clone(CLONE_VM|CLONE_VFORK)
  │
  │  ┌─── child created (shares parent PML4) ───┐
  │  │                                            │
  │  │  execve("/bin/prog", argv, envp)           │
  │  │    → fresh PML4, ELF mapped                │
  │  │    → close CLOEXEC fds                     │
  │  │    → unblock parent                        │
  │  │    → jump to ring 3                        │
  │  │                                            │
  ├──┘  parent unblocked                          │
  │                                               │
  │  waitpid(child, &status, 0)                   │
  │    → blocks until child exits                 │
  │                                               │
  │  child: _exit(code)                           │
  │    → mark zombie, wake parent                 │
  │                                               │
  ▼  parent: waitpid returns, reap zombie
```

---

## Key files

| File | Purpose |
|------|---------|
| `osl/src/clone.rs` | `sys_clone` — vfork child creation |
| `osl/src/exec.rs` | `sys_execve` — replace process image |
| `osl/src/spawn.rs` | `spawn_process_full` — kernel-side ELF spawning |
| `osl/src/elf_loader.rs` | ELF parsing and address space setup |
| `libkernel/src/task/scheduler.rs` | `spawn_clone_thread`, `clone_trampoline` |
| `kernel/src/ring3.rs` | `spawn_process` wrapper for boot-time use |

---

## Future work

- **`fork` + CoW page faults** — full POSIX `fork` with copy-on-write.
  Requires page fault handler and per-frame reference counting.
- **fd inheritance across clone** — currently the child gets a copy of the
  parent's fd table; selective inheritance could be added.
