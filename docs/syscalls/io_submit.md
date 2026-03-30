# io_submit (nr 502)

Submit async I/O operations to a completion port.

## Signature

```
io_submit(port_fd: i32, entries_ptr: *const IoSubmission, count: u32) → processed or -errno
```

## Arguments

| Arg | Register | Description |
|-----|----------|-------------|
| port_fd | rdi | Completion port fd (from `io_create`) |
| entries_ptr | rsi | Pointer to array of submission entries |
| count | rdx | Number of entries to submit |

## Submission entry layout

```c
struct IoSubmission {       // 48 bytes, repr(C)
    uint64_t user_data;     // Opaque value returned in completion
    uint32_t opcode;        // Operation type (see below)
    uint32_t flags;         // Reserved, must be 0
    int32_t  fd;            // Target file descriptor (opcode-dependent)
    int32_t  _pad;
    uint64_t buf_addr;      // User buffer address
    uint32_t buf_len;       // User buffer length
    uint32_t offset;        // Reserved
    uint64_t timeout_ns;    // Timeout in nanoseconds (OP_TIMEOUT)
};
```

## Opcodes

| Value | Name | Description |
|-------|------|-------------|
| 0 | OP_NOP | Immediate completion (testing/synchronization) |
| 1 | OP_TIMEOUT | Timer that completes after `timeout_ns` nanoseconds |
| 2 | OP_READ | Async read from `fd` into `buf_addr` |
| 3 | OP_WRITE | Async write from `buf_addr` to `fd` |
| 4 | OP_IRQ_WAIT | Wait for an interrupt on an IRQ fd |
| 5 | OP_IPC_SEND | Send an IPC message on a channel send-end fd |
| 6 | OP_IPC_RECV | Receive an IPC message on a channel recv-end fd |
| 7 | OP_RING_WAIT | Wait for a notification fd signal |

## Return value

On success, returns the number of entries processed.

## Errors

| Error | Condition |
|-------|-----------|
| EFAULT | `entries_ptr` is invalid |
| EBADF | `port_fd` is not a valid completion port |

Per-entry errors (EBADF, EFAULT, EINVAL) are reported via the completion
result field rather than failing the entire submission.

## Description

Each submission entry describes an async operation.  The kernel processes
entries sequentially, spawning async tasks for operations that cannot
complete immediately.  When an operation finishes, a completion entry is
posted to the port and can be harvested via `io_wait`.

For **OP_READ**, data is read into a kernel buffer and copied to user space
during `io_wait` (which runs in the process's syscall context with the
correct page tables).

For **OP_IPC_SEND/RECV**, `buf_addr` points to an `IpcMessage` struct.
File descriptors in `msg.fds` are transferred across the channel.

For **OP_RING_WAIT**, `fd` must be a notification fd (from `notify_create`).
The completion fires when another process calls `notify(fd)`.  Edge-
triggered, one-shot: re-submit to rearm.

## Implementation

`osl/src/io_port.rs` — `sys_io_submit`

## See also

- [io_create (501)](io_create.md)
- [io_wait (503)](io_wait.md)
- [Completion Port Design](../completion-port-design.md)
