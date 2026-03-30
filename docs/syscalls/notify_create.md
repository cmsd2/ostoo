# notify_create (nr 509)

Create a notification file descriptor for inter-process signaling.

## Signature

```
notify_create(flags: u32) → fd or -errno
```

## Arguments

| Arg   | Register | Description |
|-------|----------|-------------|
| flags | rdi      | Flags: `NOTIFY_CLOEXEC` (0x01) sets close-on-exec on the fd |

## Return value

On success, returns a file descriptor for the notification object.

## Errors

| Error  | Condition |
|--------|-----------|
| EINVAL | Unknown flags are set |
| EMFILE | Process fd table is full |

## Description

Creates a notification fd for signaling between processes.  The fd is
used with two operations:

- **Consumer**: submits `OP_RING_WAIT` (opcode 7) via `io_submit` on
  the notification fd.  The completion port blocks until the producer
  signals.
- **Producer**: calls `notify(fd)` (syscall 510) to signal.  If an
  `OP_RING_WAIT` is armed, a completion is posted to the consumer's port.

The notification fd can be passed to child processes via inheritance
(clone + execve) or via IPC fd-passing (`ipc_send` / `ipc_recv`).

### Semantics

- **Edge-triggered, one-shot**: one `notify()` produces one completion.
  The consumer must re-submit `OP_RING_WAIT` to receive the next signal.
- **Buffered**: if `notify()` is called before `OP_RING_WAIT` is armed,
  the notification is buffered.  The next `OP_RING_WAIT` completes
  immediately.  Multiple pre-arm signals coalesce into one.
- **Single waiter**: only one `OP_RING_WAIT` can be pending per fd.

### Flags

| Flag | Value | Description |
|------|-------|-------------|
| `NOTIFY_CLOEXEC` | 0x01 | Set close-on-exec on the returned fd |

### Userspace usage (C)

```c
#define SYS_NOTIFY_CREATE 509
#define NOTIFY_CLOEXEC    0x01

static long notify_create(unsigned int flags) {
    return syscall(SYS_NOTIFY_CREATE, flags);
}

int nfd = notify_create(0);
```

## Implementation

`osl/src/notify.rs` — `sys_notify_create`

Backing struct: `libkernel/src/notify.rs` — `NotifyInner`

## See also

- [notify (510)](notify.md) — signal the notification fd
- [io_submit (502)](io_submit.md) — `OP_RING_WAIT` opcode
- [Completion Port Design](../completion-port-design.md) — Phase 4
