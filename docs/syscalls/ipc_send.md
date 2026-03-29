# ipc_send (nr 506)

Send a message on an IPC channel (blocking).

## Signature

```
ipc_send(fd: i32, msg_ptr: *const IpcMessage, flags: u32) → 0 or -errno
```

## Arguments

| Arg | Register | Description |
|-----|----------|-------------|
| fd | rdi | Channel send-end fd (from `ipc_create`) |
| msg_ptr | rsi | Pointer to `IpcMessage` to send |
| flags | rdx | `IPC_NONBLOCK` (0x1) for non-blocking mode |

## Return value

On success, returns 0.

## Errors

| Error | Condition |
|-------|-----------|
| EFAULT | `msg_ptr` is invalid |
| EBADF | `fd` is not a valid channel send-end |
| EPIPE | Receive end has been closed |
| EAGAIN | `IPC_NONBLOCK` set and channel is full |
| EMFILE | (receiver) fd table full during fd transfer |

## Description

Sends a message through the channel.  If the channel buffer is full and
`IPC_NONBLOCK` is not set, the calling thread blocks until the receiver
drains space.

If `msg.fds` contains valid file descriptors (not -1), those fd objects are
extracted from the sender's fd table and transferred to the receiver.  The
sender's fds remain open — this is a dup, not a move.

When a receiver is blocked waiting via `ipc_recv`, the send uses scheduler
donate to directly switch to the receiver thread for low-latency delivery.

For async (non-blocking, multiplexed) sending, use `OP_IPC_SEND` via
`io_submit` instead.

## Implementation

`osl/src/ipc.rs` — `sys_ipc_send`

## See also

- [ipc_create (505)](ipc_create.md)
- [ipc_recv (507)](ipc_recv.md)
- [io_submit (502)](io_submit.md) — `OP_IPC_SEND` for async mode
