# io_ring_enter (nr 512)

Process SQ entries and optionally wait for CQ completions on a ring-mode
completion port.

## Signature

```
io_ring_enter(port_fd: i32, to_submit: u32, min_complete: u32, flags: u32) → i64
```

## Arguments

| Arg          | Register | Description |
|--------------|----------|-------------|
| port_fd      | rdi      | Completion port fd (must be in ring mode) |
| to_submit    | rsi      | Max number of SQ entries to process |
| min_complete | rdx      | Min CQ entries to wait for before returning |
| flags        | r10      | Reserved, must be 0 |

## Return value

On success, returns the number of CQ entries available (tail - head).

## Errors

| Error  | Condition |
|--------|-----------|
| EBADF  | `port_fd` is invalid or not a completion port |
| EINVAL | Port is not in ring mode, or `flags != 0` |

## Description

This is the single syscall for ring-mode operation, replacing both
`io_submit` and `io_wait`.

### Processing phases

1. **Drain SQ**: reads up to `to_submit` entries from the shared SQ ring
   (from the kernel's SQ head to the userspace-written SQ tail).  Each
   SQE is processed identically to `io_submit`.  The SQ head is advanced.

2. **Flush deferred completions**: drains the kernel queue of completions
   that need syscall-context processing (OP_READ data copy, OP_IPC_RECV
   fd installation).  These are written to the CQ ring.

3. **Wait**: if `min_complete > 0`, blocks until the CQ ring has at least
   `min_complete` entries available.  On each wakeup, deferred completions
   are flushed again.

### Dual-mode completion posting

When rings are active, `CompletionPort::post()` routes completions:

- **Simple** (no read_buf, no transfer_fds): CQE written directly to
  the shared CQ ring.  This is the fast path for OP_NOP, OP_TIMEOUT,
  OP_WRITE, OP_IRQ_WAIT, OP_IPC_SEND, OP_RING_WAIT.
- **Deferred**: pushed to the kernel queue and flushed by `io_ring_enter`
  in syscall context.  This handles OP_READ and OP_IPC_RECV which need
  to copy data to user buffers.

### Userspace usage (C)

```c
#define SYS_IO_RING_ENTER 512

static long io_ring_enter(int port_fd, unsigned int to_submit,
                          unsigned int min_complete, unsigned int flags) {
    return syscall(SYS_IO_RING_ENTER, port_fd, to_submit, min_complete, flags);
}

/* Write SQE to SQ ring */
uint32_t tail = __atomic_load_n(&sqh->tail, __ATOMIC_RELAXED);
struct io_submission *sqe = sq_entry(sq, tail, sqh->mask);
sqe->opcode = OP_NOP;
sqe->user_data = 42;
__atomic_store_n(&sqh->tail, tail + 1, __ATOMIC_RELEASE);

/* Process 1 SQE, wait for 1 CQE */
io_ring_enter(port, 1, 1, 0);

/* Read CQE from CQ ring */
uint32_t head = __atomic_load_n(&cqh->head, __ATOMIC_RELAXED);
uint32_t cq_tail = __atomic_load_n(&cqh->tail, __ATOMIC_ACQUIRE);
if (head != cq_tail) {
    struct io_completion *cqe = cq_entry(cq, head, cqh->mask);
    /* process cqe */
    __atomic_store_n(&cqh->head, head + 1, __ATOMIC_RELEASE);
}
```

## Implementation

`osl/src/io_port.rs` — `sys_io_ring_enter`

## See also

- [io_setup_rings (511)](io_setup_rings.md) — set up the shared rings
- [io_create (501)](io_create.md) — create the completion port
- [io_submit (502)](io_submit.md) — legacy submission (still works in ring mode)
- [Completion Port Design](../completion-port-design.md) — Phase 5
