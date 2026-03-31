# SMP Safety Audit

An audit of concurrency issues that would arise when running on multiple CPUs.
The kernel currently runs single-core only; this document catalogues what must
change before bringing up Application Processors.

Issues are grouped by severity:
**Critical** = data corruption / crash on SMP,
**High** = deadlock or lost wakeup,
**Medium** = ordering bugs or contention,
**Low** = design limitation / hardening.

---

## Critical

### 1. PerCpuData is a single static

`libkernel/src/syscall.rs:44-65`

`PerCpuData` (kernel\_rsp, user\_rsp, user\_rip, user\_rflags, user\_r9,
saved\_frame\_ptr) lives at a single address.  Every CPU's GS base points
there.  A SYSCALL on CPU 1 overwrites CPU 0's saved registers mid-flight.

**Impact**: Stack corruption, wrong return to userspace, privilege escalation.

**Fix**: Allocate a distinct `PerCpuData` page per CPU and set
`IA32_GS_BASE` / `IA32_KERNEL_GS_BASE` independently during AP bringup.

---

### 2. GDT / TSS / IST stacks are shared

`libkernel/src/gdt.rs:29-54`

A single `TSS` (with a single double-fault IST stack) and a single `GDT` are
used by all CPUs.  `set_kernel_stack()` (:77-84) unsafely mutates the shared
TSS's `rsp0` field.

**Impact**: Two CPUs taking a ring-3 → ring-0 transition simultaneously use
the same kernel stack.  Two simultaneous double faults corrupt each other's
IST stack.

**Fix**: Per-CPU GDT, per-CPU TSS, per-CPU IST stacks.

---

### 3. Scheduler has a single `current_idx`

`libkernel/src/task/scheduler.rs:130, 809, 836`

`SCHEDULER` is a single `SpinMutex<Scheduler>` with one `current_idx` field
that records which thread is currently executing.  On SMP each CPU runs a
different thread, but `current_idx` can only represent one.

**Impact**: Every use of `sched.current_idx` — preempt\_tick, block, save
context — operates on whichever CPU wrote it last, not the local CPU's thread.

**Fix**: Per-CPU `current_idx` (or per-CPU scheduler instances).

---

### 4. `block_current_thread()` uses stale `current_idx`

`libkernel/src/task/scheduler.rs:640-661`

```rust
pub fn block_current_thread() {
    without_interrupts(|| {
        let mut sched = SCHEDULER.lock();
        let idx = sched.current_idx;        // ← global, not per-CPU
        sched.threads[idx].state = Blocked;
    });
    loop {
        enable_and_hlt();
        let state = without_interrupts(|| {
            let sched = SCHEDULER.lock();
            let idx = sched.current_idx;    // ← may now be another CPU's thread
            sched.threads[idx].state
        });
        if state != Blocked { break; }
    }
}
```

If CPU 0 blocks thread A and CPU 1 runs thread B, the re-check reads
`current_idx` (now B) and tests the wrong thread.  Thread A either never
wakes or wakes with B's state.

**Impact**: Hung threads, wrong-thread wakeup.

**Fix**: Save the thread's own index before blocking; check that saved index
in the loop, not `current_idx`.

---

### 5. `CURRENT_THREAD_IDX_ATOMIC` is a single global

`libkernel/src/task/scheduler.rs:16-24`

```rust
static CURRENT_THREAD_IDX_ATOMIC: AtomicUsize = AtomicUsize::new(0);

pub fn current_thread_idx() -> usize {
    CURRENT_THREAD_IDX_ATOMIC.load(Ordering::Relaxed)
}
```

Called from ISR context (interrupt handlers), syscall context (console, pipes,
channels), and the scheduler itself.  On SMP the value reflects whichever CPU
wrote it last, not the caller's CPU.

**Impact**: Signal delivery, pipe wakeup, IPC blocking — all index the wrong
thread when the reading CPU differs from the writing CPU.

**Fix**: Per-CPU current-thread-index (read from a per-CPU variable or from
a CPU-local register like GS).

---

### 6. IO APIC register select/window interleaving

