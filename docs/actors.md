# Actor System

## Overview

The kernel uses a lightweight actor model for device drivers and long-running
system services.  Each actor is an async task that owns its state behind an
`Arc`, receives typed messages through a `Mailbox`, and responds to requests
via one-shot `Reply` channels.

The design avoids shared mutable state and lock contention between drivers: all
cross-actor communication is by message passing.

---

## Core Primitives

### `Mailbox<M>` — `libkernel::task::mailbox`

An async, mutex-backed message queue.

```
sender                         receiver (actor run loop)
──────                         ────────────────────────
mailbox.send(msg)         →    while let Some(msg) = inbox.recv().await { ... }
                               (suspends when queue empty; woken on send)
mailbox.close()           →    recv() drains remaining msgs, then returns None
```

Key properties:

- **`send`** acquires the lock, checks `closed`, and either enqueues the message
  or drops it immediately.  Dropping a message also drops any embedded `Reply`,
  which closes the reply channel and unblocks the sender with `None`.
- **`close`** sets `closed = true` under the lock and wakes the receiver.
  Messages already in the queue are *not* removed — `recv` delivers them before
  returning `None`.  Any `send` arriving after `close` is silently dropped.
- **`reopen`** clears the closed flag, used when restarting a driver.
- The mutex makes `send` and `close` atomic with respect to each other,
  eliminating the race between "is it closed?" and "enqueue".

`recv` uses a double-check pattern to avoid missed wakeups:

```
poll():
  lock → dequeue / check closed → unlock   (fast path)
  register waker
  lock → dequeue / check closed → unlock   (second check)
  → Pending
```

The lock is always released before registering the waker and before waking it,
so a `send` or `close` that arrives between the two checks will either be seen
by the second check or will wake the (now-registered) waker.

### `Reply<T>` — one-shot response channel

`Reply<T>` is the sending half of a request/response pair.

```rust
// Actor receives:
ActorMsg::Info(reply) => reply.send(ActorInfo { name: "dummy" }),

// Sender awaits:
let info: Option<ActorInfo> = inbox.ask(ActorMsg::Info).await;
```

`Reply::new()` returns `(Reply<T>, Arc<Mailbox<T>>)`.  The actor calls
`reply.send(value)` to deliver a response; the `Drop` impl calls `close()` on
the inner mailbox regardless, so the receiver always unblocks:

- **`reply.send(value)`** → value pushed, then `Reply` dropped → `close()` called.
  `close()` does not drain the queue, so the value is still there for `recv`.
- **`reply` dropped without send** → `close()` called on an empty mailbox →
  `recv()` returns `None`.

### `ActorMsg<M>` — the envelope type

Every actor mailbox is `Mailbox<ActorMsg<M>>` where `M` is the actor-specific
message type.

```rust
pub enum ActorMsg<M> {
    Info(Reply<ActorInfo>),   // generic, handled by every actor
    Inner(M),                  // actor-specific
}
```

`ActorMsg::Info` is answered uniformly: every actor replies with
`ActorInfo { name }`.  Actor-specific messages travel as `ActorMsg::Inner(m)`.

### `ask` — the request/response pattern

```rust
// Returns Option<R>; None if the actor is stopped or dropped the reply.
let result = inbox.ask(|reply| ActorMsg::Inner(MyMsg::GetThing(reply))).await;
```

`ask` creates a `Reply`, wraps it in a message, sends it, and awaits the
response.  Because a closed mailbox drops incoming messages (and their
`Reply`s), `ask` on a stopped actor returns `None` immediately rather than
hanging.

---

## Driver Lifecycle — `devices::task_driver`

### `DriverTask` trait

```rust
pub trait DriverTask: Send + Sync + 'static {
    type Message: Send;
    fn name(&self) -> &'static str;
    fn run(
        handle: Arc<Self>,
        stop:   StopToken,
        inbox:  Arc<Mailbox<ActorMsg<Self::Message>>>,
    ) -> impl Future<Output = ()> + Send;
}
```

