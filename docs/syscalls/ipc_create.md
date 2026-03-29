# ipc_create (nr 505)

Create a bidirectional IPC channel pair.

## Signature

```
ipc_create(fds_ptr: *mut [i32; 2], capacity: u32, flags: u32) → 0 or -errno
```

## Arguments

| Arg | Register | Description |
|-----|----------|-------------|
| fds_ptr | rdi | Pointer to 2-element i32 array for [send_fd, recv_fd] |
| capacity | rsi | Channel buffer capacity (max queued messages) |
| flags | rdx | `IPC_CLOEXEC` (0x1) to set FD_CLOEXEC on both fds |

## Return value

On success, writes `[send_fd, recv_fd]` to `fds_ptr` and returns 0.

## Errors

| Error | Condition |
|-------|-----------|
| EFAULT | `fds_ptr` is invalid |
| EINVAL | Unknown flags |
| EMFILE | Process fd table is full |

## Description

Creates an IPC channel and returns two file descriptors: a send end and a
receive end.  Messages are fixed-size `IpcMessage` structs (56 bytes)
containing:

```c
struct IpcMessage {         // repr(C)
    uint64_t tag;           // User-defined message type
    uint64_t data[3];       // 24 bytes inline payload
    int32_t  fds[4];        // File descriptors to transfer (-1 = unused)
};
```

The channel supports capability-based fd passing: file descriptors listed in
`msg.fds` are duplicated from the sender's fd table and installed in the
receiver's fd table on delivery.

Channels can be used in both blocking mode (via `ipc_send`/`ipc_recv`
syscalls) and async mode (via `OP_IPC_SEND`/`OP_IPC_RECV` on a completion
port).

## Implementation

`osl/src/ipc.rs` — `sys_ipc_create`

## See also

- [ipc_send (506)](ipc_send.md)
- [ipc_recv (507)](ipc_recv.md)
- [IPC Channels](../ipc-channels.md)