`libkernel/src/apic/io_apic/mapped.rs:120-142`

64-bit redirection entries are read/written as two 32-bit MMIO accesses
through a shared IOREGSEL / IOWIN register pair.  Although callers hold the
`IO_APICS` SpinMutex, the lock does not disable interrupts.  If a timer ISR
fires between the two halves of a 64-bit access on the same CPU, and the ISR
path touches IO APIC registers, the IOREGSEL is clobbered.

Currently no ISR path touches the IO APIC, so this is latent.  On SMP with
multiple IO APICs, per-APIC locking would be needed.

**Impact**: Corrupted redirection entry → interrupt routed to wrong vector or
silently masked.

**Fix**: Use `IrqMutex` (or at minimum `without_interrupts`) around all IO
APIC register-pair accesses.  Consider per-APIC locks for scalability.

---

## High

### 7. SCHEDULER lock is a SpinMutex — ISR can spin on it

`libkernel/src/task/scheduler.rs:130`

`SCHEDULER` uses `SpinMutex` (interrupts stay enabled).  Syscall-context
callers (`block_current_thread`, `unblock`, `spawn_thread`) wrap acquisitions
in `without_interrupts`, but the lock itself does not enforce this.
If a code path acquires the lock without disabling interrupts and the timer
ISR fires on the same CPU, `preempt_tick` (:803) will spin forever waiting for
the syscall to release the lock — which it never can, because it's preempted.

**Impact**: Deadlock (single-CPU or SMP).

**Fix**: Change `SCHEDULER` to `IrqMutex`, or ensure every acquisition site
uses `without_interrupts`.  All current sites do, but the type does not
enforce it — a future caller could forget.

---

### 8. MEMORY lock is not ISR-safe

`libkernel/src/memory/mod.rs:559`

`MEMORY` uses `SpinMutex`.  The comment warns "must not be called from
interrupt context", but this is not enforced by the type.  Any future ISR
path that triggers frame allocation or page-table manipulation will deadlock
on single-CPU if a syscall holds the lock.

**Impact**: Deadlock.

**Fix**: Change to `IrqMutex`, or add a compile-time / runtime ISR guard.

---

### 9. Global heap allocator is not ISR-safe

`libkernel/src/lib.rs:20`

```rust
#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();
```

`LockedHeap` uses `spin::Mutex` internally — no interrupt disabling.  Any
heap allocation from ISR context while a syscall holds the heap lock will
deadlock.

The scheduler's `push_back` on the ready queue can trigger a `Vec`
reallocation if the queue grows.  Currently the scheduler lock is acquired
with interrupts disabled, so the heap allocation happens with IF=0 — safe on
single-CPU.  On SMP, CPU 1's ISR could try to allocate while CPU 0 holds the
heap lock.

**Impact**: Deadlock (ISR + heap contention).

**Fix**: Use an ISR-safe allocator wrapper, or guarantee no heap allocation
from ISR context.

---

### 10. Console ISR → scheduler lock ordering

`libkernel/src/console.rs:35-47`

`push_input()` is called from the keyboard ISR.  It acquires
`CONSOLE_INPUT` (SpinMutex), then calls `scheduler::unblock()` which acquires
`SCHEDULER` (SpinMutex, inside `without_interrupts`).

On SMP:
- CPU 0: syscall holds SCHEDULER lock (IF disabled), tries to read console →
  acquires CONSOLE\_INPUT.
- CPU 1: keyboard ISR holds CONSOLE\_INPUT, calls `unblock()` →
  spins on SCHEDULER.
- CPU 0: still holds SCHEDULER, spins on CONSOLE\_INPUT held by CPU 1.

**Impact**: Deadlock (lock-order inversion: SCHEDULER → CONSOLE\_INPUT vs.
CONSOLE\_INPUT → SCHEDULER).

**Fix**: Don't call `unblock()` while holding CONSOLE\_INPUT.  Buffer the
thread index and call `unblock()` after dropping the console lock.

---

### 11. `DONATE_TARGET` is a single global

