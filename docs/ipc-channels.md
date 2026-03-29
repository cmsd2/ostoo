# IPC Channels

## Overview

Capability-based IPC channels for structured message passing between processes.
A channel is a unidirectional message conduit with configurable buffer capacity.
Channels come in pairs: a **send end** and a **receive end**, each exposed as a
file descriptor (capability).

The buffer capacity, set at creation time, determines the communication model:

- **capacity = 0** -- Synchronous rendezvous.  Sender blocks until a receiver
  calls recv.  Direct message transfer with scheduler donate for minimal
  latency (matching seL4 endpoint characteristics).
- **capacity > 0** -- Asynchronous buffered.  Sender enqueues and returns
  immediately.  Blocks only when the buffer is full.

This gives applications full control: create a sync channel for tight RPC-style
communication, or an async channel for decoupled producer-consumer patterns.

---

## Message Format

```c
struct ipc_message {
    uint64_t tag;       /* user-defined message type */
    uint64_t data[3];   /* 24 bytes of inline payload */
    int32_t  fds[4];    /* file descriptors for capability passing (-1 = unused) */
};
/* Total: 48 bytes */
```

The `tag` field is opaque to the kernel -- applications use it to identify
message types.  The `data` array carries the payload (pointers, handles,
small structs).  The `fds` array carries file descriptors for capability
passing (set unused slots to -1).  For bulk data, use shared memory with
a channel for signaling.

---

## Syscalls

### ipc_create (505)

```
long ipc_create(int fds[2], unsigned capacity, unsigned flags);
```

Creates a channel pair.  Writes the send-end fd to `fds[0]` and the
receive-end fd to `fds[1]`.

| Parameter  | Description |
|-----------|-------------|
| `fds`      | User pointer to a 2-element int array |
| `capacity` | Buffer capacity: 0 = sync, >0 = async buffered |
| `flags`    | `IPC_CLOEXEC` (0x1): set close-on-exec on both fds |

Returns 0 on success, negative errno on failure.

### ipc_send (506)

```
long ipc_send(int fd, const struct ipc_message *msg, unsigned flags);
```

Send a message through a send-end fd.

| Parameter | Description |
|----------|-------------|
| `fd`      | Send-end file descriptor |
| `msg`     | Pointer to message in user memory |
| `flags`   | `IPC_NONBLOCK` (0x1): return `-EAGAIN` instead of blocking |

**Blocking behavior**:
- Sync (cap=0): blocks until a receiver calls recv, then transfers directly
- Async (cap>0): blocks only if the buffer is full

Returns 0 on success, `-EPIPE` if receive end is closed, `-EAGAIN` if
non-blocking and would block.

### ipc_recv (507)

```
long ipc_recv(int fd, struct ipc_message *msg, unsigned flags);
```

Receive a message from a receive-end fd.

| Parameter | Description |
|----------|-------------|
| `fd`      | Receive-end file descriptor |
| `msg`     | Pointer to buffer in user memory |
| `flags`   | `IPC_NONBLOCK` (0x1): return `-EAGAIN` instead of blocking |

Returns 0 on success, `-EPIPE` if send end is closed and no messages remain.

---

## Examples

### Sync channel (capacity=0)

```c
int fds[2];
ipc_create(fds, 0, 0);         /* sync channel */
int send_fd = fds[0], recv_fd = fds[1];

/* In child (after clone+execve, with recv_fd inherited): */
struct ipc_message msg;
ipc_recv(recv_fd, &msg, 0);    /* blocks until parent sends */

/* In parent: */
struct ipc_message req = { .tag = 1, .data = {42, 0, 0}, .fds = {-1, -1, -1, -1} };
ipc_send(send_fd, &req, 0);    /* blocks until child recvs, then donates */
```

### Async channel (capacity=4)