The `run` future is `'static` because all state is accessed through `Arc<Self>`.
`StopToken` can be polled between messages for cooperative stop, though most
actors simply let `inbox.recv()` return `None` (which happens when the mailbox
is closed by `stop()`).

### `TaskDriver<T>` — the lifecycle wrapper

`TaskDriver<T>` implements `Driver` (the registry interface) and owns:

| Field | Type | Purpose |
|---|---|---|
| `task` | `Arc<T>` | actor state, shared with the run future |
| `running` | `Arc<AtomicBool>` | set true on start, false when run exits |
| `stop_flag` | `Arc<AtomicBool>` | `StopToken` reads this |
| `inbox` | `Arc<Mailbox<ActorMsg<T::Message>>>` | message channel |

**Lifecycle:**

```
TaskDriver::new()
  inbox starts CLOSED → sends before start() are dropped immediately

start()
  inbox.reopen()          opens the mailbox
  running = true
  spawn(async { T::run(handle, stop, inbox).await; running = false; })

stop()
  stop_flag = true        StopToken fires
  inbox.close()           recv() will return None after draining

(run loop exits)
  running = false
```

`TaskDriver::new` returns `(TaskDriver<T>, Arc<Mailbox<ActorMsg<T::Message>>>)`.
The caller holds onto the `Arc<Mailbox>` to send actor-specific messages and
registers it in the process registry (see below).

---

## The `#[actor]` Macro — `devices_macros`

The macro generates a complete `DriverTask` implementation from an annotated
`impl` block, eliminating the run-loop boilerplate.

```rust
pub enum DummyMsg {
    SetInterval(u64),
}

pub struct Dummy;

#[actor("dummy", DummyMsg)]
impl Dummy {
    #[on_message(SetInterval)]
    async fn set_interval(&self, secs: u64) {
        info!("[dummy] interval set to {}s", secs);
    }
}
```

The macro generates:

```rust
// Inherent impl with the handler methods (stripped of #[on_message]):
impl Dummy {
    async fn set_interval(&self, secs: u64) { ... }
}

// DriverTask impl with the generated run loop:
impl DriverTask for Dummy {
    type Message = DummyMsg;
    fn name(&self) -> &'static str { "dummy" }

    async fn run(handle: Arc<Self>, _stop: StopToken,
                 inbox: Arc<Mailbox<ActorMsg<DummyMsg>>>) {
        log::info!("[dummy] started");
        while let Some(msg) = inbox.recv().await {
            match msg {
                ActorMsg::Info(reply) => {
                    reply.send(ActorInfo { name: "dummy" });
                }
                ActorMsg::Inner(msg) => match msg {
                    DummyMsg::SetInterval(secs) => handle.set_interval(secs).await,
                }
            }
        }
        log::info!("[dummy] stopped");
    }
}

// Convenience type alias:
pub type DummyDriver = TaskDriver<Dummy>;
```

`ActorMsg::Info` is always handled automatically — actor authors only write
handlers for their own `Inner` variants.

---

## Process Registry — `libkernel::task::registry`

The registry maps actor names to their mailboxes, allowing any code to send
messages to a named actor without holding a direct reference.

```rust
// Registration (at init time, in main.rs):
registry::register("dummy", dummy_inbox.clone());

// Typed lookup (when the caller knows the inner message type):
let inbox: Arc<Mailbox<ActorMsg<DummyMsg>>> = registry::get::<DummyMsg>("dummy")?;
inbox.send(ActorMsg::Inner(DummyMsg::SetInterval(5)));

// Generic info query (no knowledge of inner type needed):
if let Some(info) = registry::ask_info("dummy").await {
    println!("actor name: {}", info.name);
}
```

Each registry entry stores two representations of the same mailbox:

| Field | Type | Used for |
|---|---|---|
| `mailbox` | `Arc<dyn Any + Send + Sync>` | typed downcast via `get<M>` |
| `informable` | `Arc<dyn Informable>` | generic `Info` query via `ask_info` |