`libkernel/src/task/scheduler.rs:682-700`

`DONATE_TARGET: AtomicUsize` stores one target thread index, consumed by
the next `yield_tick`.  On SMP, CPU 0 stores a donate target, but CPU 1's
`yield_tick` consumes it first.

**Impact**: Scheduler donate delivers the wrong thread to the wrong CPU;
intended recipient never gets donated to.

**Fix**: Per-CPU donate target, or pass the target through a different
mechanism (e.g. IPI + per-CPU mailbox).

---

### 12. Lost wakeup in `sys_wait4`

`osl/src/syscalls/process.rs:26-64`

```
1. find_zombie_child(parent) → None
2. ← child exits on CPU 1, calls unblock(parent_wait_thread)
      but wait_thread is not yet set → no-op
3. set wait_thread = current_thread
4. block_current_thread()        → sleeps forever
```

The zombie check and the `wait_thread` registration are not atomic.

**Impact**: Parent process hangs forever waiting for an already-exited child.

**Fix**: Hold the process table lock across the zombie check and the
`wait_thread` write, so that `terminate_process()` on another CPU sees the
wait\_thread before posting the zombie.

---

## Medium

### 13. Relaxed ordering on cross-CPU atomics

Several atomics use `Ordering::Relaxed` where `Acquire`/`Release` would be
more appropriate for cross-CPU visibility:

| Atomic | File | Line | Used by |
|--------|------|------|---------|
| `CURRENT_THREAD_IDX_ATOMIC` | scheduler.rs | 16 | ISR + syscall |
| `current_pid` | process.rs | 488 | syscall context |
| `FOREGROUND_PID` | console.rs | 31 | keyboard ISR |
| `LAPIC_EOI_ADDR` | interrupts.rs | 12 | ISR |

On x86-64 all loads/stores are implicitly acquire/release for aligned
naturally-sized values, so this is a correctness concern primarily on
weakly-ordered architectures or under compiler reordering.  Using explicit
`Acquire`/`Release` is still best practice for documentation and portability.

**Impact**: Stale reads possible under compiler reordering; wrong-process
signal delivery, wrong-process console input routing.

**Fix**: `Release` on writes, `Acquire` on reads.

---

### 14. Stack arena contention

`libkernel/src/stack_arena.rs:16`

A single `SpinMutex<ArenaInner>` protects a 32-bit free bitmap for all
thread stack allocations / deallocations.  On SMP with frequent thread
creation, this becomes a serialisation bottleneck.

**Impact**: Performance (lock contention), not correctness.

**Fix**: Per-CPU arenas, or lock-free bitmap (atomic CAS on `u32`).

---

### 15. Lock ordering not documented or enforced

Multiple subsystems acquire locks in ad-hoc order.  Observed nesting:

- `CONSOLE_INPUT → SCHEDULER` (push\_input → unblock)
- `IrqInner → CompletionPort` (irq\_fd\_dispatch → post)
- `NotifyInner → CompletionPort` (signal\_notify → post)
- `PROCESS_TABLE → SCHEDULER` (with\_process → spawn\_user\_thread)

No static or runtime enforcement exists.  Adding a second CPU increases the
risk of discovering new inversion paths.

**Impact**: Latent deadlocks as code evolves.

**Fix**: Document a global lock ordering.  Consider runtime lock-order
checking in debug builds (e.g. per-CPU lock-stack tracking).

---

### 16. User memory TOCTOU with `CLONE_VM`

`osl/src/user_mem.rs:27-45`

`user_slice()` validates then returns a `'static` slice.  With `CLONE_VM`
(vfork), the parent and child share an address space.  If the child calls
`mmap` / `munmap` while the parent is mid-syscall with a validated slice, the
pages backing the slice may be unmapped.

Currently mitigated because `CLONE_VM` blocks the parent (`vfork`
semantics), so only the child runs.  If shared-address-space threading is
added, this becomes exploitable.

**Impact**: Latent use-after-free in shared address spaces.

**Fix**: Pin pages for the duration of the syscall, or copy user data into a
kernel buffer before releasing the process lock.

