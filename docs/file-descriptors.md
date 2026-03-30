# File Descriptors & Pipes

Design for per-process file descriptor tables, the `FileHandle` trait,
blocking syscalls, and the `pipe` implementation.

---

## Motivation

The kernel currently has three syscalls: `write` (hardcoded to stdout/stderr
via `crate::print!()`), `exit`, and `arch_prctl`.  There is no concept of a
file descriptor, no `read`/`close`, and no IPC mechanism between user
processes.

Adding a proper file descriptor layer enables:

- `pipe` for parent→child / sibling IPC
- Redirecting stdout/stderr to pipes (shell pipelines)
- Future `open`/`read`/`write`/`close` for VFS-backed files
- `dup2` for fd redirection

---

## Overview

```
  User process                      Kernel
  ─────────────                     ──────
  write(fd, buf, n)  ──syscall──►  fd_table[fd].write(buf)
                                       │
                         ┌─────────────┼──────────────┐
                         ▼             ▼              ▼
                    ConsoleHandle  PipeWriter     VfsHandle  SharedMem
                    → print!()     → PipeInner    → VFS     → shmem frames
                                       ▲
                         ┌─────────────┘
                         │
  read(fd, buf, n)  ──►  fd_table[fd].read(buf)
                              │
                         PipeReader
                         → PipeInner
```

---

## Layer 1: FileHandle trait

```rust
/// A kernel object backing an open file descriptor.
///
/// Implementations must be safe to share across threads (the fd table
/// holds `Arc<dyn FileHandle>`).
pub trait FileHandle: Send + Sync {
    /// Read up to `buf.len()` bytes.  Returns the number of bytes read,
    /// or 0 for EOF.  May block the calling thread (see "Blocking" below).
    fn read(&self, buf: &mut [u8]) -> Result<usize, FileError>;

    /// Write up to `buf.len()` bytes.  Returns the number of bytes written.
    /// May block the calling thread.
    fn write(&self, buf: &[u8]) -> Result<usize, FileError>;

    /// Release resources associated with this handle.
    /// Called when the last `Arc` is dropped (i.e. last fd closed).
    fn close(&self) {}

    /// Return a name for downcasting purposes.
    fn kind(&self) -> &'static str;

    /// For directory handles: serialize entries as linux_dirent64 into buf.
    fn getdents64(&self, _buf: &mut [u8]) -> Result<usize, FileError> {
        Err(FileError::NotATty)
    }
}
```

`FileError` is a structured enum in `libkernel::file` (using snafu for Display):

```rust
#[derive(Debug, Clone, Copy, Snafu)]
pub enum FileError {
    BadFd,           // bad file descriptor
    IsDirectory,     // is a directory
    NotATty,         // inappropriate ioctl for device
    TooManyOpenFiles, // too many open files
}
```

Linux errno numeric codes are defined separately in `osl::errno` and converted
from `FileError` via `errno::file_errno()`.  This keeps `libkernel` free of
Linux-specific numeric constants.

`FileHandle::read`/`write` are **synchronous** — they return when the
operation completes or an error occurs.  Blocking is handled at the
scheduler level (see below), not via async/await.

---

## Layer 2: Per-process fd table

Add to `Process`:

```rust
pub struct Process {
    // ... existing fields ...
    pub fd_table: Vec<Option<Arc<dyn FileHandle>>>,
}
```

On process creation, pre-populate fds 0–2:

```rust
fd_table: vec![
    Some(Arc::new(ConsoleHandle)),  // 0: stdin  (read returns EBADF for now)
    Some(Arc::new(ConsoleHandle)),  // 1: stdout
    Some(Arc::new(ConsoleHandle)),  // 2: stderr
],
```

Fd allocation: scan for the first `None` slot; if none, push a new entry.
This matches the POSIX "lowest available fd" rule.

```rust
impl Process {
    pub fn alloc_fd(&mut self, handle: Arc<dyn FileHandle>) -> Result<usize, FileError> {
        for (i, slot) in self.fd_table.iter().enumerate() {
            if slot.is_none() {
                self.fd_table[i] = Some(handle);
                return Ok(i);
            }
        }
        if self.fd_table.len() < MAX_FDS {
            let fd = self.fd_table.len();
            self.fd_table.push(Some(handle));
            Ok(fd)
        } else {
            Err(FileError::TooManyOpenFiles)
        }
    }

    pub fn close_fd(&mut self, fd: usize) -> Result<(), FileError> {
        if fd >= self.fd_table.len() {
            return Err(FileError::BadFd);
        }
        match self.fd_table[fd].take() {
            Some(handle) => { handle.close(); Ok(()) }
            None => Err(FileError::BadFd),
        }
    }

    pub fn get_fd(&self, fd: usize) -> Result<Arc<dyn FileHandle>, FileError> {
        self.fd_table.get(fd)
            .and_then(|slot| slot.clone())
            .ok_or(FileError::BadFd)
    }
}
```

---

## Layer 3: ConsoleHandle

The simplest `FileHandle` — wraps the existing `crate::print!()` behaviour:

