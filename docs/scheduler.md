# Preemptive Scheduler & Multi-threaded Async Executor

## Overview

The kernel uses a **round-robin preemptive scheduler** built on top of the
LAPIC timer (1000 Hz).  Every 10 ms (configurable via `QUANTUM_TICKS`) the
timer ISR saves the current CPU state and switches to the next ready thread,
regardless of what that thread was doing.  This prevents any single async task
— even one that busy-loops — from starving all others.

The async executor's state lives in **global statics**, so multiple kernel
threads can pull and poll tasks from the same shared queue concurrently.

---

## Thread Lifecycle

```
        spawn_thread()
             │
             ▼
          [ Ready ] ◄──────────────────────────────┐
             │  (selected by scheduler)             │
             ▼                                      │
         [ Running ]  ──── quantum expired ─────────┘
```

Threads cycle between **Ready** and **Running** in strict round-robin order.
There is no blocked/sleeping state for threads — a thread that has nothing to
do (idle executor loop) calls `HLT` until an interrupt wakes it.

Thread 0 is the initial kernel thread (the boot stack).  Additional threads
are created with `scheduler::spawn_thread(entry: fn() -> !)`.  The entry
function must never return; in practice it calls `executor::run_worker()`.

---

## Context Switch Mechanism

### LAPIC timer IDT entry

The IDT entry for `LAPIC_TIMER_VECTOR` (0x30) is set with `set_handler_addr`
pointing directly at `lapic_timer_stub`.  This bypasses the
`extern "x86-interrupt"` wrapper so the stub can manipulate RSP freely.

### Assembly stub (`lapic_timer_stub`)

```asm
lapic_timer_stub:
    push rax; push rbx; push rcx; push rdx
    push rsi; push rdi; push rbp
    push r8;  push r9;  push r10; push r11
    push r12; push r13; push r14; push r15
    mov  rdi, rsp           // current_rsp → first argument
    call preempt_tick       // returns new rsp in rax
    mov  rsp, rax           // switch to (possibly new) thread's stack
    pop r15; pop r14; pop r13; pop r12
    pop r11; pop r10; pop r9;  pop r8
    pop rbp; pop rdi; pop rsi
    pop rdx; pop rcx; pop rbx; pop rax
    iretq
```

The CPU pushes an interrupt frame (SS/RSP/RFLAGS/CS/RIP, 40 bytes) before
the stub runs.  The stub pushes 15 GPRs (120 bytes).  Together that is
160 bytes = 10 × 16, so RSP is 16-byte aligned before `call`, satisfying the
SysV ABI (`RSP + 8` aligned at function entry).

### `preempt_tick(current_rsp: u64) -> u64`

Runs entirely on the **current** thread's stack (inside the `call`/`ret`
pair), then returns the next thread's `saved_rsp` in RAX.

1. Increments the global tick counter and wakes sleeping async tasks.
2. Sends LAPIC EOI.
3. Locks `SCHEDULER` (interrupts already off — no deadlock risk).
4. If not yet initialised, returns `current_rsp` unchanged.
5. Decrements the current thread's `ticks_remaining`; if still > 0, returns
   unchanged.
6. Saves `current_rsp` in `current_thread.saved_rsp`.
7. Pushes the current thread index onto `ready_queue` (marks it Ready).
8. Pops the front of `ready_queue` as `next_idx`.  Because we just pushed
   current, the queue is always non-empty; `unwrap_or(current_idx)` is only
   a safety fallback.  If current was the only thread it gets re-scheduled.
9. Resets `ticks_remaining = QUANTUM_TICKS`, marks thread as Running.
10. Returns `next_thread.saved_rsp`.

The stub then sets RSP = returned value and executes the symmetric pops +
`iretq`, which resumes execution on the new thread.

---

## Initial Stack Layout for New Threads

`spawn_thread(entry)` allocates a 64 KiB `Vec<u8>` and writes a fake
interrupt frame at the top.  The frame is exactly what a preempted thread's
stack looks like, so the same assembly stub can start a new thread as if it
were resuming a preempted one.

```
high address  ┌──────────────────────┐
              │  SS   = 0            │  ← null selector, valid for ring-0
              │  RSP  = stack_top−160│  ← thread's initial stack pointer
              │  RFLAGS = 0x202      │  ← bit 9 (IF) + bit 1 (reserved)
              │  CS   = 0x08         │  ← kernel code segment
              │  RIP  = entry        │  ← thread entry point
              │  r15  = 0            │
              │  r14  = 0            │
              │  …                   │
              │  rax  = 0            │  ← saved_rsp points here
low address   └──────────────────────┘
```

