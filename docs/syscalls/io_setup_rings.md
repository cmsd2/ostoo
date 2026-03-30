# io_setup_rings (nr 511)

Set up shared-memory submission and completion rings on a completion port.

## Signature

```
io_setup_rings(port_fd: i32, params: *mut IoRingParams) → 0 or -errno
```

## Arguments

| Arg      | Register | Description |
|----------|----------|-------------|
| port_fd  | rdi      | Completion port fd (from `io_create`) |
| params   | rsi      | Pointer to `IoRingParams` struct (in/out) |

### IoRingParams struct

```c
struct io_ring_params {
    uint32_t sq_entries;   /* IN: requested SQ size (rounded to pow2, max 64) */
    uint32_t cq_entries;   /* IN: requested CQ size (rounded to pow2, max 128) */
    int32_t  sq_fd;        /* OUT: shmem fd for SQ ring page */
    int32_t  cq_fd;        /* OUT: shmem fd for CQ ring page */
};
```

## Return value

Returns 0 on success.  `params->sq_entries` and `params->cq_entries` are
updated to the actual (rounded) sizes.  `params->sq_fd` and `params->cq_fd`
are set to new shmem fds.

## Errors

| Error  | Condition |
|--------|-----------|
| EFAULT | `params` pointer is invalid |
| EBADF  | `port_fd` is invalid or not a completion port |
| EBUSY  | Ring already set up on this port |
| ENOMEM | Could not allocate ring pages |
| EMFILE | fd table full |

## Description

Transitions a completion port into **ring mode**.  After this call:

- `io_submit` still works (completions go to the CQ ring)
- `io_wait` returns `-EINVAL` (replaced by `io_ring_enter`)
- Completions are posted to the shared CQ ring, readable by userspace
  without a syscall

The caller must `mmap(MAP_SHARED)` both the SQ and CQ fds to access the
ring buffers.  Each ring is a single 4 KiB page with the layout:

```
Offset 0:  RingHeader (16 bytes)
  u32 head      — consumer advances
  u32 tail      — producer advances
  u32 mask      — capacity - 1
  u32 flags     — reserved (0)

Offset 64: entries[] (cache-line aligned)
  SQ: IoSubmission[sq_entries]   — 48 bytes each
  CQ: IoCompletion[cq_entries]   — 24 bytes each
```

Head and tail are accessed with atomic load/store operations with
acquire/release ordering.

### Capacity limits

| Ring | Entry size | Max entries | Calculation |
|------|-----------|-------------|-------------|
| SQ   | 48 bytes  | 64          | (4096 - 64) / 48 rounded to pow2 |
| CQ   | 24 bytes  | 128         | (4096 - 64) / 24 rounded to pow2 |

### Userspace usage (C)

```c
#define SYS_IO_SETUP_RINGS 511

static long io_setup_rings(int port_fd, struct io_ring_params *p) {
    return syscall(SYS_IO_SETUP_RINGS, port_fd, p);
}

int port = io_create(0);
struct io_ring_params params = { .sq_entries = 64, .cq_entries = 128 };
io_setup_rings(port, &params);

void *sq = mmap(NULL, 4096, PROT_READ|PROT_WRITE, MAP_SHARED, params.sq_fd, 0);
void *cq = mmap(NULL, 4096, PROT_READ|PROT_WRITE, MAP_SHARED, params.cq_fd, 0);
```

## Implementation

`osl/src/io_port.rs` — `sys_io_setup_rings`

## See also

- [io_create (501)](io_create.md) — create the completion port
- [io_ring_enter (512)](io_ring_enter.md) — process SQ entries and wait for CQ entries
- [Completion Port Design](../completion-port-design.md) — Phase 5