`Informable` is a simple object-safe trait:

```rust
pub trait Informable: Send + Sync {
    fn send_info(&self, reply: Reply<ActorInfo>);
}
// Blanket impl for all actor mailboxes:
impl<M: Send> Informable for Mailbox<ActorMsg<M>> { ... }
```

`ask_info` clones the `Arc<dyn Informable>` while holding the registry lock,
drops the lock, then sends the request and awaits the reply — the lock is never
held across an `await`.

---

## Manual Actor Implementation — the Shell

Actors that need async command dispatch or other patterns the macro does not
support implement `DriverTask` directly.  The shell is the canonical example.

### Two-task split

The shell separates keyboard reading from command execution:

```
keyboard_task (free async fn)
  KeyStream → buffers characters
  on Enter: inbox.ask(|r| ActorMsg::Inner(ShellMsg::KeyLine(line, r))).await
  (suspended here until the shell actor finishes the command)
                          ↓
Shell actor (DriverTask)
  inbox.recv().await
  ActorMsg::Info(reply)              → reply.send(ActorInfo { name: "shell" })
  ActorMsg::Inner(KeyLine(line, r))  → execute_command(&line).await
                                       r dropped on return
                                       → keyboard_task's ask().await unblocks
                                       → prompt reprinted
```

`ShellMsg::KeyLine` carries a `Reply<()>` as a **completion signal**, not a
data reply.  When the shell's handler returns, `_reply` is dropped, which calls
`Reply::drop` → `close()` on the reply mailbox → `keyboard_task`'s
`ask().await` returns.  This backpressure ensures only one command runs at a
time and the prompt is printed only after the command finishes.

The keyboard task is a plain `async fn`, not a `DriverTask`.  It is spawned
once at startup and runs for the lifetime of the kernel.  The shell actor can
be stopped and restarted independently via `driver stop/start shell`.

### Closed-mailbox behaviour for the shell

When the shell actor is stopped (`inbox.close()`), the keyboard task's next
`ask()` will find the mailbox closed and return `None` immediately (the
`ShellMsg::KeyLine` message is dropped, dropping `Reply<()>`, unblocking the
`ask`).  The keyboard task continues running and buffering characters; it will
simply not dispatch commands until the shell actor is restarted.

---

## Startup Sequence

```rust
// main.rs (abridged)

// Dummy driver
let (dummy_driver, dummy_inbox) = DummyDriver::new(Dummy);
devices::driver::register(Box::new(dummy_driver));
registry::register("dummy", dummy_inbox);         // mailbox starts closed

// Shell actor
let (shell_driver, shell_inbox) = ShellDriver::new(Shell::new());
devices::driver::register(Box::new(shell_driver));
registry::register("shell", shell_inbox.clone()); // mailbox starts closed
devices::driver::start_driver("shell").ok();      // reopen + spawn run loop

// Keyboard feeder (not a DriverTask, runs forever)
executor::spawn(Task::new(shell::keyboard_task(shell_inbox)));
```

The dummy driver starts stopped.  `driver start dummy` from the shell opens its
mailbox and spawns its run loop.

---

## File Map

| Path | Role |
|---|---|
| `libkernel/src/task/mailbox.rs` | `Mailbox<M>`, `Reply<T>`, `ActorMsg<M>`, `ActorInfo` |
| `libkernel/src/task/registry.rs` | process registry, `Informable`, `ask_info` |
| `devices/src/task_driver.rs` | `DriverTask` trait, `TaskDriver<T>`, `StopToken` |
| `devices/src/driver.rs` | `Driver` trait, driver registry (`start/stop/list`) |
| `devices-macros/src/lib.rs` | `#[actor]` and `#[on_message]` proc macros |
| `devices/src/dummy.rs` | example `#[actor]`-generated driver |
| `kernel/src/shell.rs` | manually-implemented actor + `keyboard_task` |
