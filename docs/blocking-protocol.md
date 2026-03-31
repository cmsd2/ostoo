# Blocking Protocol

This document describes the blocking/wakeup protocol used throughout the
kernel, the lost-wakeup race it currently suffers from, and a proposed fix
based on a `WaitCondition` primitive.

Formal PlusCal/TLA+ models of the protocol live in `specs/`. See
[specs/PLUSCAL.md](../specs/PLUSCAL.md) for authoring instructions and
[specs/README.md](../specs/README.md) for per-spec details.

## Current protocol (buggy)

Every blocking site in the kernel follows this pattern:

```rust
{
    let mut guard = shared_state.lock();   // 1. acquire lock
    guard.waiter = Some(thread_idx);       // 2. register waiter
}                                          // 3. release lock
scheduler::block_current_thread();         // 4. mark Blocked + spin
```

The waker (a producer, timer, or signal) does:

```rust
{
    let mut guard = shared_state.lock();
    if let Some(t) = guard.waiter.take() { // clear waiter slot
        scheduler::unblock(t);             // wake up the blocked thread
    }
}
```

And `unblock()` is conditional:

```rust
pub fn unblock(thread_idx: usize) {
    let mut sched = SCHEDULER.lock();
    if let Some(t) = sched.threads.get_mut(thread_idx) {
        if t.state == ThreadState::Blocked {   // <-- only acts if Blocked
            t.state = ThreadState::Ready;
            sched.ready_queue.push_back(thread_idx);
        }
    }
}
```

### The race

A waker can execute between steps 3 (unlock) and 4 (mark Blocked):

1. **Waiter**: acquires lock, sets `waiter = Some(self)`, releases lock.
   Thread state is still `Running`.
2. **Waker**: acquires lock, calls `waiter.take()`, calls `unblock(waiter)`.
   `unblock` checks `state == Blocked` — it's `Running` — **no-op**.
   Waiter slot is now `None`.
3. **Waiter**: calls `block_current_thread()`, sets state to `Blocked`,
   spins forever. No future waker will call `unblock` because the waiter
   slot was already consumed. **Deadlock.**

This race is confirmed by the PlusCal model in `specs/completion_port.tla`
(TLC finds a deadlock trace) and by code inspection of `scheduler.rs` lines
640-676.

### Affected sites

The race affects **every** blocking site, not just the completion port:

| Site | File | Lock type |
|---|---|---|
| `sys_io_wait` | `osl/src/io_port.rs` | IrqMutex |
| `sys_io_ring_enter` phase 3 | `osl/src/io_port.rs` | IrqMutex |
| `PipeReader::read` | `libkernel/src/file.rs` | SpinMutex |
| `read_input` (console) | `libkernel/src/console.rs` | SpinMutex |
| `sys_ipc_send` | `osl/src/ipc.rs` | IrqMutex2 |
| `sys_ipc_recv` | `osl/src/ipc.rs` | IrqMutex2 |
| `sys_wait4` | `osl/src/syscalls/process.rs` | Mutex |
| `sys_clone` (vfork parent) | `osl/src/clone.rs` | Mutex |
| `blocking()` (async bridge) | `osl/src/blocking.rs` | SpinMutex |

The `sys_io_ring_enter` variant has an additional bug: check and set_waiter
are under **separate** lock acquisitions, so a completion can arrive between
the check and the registration.

## Proposed fix

### Step 1: Split `block_current_thread` into two halves

```rust
/// Mark the current thread Blocked. Safe to call while holding any lock.
/// Caller MUST call wait_until_unblocked() after releasing their lock.
// [spec: completion_port_fixed.tla CheckAndAct — "thread_state := blocked"]
pub fn mark_blocked() {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut sched = SCHEDULER.lock();
        let idx = sched.current_idx;
        sched.threads[idx].state = ThreadState::Blocked;
    });
}

/// Spin until another thread calls unblock(). Must be Blocked before calling.
// [spec: completion_port_fixed.tla WaitUnblocked — "await thread_state = running"]
pub fn wait_until_unblocked() {
    loop {
        x86_64::instructions::interrupts::enable_and_hlt();
        let state = x86_64::instructions::interrupts::without_interrupts(|| {
            let sched = SCHEDULER.lock();
            sched.threads[sched.current_idx].state
        });
        if state != ThreadState::Blocked {
            break;
        }
    }
}
```

This maps directly to the verified fix in `specs/completion_port_fixed.tla`:
`mark_blocked()` is inside the `CheckAndAct` label (under the lock) and
`wait_until_unblocked()` is the `WaitUnblocked` label (after unlock).

