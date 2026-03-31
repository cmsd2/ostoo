# Blocking Protocol

This document describes the blocking/wakeup protocol used throughout the
kernel, the lost-wakeup race it currently suffers from, and a proposed fix
based on an idle thread and a `WaitCondition` primitive.

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

### Why Blocked threads spin today

Both `preempt_tick` and `yield_tick` handle an empty ready queue by returning
`current_rsp` — i.e. they keep running the current thread even if it's
Blocked. This forces `block_current_thread()` to include a HLT spin loop:
the Blocked thread keeps running on the CPU, calling `enable_and_hlt()` in a
loop, waiting for the next timer interrupt to check if `unblock()` has
changed its state. This wastes up to one full quantum (10ms) per blocking
event and prevents the CPU from doing useful work while the thread is
Blocked.

## Proposed fix

The fix has three parts: an idle thread that eliminates the need for blocked
threads to spin, a split of `block_current_thread` that fixes the race, and
a `WaitCondition` wrapper that makes the correct pattern easy and the buggy
pattern impossible.

### Step 1: Add an idle thread

Create a per-CPU idle thread that the scheduler falls back to when the ready
queue is empty. The idle thread does nothing but HLT in a loop, yielding the
CPU until the next interrupt:

```rust
fn idle_thread() -> ! {
    loop {
        x86_64::instructions::interrupts::enable_and_hlt();
    }
}
```

The idle thread is created during scheduler init and stored in `Scheduler`:

```rust
struct Scheduler {
    // ...
    idle_thread_idx: usize,  // always present, never on the ready queue
}
```

Then `preempt_tick` and `yield_tick` switch to the idle thread instead of
staying on a Blocked/Dead thread:

```rust
let next_idx = match sched.ready_queue.pop_front() {
    Some(idx) => idx,
    None => sched.idle_thread_idx,  // was: return current_rsp
};
```

The idle thread is never pushed onto the ready queue. The scheduler only runs
it as a fallback when nothing else is Ready. The first `unblock()` call
pushes a real thread onto the ready queue, and the next timer tick preempts
idle and switches to it.

With this change, a Blocked thread no longer needs to spin — the scheduler
context-switches away from it immediately and never schedules it again until
`unblock()` makes it Ready.

### Step 2: Split `block_current_thread` into mark + yield

```rust
/// Mark the current thread Blocked and yield to the scheduler.
///
/// Safe to call while holding any lock (acquires SCHEDULER briefly).
/// The scheduler will context-switch away and never schedule this thread
/// again until unblock() is called. Execution resumes at the instruction
/// after this call.
// [spec: completion_port_fixed.tla CheckAndAct — "thread_state := blocked"
//        + WaitUnblocked — "await thread_state = running"]
pub fn mark_blocked_and_yield() {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut sched = SCHEDULER.lock();
        let idx = sched.current_idx;
        sched.threads[idx].state = ThreadState::Blocked;
    });
    yield_now();  // context-switch away; resume here after unblock + reschedule
}
```

There is no spin loop. `yield_now()` triggers `int 0x50`, which enters
`yield_tick`. The scheduler sees the thread is Blocked, does not re-queue it,
and switches to the next ready thread (or idle). When `unblock()` is called
later, the thread is pushed onto the ready queue with state Ready. The
scheduler eventually picks it and context-switches back, resuming execution
right after the `yield_now()` call.

A separate `mark_blocked()` (without yield) is still useful for callers that
need to mark Blocked under a lock and yield after dropping it:

```rust
/// Mark the current thread Blocked. Does NOT yield.
/// Caller must call yield_now() after releasing their lock.
pub fn mark_blocked() {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut sched = SCHEDULER.lock();
        let idx = sched.current_idx;
        sched.threads[idx].state = ThreadState::Blocked;
    });
}
```

### Step 3: Migrate call sites

Each site becomes:

```rust
{
    let mut guard = shared_state.lock();   // 1. acquire lock
    guard.waiter = Some(thread_idx);       // 2. register waiter
    scheduler::mark_blocked();             // 3. mark Blocked UNDER LOCK
}                                          // 4. release lock
scheduler::yield_now();                    // 5. context-switch away
// execution resumes here after unblock + reschedule
```

No loop, no spin, no HLT. The thread is off the CPU until explicitly woken.

### Step 4: Introduce `WaitCondition` to enforce the pattern

A condvar-like wrapper that makes the ordering impossible to get wrong:

```rust
/// Single-waiter condvar for kernel blocking.
///
/// Encapsulates the check → register → mark_blocked → unlock → yield cycle.
/// The type system ensures mark_blocked happens before the guard drops.
pub struct WaitCondition;

impl WaitCondition {
    /// If `predicate(guard)` returns true (i.e. "should block"), register
    /// the waiter, mark the thread Blocked, release the lock, and yield.
    /// Returns when unblocked and rescheduled.
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
        scheduler::mark_blocked();  // mark Blocked while lock held
        drop(guard);                // release lock
        scheduler::yield_now();     // context-switch away; resume after unblock
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

### Step 5: Deprecate `block_current_thread`

Once all sites are migrated, remove or `#[deprecated]` the old monolithic
function. New blocking code must use `WaitCondition` or the
`mark_blocked()` / `yield_now()` pair.

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
| `yield_now()` | Next label | `await thread_state = "running"` |
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