```c
int fds[2];
ipc_create(fds, 4, 0);         /* buffered, 4 messages */

/* Producer can send 4 messages without blocking: */
for (int i = 0; i < 4; i++) {
    struct ipc_message m = { .tag = i };
    ipc_send(fds[0], &m, 0);
}

/* Consumer drains: */
struct ipc_message m;
while (ipc_recv(fds[1], &m, IPC_NONBLOCK) == 0) {
    /* process m */
}
/* returns -EAGAIN when empty */
```

### fd-passing (capability transfer)

```c
/* Create a pipe and an IPC channel */
int pipe_fds[2], ch_fds[2];
pipe(pipe_fds);
ipc_create(ch_fds, 4, 0);

/* Send the pipe write-end through the channel */
struct ipc_message msg = {
    .tag = 1,
    .data = { 0, 0, 0 },
    .fds = { pipe_fds[1], -1, -1, -1 },   /* transfer pipe write-end */
};
ipc_send(ch_fds[0], &msg, 0);

/* Receive — kernel allocates a new fd for the pipe write-end */
struct ipc_message recv_msg;
ipc_recv(ch_fds[1], &recv_msg, 0);
int new_write_fd = recv_msg.fds[0];   /* new fd number in receiver */

write(new_write_fd, "hello", 5);      /* writes to the same pipe */
```

**Semantics**: When `ipc_send` is called with non-(-1) values in the `fds`
array, the kernel looks up each fd in the sender's fd table, increments
reference counts, and stores the kernel objects inside the channel.  When
`ipc_recv` delivers the message, the kernel allocates new fds in the
receiver's fd table and rewrites `msg.fds` with the new fd numbers.

**Error handling**: If any fd in `msg.fds` is invalid, the entire send fails
with `-EBADF`.  If the receiver's fd table is full, recv fails with
`-EMFILE`.

**Cleanup**: If a message with transferred fds is never received (e.g., the
channel is destroyed with messages in the queue), the kernel closes the
transferred fd objects automatically.

---

## Kernel Implementation

### Files

| File | Purpose |
|------|---------|
| `libkernel/src/channel.rs` | `ChannelInner` kernel object, `IpcMessage` struct, send/recv/close logic |
| `libkernel/src/file.rs` | `FdObject::Channel(ChannelFd)` variant, `ChannelFd::Send`/`Recv` |
| `osl/src/ipc.rs` | Syscall implementations (`sys_ipc_create/send/recv`) |
| `osl/src/syscall_nr.rs` | `SYS_IPC_CREATE=505`, `SYS_IPC_SEND=506`, `SYS_IPC_RECV=507` |

### Sync rendezvous internals

When capacity=0, sender and receiver rendezvous directly:

1. If receiver is already blocked: sender copies message to `pending_send`,
   unblocks receiver, donates quantum via `set_donate_target` + `yield_now`
2. If no receiver: sender stores message in `pending_send`, records thread
   index, blocks via `block_current_thread()`
3. Receiver wakes, takes message from `pending_send`, unblocks sender

This uses the same `block_current_thread` / `unblock` / `donate` primitives
as pipes and waitpid (see `docs/scheduler-donate.md`).

### Async buffered internals

Messages are stored in a `VecDeque<IpcMessage>` bounded by capacity:

- **Send**: push to queue, wake blocked receiver if any
- **Recv**: pop from queue, wake blocked sender if queue was full
- **Queue full**: sender blocks until receiver drains
- **Queue empty**: receiver blocks until sender enqueues

---

## Design Decisions

**Unidirectional**: Simpler and more composable than bidirectional.  For RPC,
use two channels (request + reply).  For server fan-in, share the send-end fd
via dup/fork.

**Fixed-size messages**: No heap allocation per message.  48 bytes fits common
control-plane payloads plus 4 file descriptors for capability passing.
Bulk data should use shared memory.

**Capacity determines semantics**: The application chooses sync vs async at
creation time, not at each send/recv.  This makes the channel's behavior
predictable and matches the Go channels model.

**IPC_NONBLOCK flag**: Adds flexibility for polling patterns and try-send/
try-recv without changing the channel's fundamental semantics.

**Channel as fd**: Reuses the existing fd_table, close, dup2, CLOEXEC, and
cleanup-on-exit infrastructure.  No new kernel handle namespace.

