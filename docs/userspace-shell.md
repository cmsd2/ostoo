# Userspace Shell Design

## Status

**All phases are complete.**  The userspace shell runs as the primary user
interface on boot.  The kernel shell remains as a fallback when no `/shell`
binary is found on the filesystem.

## Context

The shell was migrated from a kernel actor (`kernel/src/shell.rs`) to a
ring-3 process — a C program (`user/shell.c`) compiled with musl that reads
raw keypresses from stdin, does its own line editing, and uses syscalls for
file I/O and process management.

**Scope decisions:**
- Raw keypresses to userspace (no kernel line editing for foreground user processes)
- Minimal commands: echo, ls, cat, pwd, cd, export, env, unset, pid, exit, help, and running programs by name
- Environment variables: shell maintains an env table, passes it to child processes via posix_spawn
- Kernel provides default environment on boot (PATH=/host/bin, HOME=/, TERM=dumb, SHELL=/bin/shell)
- Kernel shell kept as fallback (dormant when userspace shell is foreground)
- No pipes yet

---

## Phase 1: Scheduler Blocking Support  ✅ COMPLETE

**Goal:** Add `Blocked` thread state so threads can sleep waiting for I/O.

**File:** `libkernel/src/task/scheduler.rs`

1. Add `Blocked` to `ThreadState` enum (line 51)
2. Modify `preempt_tick` (lines 484, 495) — treat `Blocked` like `Dead`: skip quantum decrement, don't re-queue
3. Add `pub fn block_current_thread()` — marks current thread `Blocked`, spins on `enable_and_hlt` until rescheduled with non-Blocked state
4. Add `pub fn unblock(thread_idx: usize)` — sets thread to `Ready`, pushes onto ready queue (safe from any context including ISR)

**Key detail:** Blocking from within `syscall_dispatch` works because each user process has its own 64 KiB kernel stack (set via `PER_CPU.kernel_rsp` during context switch). The timer saves/restores the full register state, so when unblocked, execution resumes mid-syscall.

---

## Phase 2: File Descriptor Table  ✅ COMPLETE

**Goal:** Per-process FD table with `FileHandle` trait, refactor existing syscalls.

### 2a: FileHandle trait + ConsoleHandle

**File:** `libkernel/src/file.rs`

- `FileError` enum (BadFd, IsDirectory, NotATty, TooManyOpenFiles) — using snafu for Display
- `FileHandle` trait: `read(&self, buf) -> Result<usize, FileError>`, `write(&self, buf) -> Result<usize, FileError>`, `close(&self)`, `kind()`, `getdents64()`
- `ConsoleHandle { readable: bool }` — write prints to kernel console; read delegates to console input buffer
- Linux errno numeric constants live in `osl/src/errno.rs`; `libkernel` has no knowledge of errno numbers

### 2b: FD table on Process

**File:** `libkernel/src/process.rs`

- Add `fd_table: Vec<Option<Arc<dyn FileHandle>>>` to `Process`
- Initialize fds 0-2 as `ConsoleHandle` in `Process::new()`
- Add `alloc_fd(handle) -> Result<usize, FileError>` (scan for first `None` slot)
- Add `close_fd(fd: usize) -> Result<(), FileError>`
- Add `get_fd(fd: usize) -> Result<Arc<dyn FileHandle>, FileError>`

### 2c: Refactor syscalls to use FD table

**File:** `osl/src/syscalls/io.rs` and `osl/src/syscalls/fs.rs`

- `sys_write` / `sys_writev`: look up fd in process fd_table, call `handle.write()` (`osl/src/syscalls/io.rs`)
- `sys_read`: look up fd, call `handle.read()` (`osl/src/syscalls/io.rs`)
- `sys_close`: call `process.close_fd(fd)` (`osl/src/syscalls/fs.rs`)

---

## Phase 3: Console Input (Raw Keypresses)  ✅ COMPLETE

**Goal:** Route decoded keypresses to a buffer that `read(0)` consumes, with blocking.

### 3a: Console input buffer

**New file:** `libkernel/src/console.rs`

- `CONSOLE_INPUT: Mutex<ConsoleInner>` with `VecDeque<u8>` (256 bytes) and `blocked_reader: Option<usize>`
- `FOREGROUND_PID: AtomicU64` — PID of the process that receives keyboard input (0 = kernel)
- `push_input(byte)` — pushes to buffer, calls `scheduler::unblock()` if a reader is blocked
- `read_input(buf) -> usize` — drains buffer into `buf`; if empty, registers `blocked_reader` and calls `block_current_thread()`, retries on wake
- `set_foreground(pid)` / `foreground_pid() -> ProcessId`
- `flush_input()` — clear buffer on foreground change

### 3b: Wire ConsoleHandle::read to console buffer

**File:** `libkernel/src/file.rs`

