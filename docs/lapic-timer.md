# LAPIC Timer

## Overview

The kernel uses the Local APIC (LAPIC) per-core timer as the primary tick source at 1000 Hz (1 ms resolution), replacing the legacy 8253 Programmable Interval Timer (PIT).

| Property | PIT | LAPIC timer |
|----------|-----|-------------|
| Frequency | 100 Hz (10 ms/tick) | 1000 Hz (1 ms/tick) |
| Scope | System-wide, ISA bus | Per-core, MMIO |
| Configuration | Port I/O | Memory-mapped registers |
| Timer future resolution | 10 ms | 1 ms |

## LAPIC Timer Calibration

The LAPIC timer counts down from a programmed initial value at a rate derived from the CPU bus frequency, which varies between machines. To determine the correct initial count for 1000 Hz, the kernel calibrates against the PIT.

### Algorithm

1. **Start one-shot countdown** — write `0xFFFF_FFFF` to `TimerInitialCount` with divide-by-16.
2. **Wait 500 ms** — busy-wait on `TICK_COUNT` for 50 PIT ticks (50 × 10 ms = 500 ms).
3. **Read elapsed count** — `elapsed = 0xFFFF_FFFF - TimerCurrentCount`.
4. **Compute bus frequency**:
   ```
   lapic_bus_freq = elapsed × divide × PIT_HZ / PIT_ticks_waited
                  = elapsed × 16 × 100 / 50
   ```
5. **Compute initial count for 1000 Hz**:
   ```
   initial_count = lapic_bus_freq / (divide × target_Hz)
                 = lapic_bus_freq / (16 × 1000)
   ```
6. **Start periodic timer** with the computed initial count.

### Implementation

`libkernel::apic::calibrate_and_start_lapic_timer()` in `libkernel/src/apic/mod.rs`:

- Called from `kernel/src/main.rs` after `libkernel::apic::init()` and `disable_pic()`.
- Releases the `LOCAL_APIC` lock before entering the HLT loop (phase 2) so the PIT ISR can proceed without deadlock.
- The LAPIC EOI address is already registered in `libkernel::LAPIC_EOI_ADDR` by `init_local()`.

### PIT Coexistence During Calibration

During the 500 ms calibration window, the PIT ISR (vector 0x20) is still active and increments `TICK_COUNT`. This is required — `wait_ticks()` depends on it. After the LAPIC timer starts:

- PIT continues at 100 Hz (vector 0x20 → `tick()`)
- LAPIC fires at 1000 Hz (vector 0x30 → `tick()`)

Both call `tick()`, giving approximately 1100 increments per second. The `Delay` future handles this correctly: early wakeups cause re-polls, which re-register the waker. Timing is slightly fast during calibration startup (~0.1% error), which is acceptable for kernel timers.

To eliminate the PIT contribution after calibration, mask GSI 2 in the IO APIC:
```rust
// follow-up: IO_APICS.lock()[0].mask_entry(2);
```
This is not yet implemented.

## Multi-Waker Design

### Problem with `AtomicWaker`

`futures_util::task::AtomicWaker` holds a single waker. With multiple concurrent `Delay` futures across different tasks, each `poll()` call overwrites the previous waker. When the ISR fires, only the last registered task is woken; others remain pending indefinitely.

### Solution: Fixed Waker Array

`libkernel/src/task/timer.rs` uses a fixed array of 8 optional wakers behind a spinlock:

```rust
static WAKERS: spin::Mutex<[Option<Waker>; 8]> = Mutex::new([None; 8]);
```

**On each tick** (`tick()` called from ISR):
1. Increment `TICK_COUNT`.
2. Acquire the lock (interrupts already disabled by CPU on IDT dispatch — no deadlock).
3. Take and wake every non-empty slot.

**In `Delay::poll()`**:
1. Check `TICK_COUNT >= target` — return `Ready` immediately if done.
2. Clone the waker (may allocate — must be done in task context, before disabling interrupts).
3. Disable interrupts (`without_interrupts`) and lock `WAKERS`.
4. Find an empty slot and insert the cloned waker. Panic if all slots are full (bug indicator).
5. Re-check `TICK_COUNT >= target` — return `Ready` if the ISR fired between step 1 and step 4.
6. Return `Pending`.

### ISR/Task Locking Contract

| Context | IF flag | Lock acquisition |
|---------|---------|-----------------|
| ISR (timer handler) | 0 (CPU clears on IDT dispatch) | Always succeeds immediately |
| Task (Delay::poll) | 1 (enabled) | Uses `without_interrupts` to prevent ISR re-entry while holding lock |

If a task held the lock with interrupts enabled, the ISR could fire and spin forever trying to acquire the same lock — a deadlock. `without_interrupts` prevents this.

## `TICKS_PER_SECOND` Constant

Defined in `libkernel/src/task/timer.rs`:

```rust
pub const TICKS_PER_SECOND: u64 = 1000;
```

Use it to convert between ticks and real time:

```rust
// Convert ticks to seconds elapsed
let secs = ticks() / TICKS_PER_SECOND;

// Create a 1-second delay
Delay::from_secs(1).await;

// Create a 250ms delay
Delay::from_millis(250).await;
```

`Delay::from_millis(ms)` uses ceiling division to avoid returning early:
```rust
Self::new((ms * TICKS_PER_SECOND + 999) / 1000)
```

## LAPIC Timer Registers

| Register | Offset | Purpose |
|----------|--------|---------|
| `LvtTimer` | `0x320` | Vector[7:0], mask[16], mode: one-shot[17]=0, periodic[17]=1 |
| `TimerInitialCount` | `0x380` | Write to start countdown |
| `TimerCurrentCount` | `0x390` | Read-only; current value |
| `TimerDivideConfiguration` | `0x3E0` | Bus clock divisor (0x3 = ÷16) |

The kernel uses divide-by-16 (`0x3`). The formula above accounts for this divisor.

## Key Files

| File | Role |
|------|------|
| `libkernel/src/task/timer.rs` | `tick()`, `wait_ticks()`, `Delay`, `TICKS_PER_SECOND`, waker array |
| `libkernel/src/interrupts.rs` | `LAPIC_TIMER_VECTOR = 0x30`, IDT entry, `lapic_timer_interrupt_handler` |
| `apic/src/local_apic/mapped.rs` | `start_oneshot_timer()`, `start_periodic_timer()`, `stop_timer()`, `read_current_count()` |
| `apic/src/lib.rs` | `calibrate_and_start_lapic_timer()` |
| `kernel/src/main.rs` | Calls calibration; spawns `timer_task()` |
