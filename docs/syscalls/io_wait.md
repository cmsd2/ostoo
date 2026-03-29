# io_wait (nr 503)

Wait for completions on a completion port.

## Signature

```
io_wait(port_fd: i32, completions_ptr: *mut IoCompletion, max: u32, min: u32, timeout_ns: u64) → count or -errno
```

## Arguments

| Arg | Register | Description |
|-----|----------|-------------|
| port_fd | rdi | Completion port fd |
| completions_ptr | rsi | User buffer for completion entries |
| max | rdx | Maximum completions to return |
| min | r10 | Minimum completions before returning (0 = non-blocking poll) |
| timeout_ns | r8 | Timeout in nanoseconds (0 = wait forever) |

## Completion entry layout

```c
struct IoCompletion {       // 24 bytes, repr(C)
    uint64_t user_data;     // Copied from submission
    int64_t  result;        // Bytes transferred, or negative errno
    uint32_t flags;         // Reserved
    uint32_t opcode;        // Operation that completed
};
```

## Return value

On success, returns the number of completions written to `completions_ptr`
(between 0 and `max`).

## Errors

| Error | Condition |
|-------|-----------|
| EFAULT | `completions_ptr` is invalid |
| EBADF | `port_fd` is not a valid completion port |

## Description

Blocks the calling thread until at least `min` completions are available on
the port, or the timeout expires.  Drains up to `max` completions and
copies them to user memory.

For **OP_READ** completions, the kernel buffer containing read data is
copied to the user-space destination address that was specified in the
original submission.

For **OP_IPC_RECV** completions, transferred file descriptors are installed
in the receiver's fd table and the `IpcMessage.fds` array is rewritten with
the new fd numbers before copying to user memory.

The timeout is implemented as a cancellable async timer task.  A timeout of
0 means wait forever (no timeout).

## Implementation

`osl/src/io_port.rs` — `sys_io_wait`

## See also

- [io_create (501)](io_create.md)
- [io_submit (502)](io_submit.md)
- [Completion Port Design](../completion-port-design.md)
