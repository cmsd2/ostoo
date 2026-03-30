# shmem_create (nr 508)

Create a shared memory object and return a file descriptor.

## Signature

```
shmem_create(size: u64, flags: u32) → fd or -errno
```

## Arguments

| Arg   | Register | Description |
|-------|----------|-------------|
| size  | rdi      | Size of the shared memory object in bytes (must be > 0) |
| flags | rsi      | Flags: `SHM_CLOEXEC` (0x01) sets close-on-exec on the fd |

## Return value

On success, returns a file descriptor for the shared memory object.

## Errors

| Error   | Condition |
|---------|-----------|
| EINVAL  | `size` is 0, or unknown flags are set |
| ENOMEM  | Not enough physical memory to allocate the backing frames |
| EMFILE  | Process fd table is full |

## Description

Allocates a shared memory object backed by eagerly-allocated, zeroed
physical frames.  Returns a file descriptor referring to it.

The fd can be inherited by child processes (via `clone` + `execve`, unless
`SHM_CLOEXEC` is set) or transferred via IPC fd-passing (`ipc_send` /
`ipc_recv`).  Both sides can then call `mmap(MAP_SHARED, fd)` to map the
same physical pages into their address spaces.

Physical frames are reference-counted.  A frame is freed only when all
mappings are removed **and** the last fd referring to the shared memory
object is closed.

### Flags

| Flag | Value | Description |
|------|-------|-------------|
| `SHM_CLOEXEC` | 0x01 | Set close-on-exec on the returned fd (analogous to Linux's `MFD_CLOEXEC`) |

### Userspace usage (C)

```c
#define SYS_SHMEM_CREATE 508
#define SHM_CLOEXEC      0x01

static long shmem_create(unsigned long size, unsigned int flags) {
    return syscall(SYS_SHMEM_CREATE, size, flags);
}

/* Create 4 KiB shared memory, mmap it */
int fd = shmem_create(4096, 0);
void *ptr = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
```

## Implementation

`osl/src/syscalls/shmem.rs` — `sys_shmem_create`

Backing struct: `libkernel/src/shmem.rs` — `SharedMemInner`

## See also

- [mmap (9)](mmap.md) — `MAP_SHARED` with a shmem fd
- [ipc_send (506)](ipc_send.md) — fd-passing for capability transfer
- [mmap Design](../mmap-design.md) — Phase 5b: anonymous shared memory