```rust
pub struct ConsoleHandle {
    pub readable: bool,
}

impl FileHandle for ConsoleHandle {
    fn read(&self, buf: &mut [u8]) -> Result<usize, FileError> {
        if !self.readable {
            return Err(FileError::BadFd);
        }
        Ok(crate::console::read_input(buf))
    }

    fn write(&self, buf: &[u8]) -> Result<usize, FileError> {
        if let Ok(s) = core::str::from_utf8(buf) {
            crate::print!("{}", s);
        }
        Ok(buf.len())
    }

    fn kind(&self) -> &'static str { "console" }
}
```

---

## Layer 4: Blocking syscalls (Option C)

Pipe `read` and `write` must block when the buffer is empty or full.
Rather than adding async/await to the syscall path, we add a `Blocked`
state to the scheduler.

### New thread state

```rust
enum ThreadState {
    Ready,
    Running,
    Blocked,   // ← new
    Dead,
}
```

### Blocking API

```rust
/// Block the current thread until `waker` is called.
///
/// Saves the current thread's state as `Blocked` and yields to the
/// scheduler.  Returns when another thread (or ISR) calls
/// `unblock(thread_idx)`.
///
/// Must be called with interrupts disabled.
pub fn block_current_thread() { ... }

/// Move a blocked thread back onto the ready queue.
///
/// Safe to call from ISR context (e.g. a pipe write that wakes a reader).
pub fn unblock(thread_idx: usize) { ... }
```

### How blocking works

1. Syscall handler (e.g. `sys_read` on an empty pipe) calls
   `block_current_thread()`.
2. The scheduler marks the thread `Blocked` and context-switches away.
3. `preempt_tick` never re-queues `Blocked` threads.
4. When the condition is met (e.g. a writer pushes data into the pipe),
   the pipe calls `unblock(thread_idx)`.
5. `unblock` sets the thread to `Ready` and pushes it onto the ready queue.
6. On the next preemption the thread is scheduled, returns from
   `block_current_thread`, and the syscall retries the operation.

### Avoiding lost wakeups

The pipe must check the condition and call `block_current_thread` while
holding the pipe's internal lock.  The sequence is:

```
lock pipe
if buffer_empty:
    register self as waiter (store thread_idx)
    unlock pipe
    block_current_thread()       ← yields here
    goto top                     ← retry after wakeup
else:
    copy data
    wake writer if blocked
    unlock pipe
    return count
```

The critical property: between checking the condition and blocking, no
writer can sneak in — the pipe lock is held.  The writer will see the
registered waiter and call `unblock` after releasing the lock.

---

## Layer 5: Pipe

### Shared state

```rust
struct PipeInner {
    buf: VecDeque<u8>,
    capacity: usize,               // default 4096
    reader_closed: bool,
    writer_closed: bool,
    blocked_reader: Option<usize>,  // thread_idx waiting for data
    blocked_writer: Option<usize>,  // thread_idx waiting for space
}

pub struct Pipe {
    inner: Mutex<PipeInner>,
}
```

### Read end

```rust
pub struct PipeReader(Arc<Pipe>);

impl FileHandle for PipeReader {
    fn read(&self, buf: &mut [u8]) -> Result<usize, FileError> {
        loop {
            let mut inner = self.0.inner.lock();
            if !inner.buf.is_empty() {
                let n = inner.drain_to(buf);
                // Wake blocked writer if there's now space.
                if let Some(writer) = inner.blocked_writer.take() {
                    scheduler::unblock(writer);
                }
                return Ok(n);
            }
            if inner.writer_closed {
                return Ok(0); // EOF
            }
            // Buffer empty, writer alive — block.
            inner.blocked_reader = Some(scheduler::current_thread_idx());
            drop(inner);
            scheduler::block_current_thread();
            // Woken up — retry.
        }
    }

    fn write(&self, _buf: &[u8]) -> Result<usize, FileError> {
        Err(FileError::EBADF)
    }

    fn close(&self) {
        let mut inner = self.0.inner.lock();
        inner.reader_closed = true;
        // Wake blocked writer so it sees EPIPE.
        if let Some(writer) = inner.blocked_writer.take() {
            scheduler::unblock(writer);
        }
    }
}
```

### Write end

```rust
pub struct PipeWriter(Arc<Pipe>);

impl FileHandle for PipeWriter {
    fn read(&self, _buf: &mut [u8]) -> Result<usize, FileError> {
        Err(FileError::EBADF)
    }

    fn write(&self, buf: &[u8]) -> Result<usize, FileError> {
        let mut offset = 0;
        while offset < buf.len() {
            let mut inner = self.0.inner.lock();
            if inner.reader_closed {
                return Err(FileError::EPIPE);
            }
            let space = inner.capacity - inner.buf.len();
            if space > 0 {
                let n = core::cmp::min(space, buf.len() - offset);
                inner.buf.extend(&buf[offset..offset + n]);
                offset += n;
                // Wake blocked reader.
                if let Some(reader) = inner.blocked_reader.take() {
                    scheduler::unblock(reader);
                }
            } else {
                // Buffer full — block.
                inner.blocked_writer = Some(scheduler::current_thread_idx());
                drop(inner);
                scheduler::block_current_thread();
            }
        }
        Ok(buf.len())
    }

    fn close(&self) {
        let mut inner = self.0.inner.lock();
        inner.writer_closed = true;
        // Wake blocked reader so it sees EOF.
        if let Some(reader) = inner.blocked_reader.take() {
            scheduler::unblock(reader);
        }
    }
}
```

