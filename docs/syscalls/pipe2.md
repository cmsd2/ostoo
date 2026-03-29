# pipe (nr 22) / pipe2 (nr 293)

## Linux Signature

```c
int pipe(int pipefd[2]);
int pipe2(int pipefd[2], int flags);
```

## Description

Creates a unidirectional data channel (pipe). Returns two file descriptors: `pipefd[0]` for reading and `pipefd[1]` for writing.

Both syscalls share the same implementation: `pipe(fds)` is dispatched as
`pipe2(fds, 0)` (no flags).

## Current Implementation

1. **Create pipe:** Allocates a `PipeInner` (shared `VecDeque<u8>` buffer) wrapped in `PipeReader` and `PipeWriter` handles.
2. **Allocate fds:** Allocates two file descriptors in the process's fd table.
3. **Apply flags:** If `O_CLOEXEC` (0o2000000) is set, both fds get `FD_CLOEXEC` flag.
4. **Write to user buffer:** Writes `[read_fd, write_fd]` as two `i32` values to the user buffer.

### Pipe Semantics

- **Read:** If the buffer is empty and the writer is still open, the reader blocks via `block_current_thread()`. When the writer appends data, it wakes the blocked reader. Returns 0 (EOF) if the writer has been closed and the buffer is empty.
- **Write:** Appends data to the shared buffer and wakes any blocked reader. Currently unbounded (no backpressure).
- **Close:** Closing the write end sets `write_closed = true` and wakes any blocked reader (so it gets EOF). Closing the read end drops the reader's Arc reference.

**Source:** `osl/src/syscalls/fs.rs` — `sys_pipe2`, `osl/src/syscalls/mod.rs` — pipe(22) dispatch, `libkernel/src/file.rs` — `PipeReader`, `PipeWriter`, `make_pipe`

## Usage from C (musl)

```c
#include <unistd.h>
#include <fcntl.h>

int fds[2];
pipe2(fds, O_CLOEXEC);
write(fds[1], "hello", 5);
close(fds[1]);

char buf[32];
ssize_t n = read(fds[0], buf, sizeof(buf)); // n = 5
close(fds[0]);
```

## Errors

| Errno | Condition |
|-------|-----------|
| `-EFAULT` (-14) | Invalid `pipefd` pointer |
| `-EMFILE` (-24) | Per-process fd limit reached (64) |

## Future Work

- Bounded buffer with write-side blocking (backpressure).
- `O_NONBLOCK` flag support.
