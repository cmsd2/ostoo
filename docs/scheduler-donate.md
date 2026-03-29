# Scheduler Donate (Direct-Switch) Infrastructure

## Overview

All blocking IPC in ostoo (pipes, completion ports, wait4, vfork) uses
`unblock(thread_idx)` which pushes the woken thread to the **back** of
the ready queue.  The thread then waits for the scheduler's round-robin
to reach it — up to 10 ms (full quantum).

The scheduler donate mechanism adds a voluntary yield via a dedicated
ISR vector (`int 0x50`) so the waker can switch to the woken thread
immediately, eliminating the up-to-10 ms latency.

## Mechanism

### Yield interrupt (vector 0x50)

`ipc_yield_stub` is an assembly handler identical to the LAPIC timer
stub (`lapic_timer_stub`) but calls `yield_tick` instead of
`preempt_tick`.  It provides a software-triggered context switch from
syscall context.

`yield_tick` differs from `preempt_tick`:
- No `tick()`, no `lapic_eoi()` — not a hardware interrupt
- No quantum decrement — always performs the switch
- Checks `DONATE_TARGET: AtomicUsize` for a direct-switch target

### Public API

| Function | Description |
|----------|-------------|
| `yield_now()` | Trigger `int 0x50` — voluntary preemption |
| `set_donate_target(idx)` | Set direct-switch target for next yield |
| `unblock_yield(idx)` | Unblock + set donate + yield (convenience) |

`unblock_yield` is the high-level primitive for the pattern: unblock a
thread and immediately switch to it.

### Direct-switch flow

1. Waker calls `unblock(target)` — target moves to Ready, pushed to ready queue
2. Waker calls `set_donate_target(target)` — stores target in atomic
3. Waker calls `yield_now()` — triggers `int 0x50`
4. `yield_tick` saves waker's state, sees donate target, switches to target
5. Target resumes from its blocked state immediately
6. Waker is re-queued as Ready and runs later via normal scheduling

If the donate target is no longer Ready (e.g., the timer already
dispatched it), `yield_tick` falls back to regular round-robin.

## Applied to existing primitives

### Pipes (`libkernel/src/file.rs`)

`pipe_wake_reader()` returns the woken thread index.  `PipeWriter::write()`
drops the pipe lock, then calls `set_donate_target` + `yield_now()` if a
reader was woken.

`PipeWriter::close()` does NOT yield — it runs inside `with_process()`
which holds the process table lock.  `PipeInner` tracks `writer_count`
(incremented by `on_dup()`, decremented by `close()`).  `write_closed`
is set only when `writer_count` reaches 0, matching Unix pipe semantics
where EOF is delivered only after all writer fds are closed.

`FdObject::clone()` does NOT call `on_dup()` — it is a plain Arc clone.
`on_dup()` is only called via `FdObject::notify_dup()` at actual
fd-duplication sites (clone/fork fd_table inheritance, dup2).

The pipe lock must be dropped before yielding — otherwise the reader
thread would deadlock trying to acquire it.

### Completion ports (`libkernel/src/completion_port.rs`)

`CompletionPort::post()` returns `Option<usize>` — the woken waiter
thread index.  ISR-context callers ignore the return value.
Syscall-context callers (e.g., OP_NOP in `io_port.rs`) use it to yield
to the waiter.

### Process exit (`libkernel/src/process.rs`)

`terminate_process()` calls `yield_now()` before `kill_current_thread()`.
If the parent has a `wait_thread`, the donate target is set to the
parent's thread so it returns from `wait4` immediately.  The dying
thread's remaining quantum is donated to the parent.

## Safety constraints

- `yield_now()` must NOT be called from ISR context.  The scheduler lock
  could deadlock (ISR preempts code holding the lock, ISR tries to
  acquire lock → deadlock).
- ISR paths (e.g., `irq_fd_dispatch` → `CompletionPort::post()`) continue
  using plain `unblock()`.  This is fine because ISRs are short.
- All locks (pipe, completion port) must be dropped before calling
  `yield_now()`.

## Why `int 0x50` works from syscall context

During a SYSCALL handler the CPU runs on the kernel stack with GS =
kernel GS (from `swapgs` in the syscall entry stub).  `int 0x50` pushes
a ring-0 interrupt frame.  The yield stub sees RPL = 0 in the saved CS,
skips `swapgs`.  Saves all GPRs + FXSAVE.  `yield_tick` saves RSP,
switches to target's stack.  Target's frame (from its own yield or
timer preemption) is restored via `fxrstor` + GPR pops + `iretq`.

## Key files

| File | Change |
|------|--------|
| `libkernel/src/task/scheduler.rs` | `ipc_yield_stub` asm, `yield_tick`, `DONATE_TARGET`, public API |
| `libkernel/src/interrupts.rs` | Register vector 0x50 in IDT |
| `libkernel/src/file.rs` | `pipe_wake_reader` returns thread idx, yield in PipeWriter |
| `libkernel/src/completion_port.rs` | `post()` returns `Option<usize>` |
| `osl/src/io_port.rs` | Yield after OP_NOP post |
| `libkernel/src/process.rs` | Yield before `kill_current_thread` in `terminate_process` |