### Creating a pipe

```rust
pub fn new_pipe(capacity: usize) -> (PipeReader, PipeWriter) {
    let pipe = Arc::new(Pipe {
        inner: Mutex::new(PipeInner {
            buf: VecDeque::with_capacity(capacity),
            capacity,
            reader_closed: false,
            writer_closed: false,
            blocked_reader: None,
            blocked_writer: None,
        }),
    });
    (PipeReader(pipe.clone()), PipeWriter(pipe))
}
```

---

## Layer 6: Syscall wiring

### New syscalls

| Nr  | Name    | Signature                          |
|-----|---------|------------------------------------|
| 0   | `read`  | `read(fd, buf, count) → ssize_t`  |
| 1   | `write` | `write(fd, buf, count) → ssize_t` |
| 3   | `close` | `close(fd) → int`                 |
| 22  | `pipe`  | `pipe(fds) → int`                 |

### `sys_pipe` implementation

```rust
fn sys_pipe(fds_ptr: u64) -> i64 {
    // Validate user pointer (2 × i32 = 8 bytes).
    const USER_LIMIT: u64 = 0x0000_8000_0000_0000;
    if fds_ptr == 0 || fds_ptr + 8 > USER_LIMIT {
        return FileError::EFAULT.0;
    }

    let (reader, writer) = new_pipe(4096);
    let pid = process::current_pid();
    let (read_fd, write_fd) = process::with_process(pid, |proc| {
        let rfd = proc.alloc_fd(Arc::new(reader))?;
        match proc.alloc_fd(Arc::new(writer)) {
            Ok(wfd) => Ok((rfd, wfd)),
            Err(e) => { proc.close_fd(rfd).ok(); Err(e) }
        }
    }).unwrap_or(Err(FileError::EBADF))?;

    // Write fds to user space.
    let fds = fds_ptr as *mut [i32; 2];
    unsafe { (*fds) = [read_fd as i32, write_fd as i32]; }
    0
}
```

### Refactored `sys_write`

```rust
fn sys_write(fd: u64, buf: u64, count: u64) -> i64 {
    // ... existing user pointer validation ...
    let bytes = validated_user_slice(buf, count)?;
    let pid = process::current_pid();
    let handle = process::with_process_ref(pid, |p| {
        p.fd_table.get(fd as usize).and_then(|s| s.clone())
    }).flatten().ok_or(FileError::EBADF)?;
    match handle.write(bytes) {
        Ok(n) => n as i64,
        Err(e) => e.0,
    }
}
```

---

## Implementation order

| Phase | What                                        | Files                          |
|-------|---------------------------------------------|--------------------------------|
| 1     | `FileHandle` trait + `FileError`         | `libkernel/src/file.rs` (new)  |
| 2     | `fd_table` on `Process` + `alloc_fd`/`close_fd` | `libkernel/src/process.rs` |
| 3     | `ConsoleHandle`                             | `libkernel/src/file.rs`        |
| 4     | Refactor `sys_write` to use fd table        | `libkernel/src/syscall.rs`     |
| 5     | Add `sys_read` + `sys_close`                | `libkernel/src/syscall.rs`     |
| 6     | `Blocked` thread state + `block_current_thread` / `unblock` | `libkernel/src/task/scheduler.rs` |
| 7     | `PipeInner` / `PipeReader` / `PipeWriter`   | `libkernel/src/pipe.rs` (new)  |
| 8     | `sys_pipe` syscall                          | `libkernel/src/syscall.rs`     |
| 9     | `dup2` (optional, for shell redirection)    | `libkernel/src/syscall.rs`     |

Phases 1–5 are useful independently — they give user processes a real fd
abstraction for stdout/stderr.  Phase 6 is needed for any future blocking
syscall (futex, sleep, waitpid).  Phases 7–8 deliver pipes.

---

## Open questions

- **Pipe capacity**: 4096 bytes matches Linux's historical default.  Should
  this be page-sized for alignment, or is `VecDeque` fine?
- **Multiple readers/writers**: This design supports only one blocked reader
  and one blocked writer.  For a single pipe between two processes this is
  fine, but `dup`-ed fds sharing a pipe end would need a wait queue.
- **Signal delivery**: POSIX `SIGPIPE` on write to a broken pipe is not
  modelled — we return `EPIPE` instead.  Signals can be added later.
- **`O_NONBLOCK`**: Not yet supported.  Would return `EAGAIN` instead of
  blocking.  Requires fd-level flags.
- **VFS integration**: A future `VfsHandle` implementing `FileHandle` would
  connect the VFS's async `read_file` to the synchronous `FileHandle::read`
  by using the same blocking mechanism.
