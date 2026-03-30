# notify (nr 510)

Signal a notification file descriptor.

## Signature

```
notify(fd: i32) → 0 or -errno
```

## Arguments

| Arg | Register | Description |
|-----|----------|-------------|
| fd  | rdi      | Notification fd (from `notify_create`) |

## Return value

Returns 0 on success.

## Errors

| Error | Condition |
|-------|-----------|
| EBADF | `fd` is invalid or does not refer to a notification object |

## Description

Signals a notification fd, waking a consumer waiting via `OP_RING_WAIT`
on a completion port.

If an `OP_RING_WAIT` is armed on the fd, a completion is posted to the
consumer's port with `result = 0` and `opcode = OP_RING_WAIT`.

If no `OP_RING_WAIT` is armed, the notification is buffered.  The next
`OP_RING_WAIT` submission will complete immediately.  Multiple buffered
notifications coalesce into one event.

The caller uses scheduler donate (`set_donate_target` + `yield_now`) for
low-latency wakeup of the consumer.

### Userspace usage (C)

```c
#define SYS_NOTIFY 510

static long notify(int fd) {
    return syscall(SYS_NOTIFY, fd);
}

notify(nfd);  /* wake consumer */
```

## Implementation

`osl/src/notify.rs` — `sys_notify`

## See also

- [notify_create (509)](notify_create.md) — create the notification fd
- [io_submit (502)](io_submit.md) — `OP_RING_WAIT` opcode
