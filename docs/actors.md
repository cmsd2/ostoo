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
ActorMsg::Info(reply) => reply.send(ActorStatus { name: "dummy", running: true, info: () }),

// Sender awaits:
let status: Option<ActorStatus<()>> = inbox.ask(|r| ActorMsg::Info(r)).await;
```

`Reply::new()` returns `(Reply<T>, Arc<Mailbox<T>>)`.  The actor calls
`reply.send(value)` to deliver a response; the `Drop` impl calls `close()` on
the inner mailbox regardless, so the receiver always unblocks:

- **`reply.send(value)`** → value pushed, then `Reply` dropped → `close()` called.
  `close()` does not drain the queue, so the value is still there for `recv`.
- **`reply` dropped without send** → `close()` called on an empty mailbox →
  `recv()` returns `None`.

### `ActorMsg<M, I>` — the envelope type

Every actor mailbox is `Mailbox<ActorMsg<M, I>>` where `M` is the actor-specific
message type and `I` is the actor-specific info detail type (defaults to `()`).

```rust
pub enum ActorMsg<M, I: Send = ()> {
    /// Typed info request — reply carries ActorStatus<I> with the full detail.
    Info(Reply<ActorStatus<I>>),
    /// Type-erased info request from the process registry — reply carries
    /// ActorStatus<ErasedInfo> so callers can display detail without knowing I.
    ErasedInfo(Reply<ActorStatus<ErasedInfo>>),
    /// An actor-specific message.
    Inner(M),
}
```

`ActorStatus<I>` is the response to both info variants:

```rust
pub struct ActorStatus<I = ()> {
    pub name:    &'static str,
    pub running: bool,   // always true when the actor is responding
    pub info:    I,      // actor-specific detail
}
```

`ErasedInfo` is a type alias for the boxed detail used in type-erased queries:

```rust
pub type ErasedInfo = Box<dyn core::fmt::Debug + Send>;
```

### `ask` — the request/response pattern

```rust
// Returns Option<R>; None if the actor is stopped or dropped the reply.
let result = inbox.ask(|reply| ActorMsg::Inner(MyMsg::GetThing(reply))).await;
```

`ask` creates a `Reply`, wraps it in a message, sends it, and awaits the
response.  Because a closed mailbox drops incoming messages (and their
`Reply`s), `ask` on a stopped actor returns `None` immediately rather than
hanging.

**Self-query deadlock**: an actor must never use `ask` (or `registry::ask_info`)
to query its own mailbox from within a message handler — it cannot `recv()` the
response while blocked executing the current message.  Detect self-queries by
comparing names and respond directly instead.

---

## Driver Lifecycle — `devices::task_driver`

### `DriverTask` trait

```rust
pub trait DriverTask: Send + Sync + 'static {
    type Message: Send;
    type Info:    Send + 'static;
    fn name(&self) -> &'static str;
    fn run(
        handle: Arc<Self>,
        stop:   StopToken,
        inbox:  Arc<Mailbox<ActorMsg<Self::Message, Self::Info>>>,
    ) -> impl Future<Output = ()> + Send;
}
```

`type Info` is the actor-specific detail returned by `#[on_info]`.  Use `()`
if the actor has no custom info.

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
| `inbox` | `Arc<Mailbox<ActorMsg<T::Message, T::Info>>>` | message channel |

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

`TaskDriver::new` returns `(TaskDriver<T>, Arc<Mailbox<ActorMsg<T::Message, T::Info>>>)`.
The caller holds onto the `Arc<Mailbox>` to send actor-specific messages and
registers it in the process registry (see below).

---

## The `#[actor]` Macro — `devices_macros`

The macro generates a complete `DriverTask` implementation from an annotated
`impl` block, eliminating the run-loop boilerplate.

```rust
#[derive(Debug)]
pub struct DummyInfo {
    pub interval_secs: u64,
}

pub enum DummyMsg {
    SetInterval(u64),
}

pub struct Dummy {
    interval_secs: AtomicU64,
}

#[actor("dummy", DummyMsg)]
impl Dummy {
    #[on_info]
    async fn on_info(&self) -> DummyInfo {
        DummyInfo { interval_secs: self.interval_secs.load(Ordering::Relaxed) }
    }

    #[on_message(SetInterval)]
    async fn set_interval(&self, secs: u64) {
        self.interval_secs.store(secs, Ordering::Relaxed);
    }
}
```

The macro generates:

