# ipc_recv (nr 507)

Receive a message from an IPC channel (blocking).

## Signature

```
ipc_recv(fd: i32, msg_ptr: *mut IpcMessage, flags: u32) → 0 or -errno
```

## Arguments

| Arg | Register | Description |
|-----|----------|-------------|
| fd | rdi | Channel recv-end fd (from `ipc_create`) |
| msg_ptr | rsi | Pointer to `IpcMessage` buffer for received message |
| flags | rdx | `IPC_NONBLOCK` (0x1) for non-blocking mode |

## Return value

On success, writes the received message to `msg_ptr` and returns 0.

## Errors

| Error | Condition |
|-------|-----------|
| EFAULT | `msg_ptr` is invalid |
| EBADF | `fd` is not a valid channel recv-end |
| EPIPE | Send end has been closed and channel is empty |
| EAGAIN | `IPC_NONBLOCK` set and no message available |
| EMFILE | fd table full during fd transfer installation |

## Description

Receives a message from the channel.  If no message is available and
`IPC_NONBLOCK` is not set, the calling thread blocks until a sender posts a
message.

If the received message carries file descriptors (`msg.fds` entries != -1),
the transferred fd objects are installed in the receiver's fd table and the
`fds` array is rewritten with the new fd numbers before being copied to
user memory.

For async (non-blocking, multiplexed) receiving, use `OP_IPC_RECV` via
`io_submit` instead.

## Implementation

`osl/src/ipc.rs` — `sys_ipc_recv`

## See also

- [ipc_create (505)](ipc_create.md)
- [ipc_send (506)](ipc_send.md)
- [io_submit (502)](io_submit.md) — `OP_IPC_RECV` for async mode