- `ConsoleHandle::read()` calls `console::read_input(buf)` when `readable == true`

### 3c: Modify keyboard actor routing

**File:** `kernel/src/keyboard_actor.rs`

- At top of `on_key` handler: check `console::foreground_pid()`
- If non-kernel PID: convert `Key` to raw byte(s) and call `console::push_input()`:
  - `Key::Unicode(c)` → ASCII byte (if c.is_ascii())
  - Enter → `\n` (0x0A)
  - Backspace → `0x7F` (DEL)
  - Ctrl+C → `0x03`, Ctrl+D → `0x04`, Tab → `0x09`
  - Arrow keys → VT100 sequences (ESC `[` A/B/C/D) — optional for later
  - Return early (skip kernel line-editor)
- If kernel PID: existing line-editor behavior unchanged

---

## Phase 4: VFS Syscalls  ✅ COMPLETE

**Goal:** `open`, `read` (files), `close`, `getdents64` so userspace can read files and list directories.

### 4a: Async-to-sync bridge

**File:** `osl/src/blocking.rs`

```rust
pub fn blocking<T: Send + 'static>(future: impl Future<Output=T> + Send + 'static) -> T {
    let result = Arc::new(Mutex::new(None));
    let thread_idx = scheduler::current_thread_idx();
    let r = result.clone();
    executor::spawn(Task::new(async move {
        *r.lock() = Some(future.await);
        scheduler::unblock(thread_idx);
    }));
    scheduler::block_current_thread();
    result.lock().take().unwrap()
}
```

Spawns the async VFS operation as a kernel task, blocks the user thread, unblocks when complete.

### 4b: VfsHandle (buffered file)

**File:** `osl/src/file.rs`

- `VfsHandle` — holds `Vec<u8>` content + read position; entire file loaded at `open` time
- `DirHandle` — holds `Vec<VfsDirEntry>` listing + cursor; loaded at `open` time

### 4c: sys_open (syscall 2)

**File:** `osl/src/syscalls/fs.rs`

- Read null-terminated path from userspace, validate pointer
- Resolve path relative to process `cwd` (see Phase 5a)
- Use `osl::blocking::blocking()` to call `devices::vfs::read_file()` or `devices::vfs::list_dir()` (try file first, fall back to dir for `O_DIRECTORY`)
- Wrap in `VfsHandle` or `DirHandle`, allocate fd via `process.alloc_fd()`
- Return fd or -ENOENT

### 4d: sys_getdents64 (syscall 217)

**File:** `osl/src/syscalls/io.rs`

- Look up fd → must be `DirHandle`
- Serialize entries as `linux_dirent64` structs into user buffer (d_ino, d_off, d_reclen, d_type, d_name)
- Return total bytes written, or 0 at end

### 4e: Existing sys_read/sys_close already work via FD table (Phase 2c)

---

## Phase 5: Process Management Syscalls  ✅ COMPLETE

**Goal:** chdir/getcwd, process creation (clone+execve), waitpid.

### 5a: chdir / getcwd

**File:** `libkernel/src/process.rs` — add `cwd: String` to `Process`, default `"/"`

**File:** `osl/src/syscalls/fs.rs`
- `sys_chdir` (nr 80): validate path exists via `osl::blocking::blocking(devices::vfs::list_dir(path))`, update `process.cwd`
- `sys_getcwd` (nr 79): copy `process.cwd` to user buffer

### 5b: Process spawning (clone + execve)

Process creation uses standard Linux `clone(CLONE_VM|CLONE_VFORK)` + `execve`.
musl's `posix_spawn` and Rust's `std::process::Command` work unmodified.

See [clone](syscalls/clone.md) and [execve](syscalls/execve.md).

### 5c: spawn_process_full (kernel-side ELF spawning)

**File:** `osl/src/spawn.rs`

- `spawn_process_full` takes `elf_data`, `argv: &[&[u8]]`, `envp: &[&[u8]]`, and `parent_pid: ProcessId` params
- `build_initial_stack` writes argv strings + pointer array + argc (Linux x86_64 ABI)

**File:** `libkernel/src/process.rs`

- `parent_pid: ProcessId` on `Process`
- `wait_thread: Option<usize>` (thread to wake on child exit)
- `vfork_parent_thread: Option<usize>` (thread to unblock after execve)

### 5d: waitpid (syscall 61 / wait4)

**File:** `osl/src/syscalls/process.rs`

- `sys_waitpid(pid, status_ptr, options) -> pid`
- Find zombie child matching requested pid (or any child if pid == -1)
- If found: write exit status to userspace, reap, return child PID
- If not found: register `wait_thread` on parent, block, retry on wake

**File:** `libkernel/src/process.rs`