```rust
// Inherent impl with the handler methods (attributes stripped):
impl Dummy {
    async fn on_info(&self) -> DummyInfo { ... }
    async fn set_interval(&self, secs: u64) { ... }
}

// DriverTask impl with the generated run loop:
impl DriverTask for Dummy {
    type Message = DummyMsg;
    type Info    = DummyInfo;
    fn name(&self) -> &'static str { "dummy" }

    async fn run(handle: Arc<Self>, _stop: StopToken,
                 inbox: Arc<Mailbox<ActorMsg<DummyMsg, DummyInfo>>>) {
        log::info!("[dummy] started");
        while let Some(msg) = inbox.recv().await {
            match msg {
                ActorMsg::Info(reply) => {
                    reply.send(ActorStatus {
                        name: "dummy", running: true,
                        info: handle.on_info().await,
                    });
                }
                ActorMsg::ErasedInfo(reply) => {
                    let info: Box<dyn Debug + Send> = Box::new(handle.on_info().await);
                    reply.send(ActorStatus { name: "dummy", running: true, info });
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

### `#[on_info]` — custom actor info

Without `#[on_info]`, the generated `Info` and `ErasedInfo` arms respond with
`info: ()`.  Annotate one method with `#[on_info]` to provide actor-specific
detail:

```rust
#[on_info]
async fn on_info(&self) -> MyInfo {
    MyInfo { /* fields populated from self */ }
}
```

The return type must implement `Debug + Send` (for boxing into `ErasedInfo`).
`#[derive(Debug)]` is sufficient.  The macro sets `type Info = MyInfo` and
generates both arms automatically.

---

## Process Registry — `libkernel::task::registry`

The registry maps actor names to their mailboxes, allowing any code to send
messages to a named actor without holding a direct reference.

```rust
// Registration (at init time, in main.rs):
registry::register("dummy", dummy_inbox.clone());

// Typed lookup (when the caller knows both message and info types):
let inbox: Arc<Mailbox<ActorMsg<DummyMsg, DummyInfo>>> =
    registry::get::<DummyMsg, DummyInfo>("dummy")?;
inbox.send(ActorMsg::Inner(DummyMsg::SetInterval(5)));

// Type-erased info query (no knowledge of M or I needed):
if let Some(status) = registry::ask_info("dummy").await {
    println!("name: {}  running: {}  info: {:?}", status.name, status.running, status.info);
}
```

Each registry entry stores two representations of the same mailbox:

| Field | Type | Used for |
|---|---|---|
| `mailbox` | `Arc<dyn Any + Send + Sync>` | typed downcast via `get<M, I>` |
| `informable` | `Arc<dyn Informable>` | type-erased `ErasedInfo` query via `ask_info` |

`Informable` is a simple object-safe trait:

```rust
pub trait Informable: Send + Sync {
    fn send_info(&self, reply: Reply<ActorStatus<ErasedInfo>>);
}
// Blanket impl for all actor mailboxes:
impl<M: Send, I: Send + 'static> Informable for Mailbox<ActorMsg<M, I>> {
    fn send_info(&self, reply: Reply<ActorStatus<ErasedInfo>>) {
        self.send(ActorMsg::ErasedInfo(reply));
    }
}
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
  ActorMsg::Info(reply)              → reply.send(ActorStatus { name: "shell", ... })
  ActorMsg::ErasedInfo(reply)        → reply.send(ActorStatus { name: "shell", ... })
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

### Self-query avoidance

The `driver info shell` command would normally call `registry::ask_info("shell")`,
which sends `ErasedInfo` to the shell's own mailbox.  But the shell cannot
`recv()` that message while it is blocked executing the command — deadlock.

The shell detects this by comparing the requested name against `self.name()` and
responds directly without going through the registry:

```rust
} else if name == self.name() {
    // Respond directly — querying our own mailbox would deadlock.
    println!("  name:    {}", self.name());
    println!("  running: true");
}
```

Any actor that exposes a command interface and may be asked about itself must
apply the same pattern.

---

## Startup Sequence

```rust
// main.rs (abridged)

// Dummy driver
let (dummy_driver, dummy_inbox) = DummyDriver::new(Dummy::new());
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
| `libkernel/src/task/mailbox.rs` | `Mailbox<M>`, `Reply<T>`, `ActorMsg<M,I>`, `ActorStatus<I>`, `ErasedInfo` |
| `libkernel/src/task/registry.rs` | process registry, `Informable`, `ask_info` |
| `devices/src/task_driver.rs` | `DriverTask` trait, `TaskDriver<T>`, `StopToken` |
| `devices/src/driver.rs` | `Driver` trait, driver registry (`start/stop/list`) |
| `devices-macros/src/lib.rs` | `#[actor]`, `#[on_message]`, `#[on_info]` proc macros |
| `devices/src/dummy.rs` | example `#[actor]`-generated driver with `#[on_info]` |
| `kernel/src/shell.rs` | manually-implemented actor + `keyboard_task` |
