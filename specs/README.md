# Formal Specifications

PlusCal / TLA+ models for verifying ostoo's concurrent subsystems.

## Prerequisites

```bash
# macOS
brew install tlaplus

# Or download directly (requires Java 11+):
# https://github.com/tlaplus/tlaplus/releases
```

## Specs

### `spsc_ring.tla` — SPSC Ring Buffer Protocol

Models the single-producer, single-consumer ring buffer used by IoRing's
SQ and CQ rings. Verifies that:

- No entry is read before written
- No entry is overwritten before consumed
- Every produced entry is eventually consumed
- head never passes tail

Maps to: `IoRing::post_cqe()` and userspace consumption in
`libkernel/src/completion_port.rs`.

### `completion_port.tla` — CompletionPort (Buggy)

Models the completion port with multiple producers and a single consumer.
**This spec intentionally models the current code, including a lost-wakeup
race condition.** TLC will report a deadlock.

The race:
1. Consumer: `set_waiter(self)`, unlock port
2. Producer: `post()` → sees waiter, calls `unblock(consumer)`
   → thread state is `Running`, not `Blocked` → **unblock is a no-op**
3. Consumer: `block_current_thread()` → sets `Blocked`, spins forever
   → waiter slot was cleared → no future post will wake us → **deadlock**

Confirmed by reading `scheduler.rs` lines 640-676: `unblock()` only acts
when `state == Blocked`, and `block_current_thread()` sets `Blocked`
unconditionally after releasing the port lock.

### `completion_port_fixed.tla` — CompletionPort (Fixed)

Same model with the fix: set thread state to `Blocked` **while still
holding the port lock**, before releasing it. This ensures `unblock()`
always finds the thread in the `Blocked` state.

The Rust fix requires splitting `block_current_thread()` into two parts:
```rust
// BEFORE (buggy — scheduler.rs + io_port.rs):
{
    let mut p = port.lock();
    p.set_waiter(thread_idx);
}                                    // unlock
scheduler::block_current_thread();   // sets Blocked AFTER unlock — race!

// AFTER (fixed):
{
    let mut p = port.lock();
    p.set_waiter(thread_idx);
    scheduler::mark_blocked();       // set Blocked BEFORE unlock
}                                    // unlock
scheduler::wait_until_unblocked();   // spin on state != Blocked
```

TLC should verify this version passes all safety and liveness properties.

## Running

```bash
cd specs

# Check the SPSC ring model (should pass)
tlc spsc_ring.tla -config spsc_ring.cfg

# Check the buggy completion port (should find deadlock)
tlc completion_port.tla -config completion_port.cfg

# Check the fixed completion port (should pass)
tlc completion_port_fixed.tla -config completion_port_fixed.cfg
```

### Tuning

The `.cfg` files use small constants for fast checking. Increase for
more thorough verification at the cost of longer runtime:

| Constant | spsc_ring | completion_port | Notes |
|---|---|---|---|
| CAPACITY | 2 → 4 | — | Ring size |
| MAX_PRODUCE | 4 → 8 | 2 → 3 | Items per producer |
| NUM_PRODUCERS | — | 2 → 3 | Concurrent producers |
| MAX_QUEUED | — | 4 → 8 | Backpressure limit |

## Correspondence to Rust Code

| PlusCal concept | Rust code |
|---|---|
| Producer.Post (simple) | `IoRing::post_cqe()` |
| Producer.Post (deferred) | `CompletionPort::post()` → queue.push_back |
| Producer.Post wake | `self.waiter.take()` → `scheduler::unblock()` |
| Consumer.Check | `p.pending()` / `ring.cq_available()` |
| Consumer.Drain | Phase 2 drain in `sys_io_ring_enter()` |
| Consumer.SetWaiter | `p.set_waiter(thread_idx)` |
| Consumer.Block | `scheduler::block_current_thread()` |
| Ring.WriteSlot + ReleaseTail | `*cqe_ptr = cqe` + `tail.store(Release)` |
| Ring.AcquireTail + ReadSlot | `tail.load(Acquire)` + `*cqe_ptr` |