### Step 2: Migrate call sites manually

Each site becomes:

```rust
{
    let mut guard = shared_state.lock();   // 1. acquire lock
    guard.waiter = Some(thread_idx);       // 2. register waiter
    scheduler::mark_blocked();             // 3. mark Blocked UNDER LOCK
}                                          // 4. release lock
scheduler::wait_until_unblocked();         // 5. spin until woken
```

Now `unblock()` is guaranteed to find `Blocked` if the waiter slot is set,
because `mark_blocked()` ran before the lock was released.

### Step 3: Introduce `WaitCondition` to enforce the pattern

A condvar-like wrapper that makes the ordering impossible to get wrong:

```rust
/// Single-waiter condvar for kernel blocking.
///
/// Encapsulates the check → register → mark_blocked → unlock → wait cycle.
/// The type system ensures mark_blocked happens before the guard drops.
pub struct WaitCondition;

impl WaitCondition {
    /// If `predicate(guard)` returns true (i.e. "should block"), register
    /// the waiter, mark the thread Blocked, release the lock, and wait.
    /// Returns when unblocked.
    ///
    // [spec: completion_port_fixed.tla
    //   CheckAndAct (check + set_waiter + mark_blocked) = one label
    //   WaitUnblocked (await running) = next label]
    pub fn wait_while<T, L: Lock<T>>(
        mut guard: L::Guard<'_>,
        predicate: impl Fn(&T) -> bool,
        register: impl FnOnce(&mut T, usize),
    ) {
        if !predicate(&*guard) {
            return; // condition already satisfied, no need to block
        }
        let thread_idx = scheduler::current_thread_idx();
        register(&mut *guard, thread_idx);
        scheduler::mark_blocked();
        drop(guard);
        scheduler::wait_until_unblocked();
    }
}
```

Each blocking site reduces to a single call:

```rust
// Completion port
WaitCondition::wait_while(
    port.lock(),
    |p| p.pending() < min,
    |p, idx| p.set_waiter(idx),
);

// Pipe reader
WaitCondition::wait_while(
    self.inner.lock(),
    |inner| inner.buffer.is_empty() && !inner.write_closed,
    |inner, idx| { inner.reader_thread = Some(idx); },
);

// Console input
WaitCondition::wait_while(
    CONSOLE_INPUT.lock(),
    |c| c.buf.is_empty(),
    |c, idx| { c.blocked_reader = Some(idx); },
);

// sys_wait4
WaitCondition::wait_while(
    PROCESS_TABLE.lock(),
    |table| find_zombie_child(table, pid).is_none(),
    |table, idx| {
        table.get_mut(&pid).unwrap().wait_thread = Some(idx);
    },
);
```

### Step 4: Deprecate `block_current_thread`

Once all sites are migrated, remove or `#[deprecated]` the old monolithic
function. New blocking code must use `WaitCondition` or the split
`mark_blocked` / `wait_until_unblocked` pair.

## Lock ordering note

`mark_blocked()` acquires the scheduler's `SCHEDULER` SpinMutex internally.
This means any lock held by the caller must come **before** `SCHEDULER` in
the lock ordering. The current codebase already satisfies this: all IrqMutex
and SpinMutex locks protecting shared state are acquired before (and never
after) the scheduler lock.

If a future lock needs to be acquired after the scheduler lock, that lock
cannot be held when calling `mark_blocked()` — use the manual split instead,
and mark blocked before acquiring the inner lock.

## PlusCal correspondence

| Rust construct | PlusCal label | Atomicity |
|---|---|---|
| `guard = state.lock()` | Start of label | Lock acquired |
| `register(guard, idx)` | Same label | Under same lock |
| `mark_blocked()` | Same label | Under same lock (acquires SCHEDULER briefly) |
| `drop(guard)` | End of label | Lock released |
| `wait_until_unblocked()` | Next label | `await thread_state = "running"` |
| `unblock(idx)` in waker | Waker's label | `if thread_state = "blocked" then running` |

Each `WaitCondition::wait_while` call maps to exactly two PlusCal labels,
making formal verification straightforward.

## Relation to the io_ring_enter double-lock bug

`sys_io_ring_enter` phase 3 currently does:

```rust
{ let p = port.lock(); /* check cq_available */ }  // lock 1
{ let mut p = port.lock(); p.set_waiter(idx); }    // lock 2
scheduler::block_current_thread();                  // lock 3
```

The check and set_waiter are under separate locks, so a CQE posted between
them is missed. `WaitCondition` fixes this by construction: the predicate
check and waiter registration happen under a single lock acquisition.