- `find_zombie_child(parent, target_pid) -> Option<(ProcessId, i32)>`
- In `sys_exit`: if exiting process has a parent with `wait_thread`, call `unblock()`
- Clear foreground to parent when child exits

---

## Phase 6: Userspace Shell Binary  ✅ COMPLETE

**Goal:** Write shell.c, compile with musl, deploy.

### 6a: shell.c

**New file:** `user/src/shell.c`

- **Line editor:** read char by char via `read(0, &c, 1)`, handle backspace (erase `\b \b`), Enter (dispatch), Ctrl+C (cancel line), Ctrl+D (exit on empty line)
- **Echo input:** shell echoes each typed character with `write(1, &c, 1)` since kernel delivers raw keypresses
- **Command dispatch:**
  - `echo <text>` — print args
  - `pwd` — `getcwd()` + print
  - `cd <path>` — `chdir()`
  - `ls [path]` — `open()` + `getdents64()` loop + `close()`
  - `cat <file>` — `open()` + `read()` loop + `close()`
  - `exit` — `_exit(0)`
  - Anything else — try `posix_spawn(cmd)` + `waitpid()`, print error if spawn fails
- **Process spawning:** uses `posix_spawn()` (musl's wrapper around `clone` + `execve`)

### 6b: Build

**File:** `user/Makefile` — builds `src/*.c` → `bin/` as static musl binaries.

### 6c: Deploy to disk image

Compiled `shell` binary is output to `user/bin/shell`; available in guest via 9p at `/host/bin/shell` or `/bin/shell` (fallback root mount).

### 6d: Auto-launch on boot

**File:** `kernel/src/main.rs`

- After VFS is mounted, spawn an async task that reads `/shell` from VFS and calls `spawn_process()`
- Set the spawned shell as the foreground process
- If `/shell` not found, fall back to kernel shell (log a message)

### 6e: Kernel shell fallback

Automatic via the keyboard routing in Phase 3c: when foreground PID is 0 (kernel), keys go to the kernel shell actor. When the userspace shell exits or crashes, `sys_exit` resets foreground to parent (kernel), restoring the old behavior.

---

## File Summary

| File | Changes |
|------|---------|
| `libkernel/src/task/scheduler.rs` | `Blocked` state, `block_current_thread()`, `unblock()` |
| `libkernel/src/file.rs` | `FileHandle` trait (returns `FileError`), `FileError` enum, `ConsoleHandle` |
| `libkernel/src/console.rs` | Console input buffer, foreground PID tracking |
| `libkernel/src/process.rs` | `fd_table`, `cwd`, `parent_pid`, `wait_thread`; fd helpers (return `FileError`) |
| `osl/src/errno.rs` | Linux errno constants, `file_errno()` / `vfs_errno()` converters |
| `osl/src/blocking.rs` | `blocking()` async-to-sync bridge |
| `osl/src/file.rs` | `VfsHandle`, `DirHandle` (VFS-backed file handles) |
| `osl/src/syscalls/` | Syscall dispatch and implementations: read/write/close/open/getdents64/getcwd/chdir/clone/execve/waitpid |
| `osl/src/spawn.rs` | `spawn_process_full` with argv + parent PID |
| `libkernel/src/syscall.rs` | SYSCALL assembly entry stub, PER_CPU data, init |
| `kernel/src/ring3.rs` | Legacy `spawn_process` wrapper, blob spawning tests |
| `kernel/src/keyboard_actor.rs` | Foreground routing: raw bytes to console buffer |
| `kernel/src/main.rs` | Auto-launch `/shell` on boot |
| `user/shell.c` | Userspace shell with line editing and commands |

---

## Verification

1. **Phase 1:** Spawn a kernel thread that blocks itself; have another thread unblock it after a delay. Verify it resumes.
2. **Phase 2-3:** `exec /hello` still works (write goes through fd table). Boot with no userspace shell — kernel shell still functional.
3. **Phase 4:** From kernel shell, `exec` a test program that does `open("/hello")` + `read()` + `write(1)` to cat a file.
4. **Phase 5:** Test program that spawns `/hello` and waits for it.
5. **Phase 6:** Boot with `/shell` on disk. Verify: prompt appears, echo/pwd/cd/ls/cat/exit work, running `/hello` from shell works, Ctrl+C cancels input, exiting shell returns to kernel shell.

---

## Risks

- **Heap pressure:** 512 KiB kernel heap is tight with multiple processes. May need to increase to 1 MiB. Monitor with `/proc/meminfo`.
- **VFS bridge correctness:** The async task must complete before the blocked thread is woken. Guaranteed by design, but a panic in the async path leaves the thread blocked forever. Consider adding a timeout or panic handler.
- **getdents64 format complexity:** Must match Linux's `struct linux_dirent64` layout exactly for musl's `readdir()` to work. Alternative: shell can use raw `syscall(217, ...)` with custom parsing.
