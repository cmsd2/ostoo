# io_create (nr 501)

Create a completion port for async I/O.

## Signature

```
io_create(flags: u32) → fd or -errno
```

## Arguments

| Arg | Register | Description |
|-----|----------|-------------|
| flags | rdi | Reserved, must be 0 |

## Return value

On success, returns a file descriptor for the new completion port.

## Errors

| Error | Condition |
|-------|-----------|
| EINVAL | `flags` is non-zero |
| EMFILE | Process fd table is full |

## Description

Creates a new kernel `CompletionPort` object and returns a file descriptor
referring to it.  The port fd is used as the first argument to `io_submit`
and `io_wait`.

Completion ports are the core async I/O primitive in ostoo.  Operations
(reads, writes, timeouts, IRQ waits, IPC send/recv) are submitted to a port
via `io_submit` and their completions are harvested via `io_wait`.

## Implementation

`osl/src/io_port.rs` — `sys_io_create`

## See also

- [io_submit (502)](io_submit.md)
- [io_wait (503)](io_wait.md)
- [Completion Port Design](../completion-port-design.md)
