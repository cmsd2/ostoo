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

### `RecvTimeout<M>` — timed receive

`recv_timeout` races the inbox against a `Delay`, returning whichever fires first:

```rust
pub enum RecvTimeout<M> {
    Message(M),  // a message arrived before the deadline
    Closed,      // mailbox was closed (actor should exit)
    Elapsed,     // timer fired before any message
}

// Usage:
match inbox.recv_timeout(ticks).await {
    RecvTimeout::Message(msg) => { /* handle */ }
    RecvTimeout::Closed       => break,
    RecvTimeout::Elapsed      => { /* periodic work */ }
}
```

Used internally by the `#[on_tick]` generated run loop.

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
`impl` block, eliminating the run-loop boilerplate.  All attributes are
passthrough no-ops when used outside an `#[actor]` block.

### Basic usage — pure message actor

```rust
pub enum DummyMsg { SetInterval(u64) }

#[derive(Debug)]
pub struct DummyInfo { pub interval_secs: u64 }

pub struct Dummy { interval_secs: AtomicU64 }

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

**What the macro generates:**

```rust
// Inherent impl with handler methods (attributes stripped):
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
                ActorMsg::Info(reply) =>
                    reply.send(ActorStatus { name: "dummy", running: true,
                                            info: handle.on_info().await }),
                ActorMsg::ErasedInfo(reply) =>
                    reply.send(ActorStatus { name: "dummy", running: true,
                                            info: Box::new(handle.on_info().await) }),
                ActorMsg::Inner(msg) => match msg {
                    DummyMsg::SetInterval(secs) => handle.set_interval(secs).await,
                }
            }
        }
        log::info!("[dummy] stopped");
    }
}

// Convenience type alias (struct name + "Driver"):
pub type DummyDriver = TaskDriver<Dummy>;
```

Any methods in the `#[actor]` block that have no actor attribute are emitted
unchanged in the inherent impl and are callable from handler methods.

### `#[on_start]` — actor startup hook

Called once, after the `[actor] started` log line and before the message loop:

```rust
#[on_start]
async fn on_start(&self) {
    println!();
    print!("myactor> ");
}
```

Only one `#[on_start]` method is allowed per actor.

### `#[on_info]` — custom actor info

Without `#[on_info]`, `Info` and `ErasedInfo` reply with `info: ()`.  Annotate
one method to provide actor-specific detail:

```rust
#[on_info]
async fn on_info(&self) -> MyInfo {
    MyInfo { /* fields from self */ }
}
```

The return type must implement `Debug + Send`.  The macro infers `type Info =
MyInfo` and generates both `Info` and `ErasedInfo` arms automatically.

### `#[on_message(Variant)]` — inner message handler

Maps one enum variant of the actor's message type to an async handler:

```rust
#[on_message(DoThing)]
async fn do_thing(&self, n: u32) { ... }
```

The generated match arm is:

```rust
ActorMsg::Inner(MyMsg::DoThing(n)) => handle.do_thing(n).await,
```

Multiple `#[on_message]` methods are allowed, one per variant.

### `#[on_tick]` — periodic callback

When present, the macro switches to a **unified `poll_fn` loop** (see below)
that races the inbox against a `Delay`.  The actor must also provide a plain
`tick_interval_ticks(&self) -> u64` method (no attribute needed):

```rust
fn tick_interval_ticks(&self) -> u64 {
    self.interval_secs.load(Ordering::Relaxed) * TICKS_PER_SECOND
}

#[on_tick]
async fn heartbeat(&self) {
    log::info!("[myactor] tick");
}
```

Only one `#[on_tick]` method is allowed per actor.  The delay is reset after
each tick so `tick_interval_ticks` can change dynamically.

### `#[on_stream(factory)]` — interrupt/hardware stream source

Actors that need to react to hardware events (interrupts, async streams) use
`#[on_stream]`.  The `factory` argument names a plain method that returns a
`Stream + Unpin`; the handler is called for each item:

```rust
// Factory — called once when the actor starts:
fn key_stream(&self) -> KeyStream { KeyStream::new() }

// Handler — called for each item from the stream:
#[on_stream(key_stream)]
async fn on_key(&self, key: Key) {
    // process key event
}
```

