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

### `spsc_ring/spsc_ring.tla` — SPSC Ring Buffer Protocol

Models the single-producer, single-consumer ring buffer used by IoRing's
SQ and CQ rings. Verifies that:

- No entry is read before written
- No entry is overwritten before consumed
- Every produced entry is eventually consumed
- head never passes tail

Maps to: `IoRing::post_cqe()` and userspace consumption in
`libkernel/src/completion_port.rs`.

### `completion_port/completion_port.tla` — CompletionPort Blocking Protocol

Models the completion port with multiple producers and a single consumer.
The consumer uses the `WaitCondition` protocol: check + set_waiter +
mark_blocked are a single atomic step (one PlusCal label) under the port
lock. After releasing the lock, the consumer awaits unblock.

This ensures that any producer calling `unblock()` always finds
`thread_state = "blocked"`, eliminating the lost-wakeup race.

TLC verifies all safety and liveness properties:
- `SafetyInvariant` — single waiter, bounded queue, sound accounting
- `NoStarvation` — a blocked thread is always eventually unblocked
- `AllDelivered` — all posted completions are eventually consumed

The same abstract protocol applies to all blocking sites in the kernel
(completion ports, pipes, console, IPC channels, wait4, clone, blocking).

## Running

```bash
cd specs

# Check the SPSC ring model (should pass)
tlc spsc_ring/spsc_ring.tla -config spsc_ring/spsc_ring.cfg

# Check the completion port model (should pass)
tlc completion_port/completion_port.tla -config completion_port/completion_port.cfg

# Or use the helper script:
./check.sh                    # run all specs
./check.sh completion_port    # run one spec
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
| Consumer.CheckAndAct (drain) | `p.pending()` / `ring.cq_available()` + `p.drain()` |
| Consumer.CheckAndAct (block) | `WaitCondition::wait_while(Some(guard), \|g, idx\| g.set_waiter(idx))` |
| Consumer.WaitUnblocked | `scheduler::yield_now()` (inside WaitCondition) |
| `thread_state := "blocked"` | `scheduler::mark_blocked()` (inside WaitCondition, under caller's lock) |
| Ring.WriteSlot + ReleaseTail | `*cqe_ptr = cqe` + `tail.store(Release)` |
| Ring.AcquireTail + ReadSlot | `tail.load(Acquire)` + `*cqe_ptr` |
