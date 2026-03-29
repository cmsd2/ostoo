# kill (nr 62)

Send a signal to a process.

## Signature

```
kill(pid: pid_t, sig: int) → 0 or -errno
```

## Arguments

| Arg | Register | Description |
|-----|----------|-------------|
| pid | rdi | Target process ID |
| sig | rsi | Signal number (1–31) |

## Return value

Returns 0 on success.

## Errors

| Error | Condition |
|-------|-----------|
| EINVAL | Signal number is out of range (< 1 or > 31) |
| ESRCH | No process with the given PID exists |

## Description

Queues the specified signal on the target process.  The signal is delivered
before the process next returns to user space (checked after syscalls and
interrupts).

Currently only supports sending to a specific PID.  Negative PIDs (process
groups) and PID 0 (current process group) are not yet supported.

## Implementation

`osl/src/signal.rs` — `sys_kill`

## See also

- [rt_sigaction (13)](rt_sigaction.md)
- [Signal Support](../signals.md)