Multiple `#[on_stream]` methods are allowed, one per stream.

### The unified `poll_fn` loop

When one or more `#[on_stream]` or `#[on_tick]` attributes are present the
macro generates a loop that races **all event sources in a single `poll_fn`**:

```rust
// Streams initialised once before the loop:
let mut _stream_0 = handle.key_stream();
// Timer initialised if #[on_tick] is present:
let mut _delay = Delay::new(handle.tick_interval_ticks());

loop {
    enum _Event {
        _Inbox(ActorMsg<KeyboardMsg, KeyboardInfo>),
        _Stream0(Key),   // one variant per #[on_stream]
        _Tick,           // present if #[on_tick]
        _Stopped,
    }
    let mut _recv = inbox.recv();
    let _ev = poll_fn(|cx| {
        // Streams polled first — interrupt-driven, lowest latency:
        match poll_stream_next(&mut _stream_0, cx) {
            Poll::Ready(Some(item)) => return Poll::Ready(_Event::_Stream0(item)),
            Poll::Ready(None)       => return Poll::Ready(_Event::_Stopped),
            Poll::Pending           => {}
        }
        // Inbox — control messages and stop signal:
        match Pin::new(&mut _recv).poll(cx) {
            Poll::Ready(Some(msg)) => return Poll::Ready(_Event::_Inbox(msg)),
            Poll::Ready(None)      => return Poll::Ready(_Event::_Stopped),
            Poll::Pending          => {}
        }
        // Timer (lowest priority):
        if let Poll::Ready(()) = Pin::new(&mut _delay).poll(cx) {
            return Poll::Ready(_Event::_Tick);
        }
        Poll::Pending
    }).await;

    match _ev {
        _Event::_Stopped         => break,
        _Event::_Inbox(msg)      => match msg { /* Info, ErasedInfo, Inner arms */ }
        _Event::_Stream0(key)    => handle.on_key(key).await,
        _Event::_Tick            => {
            handle.heartbeat().await;
            _delay = Delay::new(handle.tick_interval_ticks());
        }
    }
}
```

All wakers (mailbox `AtomicWaker`, stream `AtomicWaker`, timer `WAKERS` slot)
register the **same** task waker, so whichever source fires first reschedules
the task.  No extra task or thread is needed.

### Using `#[actor]` outside the `devices` crate

The macro generates `impl crate::task_driver::DriverTask for …` and
`pub type XDriver = crate::task_driver::TaskDriver<X>;`.  In the `devices` crate
this resolves naturally.  For crates that use `devices` as a dependency (e.g.
`kernel`), expose `task_driver` at the crate root:

```rust
// kernel/src/task_driver.rs
pub use devices::task_driver::*;

// kernel/src/main.rs
pub mod task_driver;   // makes crate::task_driver resolve for #[actor] expansions
```

The generated type alias uses the struct name suffixed with `Driver`:
`KeyboardActor` → `KeyboardActorDriver`, `Shell` → `ShellDriver`.

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

## Actors in Practice

### Shell — pure message actor with startup hook

```rust
pub enum ShellMsg { KeyLine(String) }
pub struct Shell;

#[actor("shell", ShellMsg)]
impl Shell {
    #[on_start]
    async fn on_start(&self) {
        println!();
        print!("ostoo> ");
    }

    #[on_message(KeyLine)]
    async fn on_key_line(&self, line: String) {
        self.execute_command(&line).await;
        print!("ostoo> ");
    }

    // Plain helpers — land in the inherent impl:
    async fn execute_command(&self, line: &str) { ... }
    async fn cmd_driver(&self, rest: &str) { ... }
}
```

The shell prints its prompt in `#[on_start]` (once, when the actor starts) and
again after each command in `#[on_message(KeyLine)]`.

**Fire-and-forget dispatch**: the keyboard actor sends `ShellMsg::KeyLine` with
`mailbox.send()` (no reply), so it never blocks waiting for the shell.  The
shell processes one command at a time; new lines queue in the mailbox.

**Self-query avoidance**: `driver info shell` from within a shell command would
deadlock if it sent `ErasedInfo` to the shell's own mailbox (the shell is busy
executing the command and cannot recv).  The handler detects the name `"shell"`
and responds directly without going through the registry.