`saved_rsp` = address of the `r15` slot (bottom of the 160-byte region,
guaranteed 16-byte aligned by rounding `stack_top` down).

---

## Timer Quantum

`QUANTUM_TICKS` in `task/scheduler.rs` controls how many LAPIC ticks
(1 tick = 1 ms at 1000 Hz) each thread runs before being preempted.  The
default is **10** (10 ms per thread).

To increase to 50 ms:
```rust
pub const QUANTUM_TICKS: u32 = 50;
```

---

## Thread-safe Async Executor

### Global state

| Static | Type | Purpose |
|--------|------|---------|
| `TASK_QUEUE`   | `Mutex<VecDeque<Task>>`         | Tasks ready to be polled |
| `WAIT_MAP`     | `Mutex<BTreeMap<TaskId, Task>>` | Tasks waiting for a waker |
| `WAKE_QUEUE`   | `Arc<ArrayQueue<TaskId>>`       | Lock-free waker notifications (ISR-safe) |
| `WAKER_CACHE`  | `Mutex<BTreeMap<TaskId, Waker>>`| One Waker per live task; keeps Arc count ≥ 2 to prevent ISR deallocation |

`TASK_QUEUE` and `WAIT_MAP` use `spin::Mutex`.  On a single CPU with
preemption, a thread can be preempted while holding a spinlock; the new thread
spinning on the same lock will waste its quantum and yield back, at which
point the original thread releases the lock.  This is safe but not maximally
efficient — acceptable for a demo kernel.

### ISR-safe waker deallocation (`WAKER_CACHE`)

Both the timer ISR (`timer::tick`) and the keyboard ISR call `Waker::wake()`,
which consumes the stored `Waker`.  If that were the *last* `Arc<TaskWaker>`
reference, the `Drop` impl would call into `linked_list_allocator`, whose
spinlock may already be held by the preempted thread → **deadlock**.

`WAKER_CACHE` (`Mutex<BTreeMap<TaskId, Waker>>`) holds one cached `Waker` per
live task, keeping the `Arc` strong count ≥ 2 whenever an ISR-accessible copy
exists.  The ISR's drop reduces the count from 2 → 1; the cache's copy is only
freed from executor context when a task completes (`Poll::Ready`).

### `Task: Send` requirement

`Task::new` requires `Future<Output = ()> + Send + 'static`.  All built-in
tasks (timer, keyboard, example) satisfy this because they only hold values
that are `Send` (atomics, `Mutex`-guarded globals, simple scalars).

### `spawn(task)` and `run_worker()`

`executor::spawn` pushes a `Task` into `TASK_QUEUE`.  `executor::run_worker`
loops:
1. Move tasks whose wakers fired from `WAIT_MAP` → `TASK_QUEUE`.
2. Poll every task in `TASK_QUEUE`.  If `Pending`, move to `WAIT_MAP`.
3. `sleep_if_idle`: disable interrupts, check `WAKE_QUEUE`, then
   atomically re-enable + HLT (prevents missed-wakeup race).

---

## Locking Rules

| Lock | Where held | Rule |
|------|-----------|------|
| `SCHEDULER`           | ISR and non-ISR                       | Non-ISR callers **must** use `without_interrupts(|| ...)` |
| `TASK_QUEUE`          | Non-ISR only                          | Released before polling to allow `spawn()` inside poll |
| `WAIT_MAP`            | Non-ISR only                          | Released before locking `TASK_QUEUE` to avoid ordering inversion |
| `WAKER_CACHE`         | Non-ISR only                          | Released before polling |
| `timer::WAKERS`       | ISR (`tick`) + non-ISR (`Delay::poll`)| Non-ISR uses `without_interrupts` |

The ISR already runs with IF = 0, so it never needs to call
`without_interrupts`.

---

## Demonstrating Preemption

Add a spinning task to confirm no starvation:

```rust
executor::spawn(Task::new(async {
    loop { core::hint::spin_loop(); }
}));
```

Without preemption this would freeze the kernel.  With the scheduler, the
LAPIC timer fires every 10 ms and rotates to the next thread, so
`[timer] tick: Ns elapsed` still appears every second.
