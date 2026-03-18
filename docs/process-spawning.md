# Process Spawning

How user-space processes are created today and the options for supporting
process creation from user space.

---

## Current state

Process creation is entirely kernel-side.  The shell actor (ring 0) reads
an ELF from the VFS and calls `ring3::spawn_process(&data)` directly.
There is no syscall for launching a process.

`spawn_process` does:

1. Reap zombies (free kernel stacks of exited processes)
2. Parse the ELF (`ET_EXEC`, 64-bit, little-endian, x86_64)
3. Create a fresh PML4 with kernel entries copied (indices 256–510)
4. Map each `PT_LOAD` segment: allocate frames, zero, copy file data,
   set PRESENT + USER_ACCESSIBLE (+ WRITABLE / NO_EXECUTE per flags)
5. Map a 4 KiB user stack at `0x0050_0000` (RW + NX + USER)
6. Create `Process` (allocates PID, 64 KiB kernel stack, caches stack top)
7. Insert into global `PROCESS_TABLE`
8. `scheduler::spawn_user_thread(pid, pml4_phys)` — creates a kernel thread
   whose `SwitchFrame` enters `process_trampoline`
9. Trampoline sets TSS.rsp0, per-CPU RSP, GS MSRs, switches CR3, iretqs
   to ring 3 at the ELF entry point

There is no `fork`, `clone`, `exec`, `waitpid`, fd inheritance, argv/envp
passing, or address-space duplication.

---

## Options

### Option A: `spawn` syscall (kernel-side creation)

Expose the existing `spawn_process` logic as a syscall:

```
spawn(path_ptr, path_len, fd_actions_ptr, fd_actions_len) → pid
```

The kernel reads the ELF from the VFS, builds the address space, and
starts the thread.  The caller provides an array of `FdAction` entries
describing how to set up the child's file descriptors:

```rust
#[repr(C)]
enum FdAction {
    /// Child inherits parent's fd N as fd N.
    Inherit(i32),
    /// Child gets a copy of parent's `parent_fd` as `child_fd`.
    Dup { parent_fd: i32, child_fd: i32 },
    /// Close this fd in the child.
    Close(i32),
}
```

Shell pipelines become:

```
let (r, w) = pipe();
spawn("prog1", &[Dup(w, 1), Close(r)]);   // stdout → pipe write end
spawn("prog2", &[Dup(r, 0), Close(w)]);   // stdin  → pipe read end
close(r); close(w);
waitpid(-1, ...);
```

**Pros:**

- Simple — 90% of the code already exists in `spawn_process`
- No CoW page fault handler needed
- No temporary doubled memory from fork
- Good fit for a 512 KiB kernel heap
- Modern precedent: `posix_spawn`, Windows `CreateProcess`, Fuchsia
  `zx_process_create`, Plan 9 `rfork`+`exec`

**Cons:**

- Can't run unmodified Unix programs that assume `fork()+exec()`
- musl's `posix_spawn` internally calls `clone`+`exec` on Linux, so
  a custom shim or C library patch would be needed
- Shell job control patterns (fork, set up fds, exec) need a different
  idiom

---

### Option B: `fork` + `exec` (Unix-compatible)

The classic model.  `fork` duplicates the address space (with CoW),
`exec` replaces it with a new program.

**Pros:**

- Drop-in Linux ABI compatibility — musl binaries work unmodified
- Shell pipelines work naturally (fork, dup2, exec)
- Phase 6 of the userspace plan already targets this

**Cons:**

- CoW page fault handler is significant work: per-frame reference
  counts, copy-on-write in the page fault ISR, shared PML4/PDPT/PD/PT
  management