---

### 17. VMA / page-table flag divergence in `mprotect`

`osl/src/syscalls/mem.rs` (sys\_mprotect)

The process lock is released between `mprotect_vmas()` (updates VMA metadata)
and `update_user_page_flags()` (updates hardware page tables).  A concurrent
`mmap` or `munmap` on the same address range could see inconsistent state.

Currently safe because only one thread per process runs at a time (no kernel
threading within a process).

**Impact**: Latent protection-flag inconsistency if intra-process parallelism
is added.

**Fix**: Hold the process lock (or a per-address-space lock) across both the
VMA update and the page-table update.

---

## Low

### 18. LAPIC timer calibration is BSP-only

`libkernel/src/apic/mod.rs:205-254`

Calibration uses a global PIT busy-wait and assumes a single LAPIC.  Each AP
would need its own calibration pass (LAPIC frequencies can differ, especially
under virtualisation).

---

### 19. Dynamic vector allocation uses `without_interrupts`

`libkernel/src/interrupts.rs:22-75`

`register_handler()` disables local interrupts and acquires
`DYNAMIC_HANDLERS` (SpinMutex).  On SMP, `without_interrupts` only affects
the local CPU.  Two CPUs calling `register_handler()` concurrently will
correctly serialise via the SpinMutex — no bug, but the `without_interrupts`
wrapper is unnecessary and misleading.

---

### 20. Single ready queue scalability

`libkernel/src/task/scheduler.rs:103`

The single `VecDeque` ready queue serialises all scheduling decisions behind
one lock.  This is the standard starting point but will need per-CPU run
queues and work-stealing for acceptable SMP throughput.

---

## Summary

| # | Severity | Component | One-line summary |
|---|----------|-----------|------------------|
| 1 | Critical | syscall.rs | PerCpuData is a single static shared by all CPUs |
| 2 | Critical | gdt.rs | GDT / TSS / IST stacks shared across CPUs |
| 3 | Critical | scheduler.rs | Single `current_idx` — meaningless on SMP |
| 4 | Critical | scheduler.rs | `block_current_thread` reads stale `current_idx` |
| 5 | Critical | scheduler.rs | `CURRENT_THREAD_IDX_ATOMIC` is one global |
| 6 | Critical | io\_apic | Register select/window not ISR-safe |
| 7 | High | scheduler.rs | `SCHEDULER` SpinMutex not ISR-enforced |
| 8 | High | memory/mod.rs | `MEMORY` SpinMutex not ISR-safe |
| 9 | High | lib.rs | Global heap allocator not ISR-safe |
| 10 | High | console.rs | ISR lock-order inversion (CONSOLE → SCHEDULER) |
| 11 | High | scheduler.rs | `DONATE_TARGET` is a single global |
| 12 | High | process.rs | Lost wakeup in `sys_wait4` |
| 13 | Medium | various | Relaxed ordering on cross-CPU atomics |
| 14 | Medium | stack\_arena.rs | Single-lock bitmap contention |
| 15 | Medium | various | Lock ordering not documented |
| 16 | Medium | user\_mem.rs | TOCTOU with `CLONE_VM` (latent) |
| 17 | Medium | mem.rs | VMA / page-table flag divergence (latent) |
| 18 | Low | apic/mod.rs | LAPIC calibration BSP-only |
| 19 | Low | interrupts.rs | Misleading `without_interrupts` wrapper |
| 20 | Low | scheduler.rs | Single ready queue scalability |

## Recommended SMP bringup order

1. Per-CPU infrastructure: PerCpuData, GDT, TSS, IST stacks, LAPIC init.
2. Per-CPU scheduler state: `current_idx`, ready queue, donate target.
3. Fix `block_current_thread` to use saved thread index.
4. Promote `SCHEDULER` and `MEMORY` to `IrqMutex` (or add IF-disable wrappers).
5. Fix lock-ordering inversions (console, notify, channel → scheduler).
6. Fix `sys_wait4` lost-wakeup race.
7. Per-CPU LAPIC calibration.
8. Document and enforce global lock ordering.