### Keyboard — stream actor

```rust
pub struct KeyboardActor {
    keys_processed:   AtomicU64,
    lines_dispatched: AtomicU64,
    line:             spin::Mutex<LineBuf>,
}

#[actor("keyboard", KeyboardMsg)]
impl KeyboardActor {
    fn key_stream(&self) -> KeyStream { KeyStream::new() }

    #[on_stream(key_stream)]
    async fn on_key(&self, key: Key) {
        // buffer characters; dispatch complete lines to shell via send()
    }

    #[on_info]
    async fn on_info(&self) -> KeyboardInfo { ... }
}
```

`KeyStream` is interrupt-driven: every PS/2 scancode IRQ pushes into a lock-free
queue and wakes an `AtomicWaker`.  Because both the stream waker and the inbox
waker register the same task waker, the actor sleeps in a single `poll_fn` and
wakes on whichever event arrives first.

The line buffer lives in the actor struct behind a `spin::Mutex<LineBuf>` so it
is accessible from the `&self` reference in `on_key`.  The mutex is never held
across an `.await`.

### Dummy — tick actor (example / test driver)

```rust
#[actor("dummy", DummyMsg)]
impl Dummy {
    fn tick_interval_ticks(&self) -> u64 {
        self.interval_secs.load(Ordering::Relaxed) * TICKS_PER_SECOND
    }

    #[on_tick]
    async fn heartbeat(&self) {
        log::info!("[dummy] heartbeat");
    }

    #[on_info]
    async fn on_info(&self) -> DummyInfo { ... }

    #[on_message(SetInterval)]
    async fn set_interval(&self, secs: u64) { ... }
}
```

Starts stopped.  `driver start dummy` from the shell opens its mailbox and
spawns the run loop.  `driver dummy set-interval 3` sends `SetInterval(3)` and
changes the heartbeat rate at runtime.

---

## Startup Sequence

```rust
// main.rs (abridged)

// Dummy driver — starts stopped, user can start it from the shell
let (dummy_driver, dummy_inbox) = DummyDriver::new(Dummy::new());
devices::driver::register(Box::new(dummy_driver));
registry::register("dummy", dummy_inbox);

// Shell actor — started immediately
let (shell_driver, shell_inbox) = ShellDriver::new(Shell::new());
devices::driver::register(Box::new(shell_driver));
registry::register("shell", shell_inbox.clone());
devices::driver::start_driver("shell").ok();   // reopen + spawn run loop

// Keyboard actor — started immediately, stream-driven by PS/2 IRQs
let (kb_driver, kb_inbox) =
    KeyboardActorDriver::new(KeyboardActor::new());
devices::driver::register(Box::new(kb_driver));
registry::register("keyboard", kb_inbox);
devices::driver::start_driver("keyboard").ok();
```

---

## File Map

| Path | Role |
|---|---|
| `libkernel/src/task/mailbox.rs` | `Mailbox<M>`, `Reply<T>`, `ActorMsg<M,I>`, `ActorStatus<I>`, `ErasedInfo`, `RecvTimeout<M>` |
| `libkernel/src/task/mod.rs` | `poll_stream_next` helper used by macro-generated code |
| `libkernel/src/task/registry.rs` | process registry, `Informable`, `ask_info` |
| `devices/src/task_driver.rs` | `DriverTask` trait, `TaskDriver<T>`, `StopToken` |
| `devices/src/driver.rs` | `Driver` trait, driver registry (`start/stop/list`) |
| `devices-macros/src/lib.rs` | `#[actor]`, `#[on_message]`, `#[on_info]`, `#[on_start]`, `#[on_tick]`, `#[on_stream]` |
| `devices/src/dummy.rs` | tick + message actor (`#[on_tick]`, `#[on_message]`, `#[on_info]`) |
| `kernel/src/shell.rs` | shell actor (`#[on_start]`, `#[on_message]`) |
| `kernel/src/keyboard_actor.rs` | keyboard actor (`#[on_stream]`, `#[on_info]`) |
| `kernel/src/task_driver.rs` | `pub use devices::task_driver::*` shim for `crate::task_driver` path |