- `fork` is widely considered a design mistake (see "A `fork()` in the
  Road", HotOS 2019) — expensive, error-prone, poor interaction with
  threads
- Wastes kernel resources temporarily (duplicated page tables even
  with CoW)

---

### Option C: `clone(CLONE_VFORK)` + `exec` (skip CoW)

A minimal `clone` with `CLONE_VFORK` semantics: the child shares the
parent's address space, and the parent blocks until the child calls
`exec` or `exit`.

1. `clone(CLONE_VFORK)` — child shares parent's address space, parent
   blocked
2. Child calls `exec(path)` — gets its own address space, parent
   unblocked
3. No page table duplication or CoW needed

**Pros:**

- Good stepping stone toward full fork
- Avoids CoW complexity entirely
- Closer to Linux ABI than a custom `spawn`

**Cons:**

- Still need `exec` syscall (replacing address space of a running
  process at runtime is more complex than building one from scratch)
- Shared address space between clone and exec is fragile — child must
  not corrupt parent's stack
- More complex than `spawn` for less benefit unless Linux ABI compat
  is a goal

---

### Option D: Hybrid — `spawn` now, `fork` later  ← recommended

Do it in two phases:

**Phase 1 — `spawn` syscall (practical, minimal)**

Formalize `spawn_process` as a syscall with fd actions.  Add `waitpid`
for parent-child synchronisation.  This is enough for shell pipelines,
ELF execution from user space, and basic job control.

**Phase 2 — `fork`/`exec` (later, for musl compat)**

When unmodified musl binaries are a goal, add:

- CoW page fault handler with per-frame refcounts
- `fork` (or `clone`) — duplicate address space
- `execve` — replace current process's address space
- `waitpid` already exists from Phase 1

This matches the userspace plan (Phase 6) and avoids front-loading the
CoW complexity.

---

## Required pieces (regardless of model)

| Piece                | Why                                      | Status           |
|----------------------|------------------------------------------|------------------|
| `spawn` or `exec`   | Launch processes from user space         | Kernel-side only |
| `waitpid`           | Parent waits for child exit              | Not started      |
| fd inheritance      | Child gets parent's fds (or subset)      | Designed (file-descriptors.md) |
| `Blocked` state     | waitpid, pipe, sleep                     | Designed, not implemented |
| `brk` or `mmap`     | Any real C program needs heap            | Not started      |
| argv/envp passing   | Programs need command-line args          | Not started      |
| Multiple stack pages | 4 KiB user stack is too small for real programs | Not started |

---

## `waitpid` design sketch

```
waitpid(pid, status_ptr, flags) → pid
```

Uses the `Blocked` thread state from the file-descriptors design:

1. Caller specifies `pid` (specific child) or `-1` (any child).
2. Kernel checks if any matching child is already a zombie:
   - Yes → reap it, write exit status, return pid.
   - No  → if `WNOHANG` flag set, return 0.
   - No  → block the calling thread.
3. When a child exits (`sys_exit`), check if the parent is blocked on
   `waitpid` and `unblock` it.
4. The parent wakes, finds the zombie, reaps it, returns.

Storage: add `parent_pid: ProcessId` to `Process` so the kernel knows
which process to wake.

---

## argv/envp passing

User programs need `argc`, `argv`, and `envp` on the stack (or in
registers) at entry.  Two approaches:

**Stack-based (Linux convention):**

Map an extra page at the top of the user stack.  Write the argument
strings, then the `argv[]` pointer array, then `argc`, following the
x86_64 SysV ABI initial process stack layout:

```
high address
  ┌─────────────┐
  │ envp strings │
  │ argv strings │
  │ padding      │
  │ NULL         │  ← end of envp[]
  │ envp[0]      │
  │ NULL         │  ← end of argv[]
  │ argv[1]      │
  │ argv[0]      │
  │ argc         │  ← RSP at entry
  └─────────────┘
low address
```

The `spawn` syscall would accept `argv_ptr`/`argv_len` and
`envp_ptr`/`envp_len` from the parent, validate and copy strings into
the child's stack page, and set the child's initial RSP to point at
`argc`.

**Dedicated args page:** Map a separate read-only page at a fixed
address (e.g. `0x0060_0000`) containing the serialized args.  Simpler
to implement but non-standard.

The stack-based approach is recommended — it's what Linux does and
what musl / any libc expects.

---

## Implementation order

| Phase | What                                                    | Depends on          |
|-------|---------------------------------------------------------|---------------------|
| 1     | fd table + `FileHandle` + `ConsoleHandle`               | —                   |
| 2     | `Blocked` thread state + `block`/`unblock`              | —                   |
| 3     | `sys_read` / `sys_write` / `sys_close` via fd table     | Phase 1             |
| 4     | Pipe (`PipeReader` / `PipeWriter`)                      | Phases 1–3          |
| 5     | `spawn` syscall with fd actions                         | Phases 1, 3         |
| 6     | `waitpid` syscall                                       | Phase 2             |
| 7     | argv/envp stack setup                                   | Phase 5             |
| 8     | `brk` or `mmap` (user heap)                             | —                   |
| 9     | Shell pipelines from user space                         | Phases 4–7          |
| 10    | `fork`/`exec` (CoW, Linux ABI compat)                   | Phase 8, much later |

Phases 1–4 are covered by the file-descriptors design doc.  Phases 5–7
form the core of user-space process execution.  Phase 10 is optional
and can be deferred indefinitely if Linux ABI compat is not a priority.