---

## Completion Port Integration

IPC channels can be multiplexed with other async I/O sources (IRQs, timers,
file reads) via the completion port system.

### OP_IPC_SEND (opcode 5)

Submit an IPC send as an async operation via `io_submit`.  The message is
read from user memory at submission time.  If the channel can accept it
immediately, a completion is posted right away.  Otherwise the message is
stored and the completion fires when a receiver drains space.

**Submission fields**:

| Field     | Value |
|-----------|-------|
| `opcode`  | 5 (OP_IPC_SEND) |
| `fd`      | Channel send-end file descriptor |
| `buf_addr`| Pointer to user `struct ipc_message` to send |
| `user_data`| User-defined tag (returned in completion) |

**Completion fields**:

| Field      | Value |
|------------|-------|
| `opcode`   | 5 (OP_IPC_SEND) |
| `result`   | 0 on success, `-EPIPE` if receive end closed |
| `user_data`| Same as submission |

### OP_IPC_RECV (opcode 6)

Submit an IPC receive as an async operation via `io_submit`.  When a message
arrives on the channel, a completion is posted to the port with the message
copied to the user-provided buffer.

**Submission fields**:

| Field     | Value |
|-----------|-------|
| `opcode`  | 6 (OP_IPC_RECV) |
| `fd`      | Channel receive-end file descriptor |
| `buf_addr`| Pointer to user `struct ipc_message` buffer |
| `user_data`| User-defined tag (returned in completion) |

**Completion fields**:

| Field      | Value |
|------------|-------|
| `opcode`   | 6 (OP_IPC_RECV) |
| `result`   | 0 on success, `-EPIPE` if send end closed |
| `user_data`| Same as submission |

The message is copied to `buf_addr` by `io_wait` (same mechanism as OP_READ).

**Semantics**: Both operations are one-shot, like OP_IRQ_WAIT.  Each
submission handles exactly one message.  Re-submit after each completion
for continuous send/receive.

### Example: event loop with IPC + timer

```c
int port = io_create(0);
int fds[2];
ipc_create(fds, 4, 0);

struct ipc_message recv_buf;
struct io_submission subs[2] = {
    { .opcode = 6 /* OP_IPC_RECV */, .fd = fds[1],
      .buf_addr = (uint64_t)&recv_buf, .user_data = 1 },
    { .opcode = 1 /* OP_TIMEOUT */, .timeout_ns = 1000000000, .user_data = 2 },
};
io_submit(port, subs, 2);

struct io_completion comp;
io_wait(port, &comp, 1, 1, 0);
if (comp.user_data == 1) {
    /* IPC message received in recv_buf */
} else {
    /* timer fired */
}
```

### Kernel internals

When `io_submit` processes OP_IPC_RECV, it calls `arm_recv()`:

1. If a message is already in the queue, posts a completion immediately
2. Otherwise, registers the port on the channel (`pending_port` field)
3. When a future `ipc_send` deposits a message, `try_send` detects the
   armed port and returns `SendAction::PostToPort` — the caller serializes
   the message and posts it to the port after releasing the channel lock

When `io_submit` processes OP_IPC_SEND, it calls `arm_send()`:

1. If the channel can accept the message (queue not full, or receiver waiting),
   delivers it and posts a success completion immediately
2. Otherwise, stores the port + message in `pending_send_port`
3. When a future `ipc_recv` drains space, `try_recv` detects the armed send
   port and returns `RecvAction::MessageAndNotifySendPort` — the caller posts
   a success completion to the send port

Lock ordering: channel lock is always acquired before port lock (never
reversed), preventing deadlocks.

---

## Future Extensions

- **ipc_call(fd, send_msg, recv_msg)** -- atomic send+recv for RPC
- **Bidirectional channels** -- two queues in one object
- ~~**fd-passing in messages**~~ -- implemented: `fds[4]` in IpcMessage, kernel transfers fd objects between processes
- **Notification objects** -- seL4-style bitmask signaling
