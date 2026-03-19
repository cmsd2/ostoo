# Code Quality Audit

A review of code smells, magic numbers, duplicated code, and missing
abstractions across the codebase.  Companion to `unsafe-audit.md` which
covers `unsafe` specifically.

Date: 2026-03-19

---

## 1. Magic Numbers

### 1.1 Syscall numbers — `osl/src/dispatch.rs:35-56`

The main dispatch match uses bare integer literals for all 22 syscall
numbers.  Adding or reordering syscalls is error-prone.

```rust
// Current:
match nr {
    0 => sys_read(...),
    1 => sys_write(...),
    ...
    500 => sys_spawn(...),
}
```

**Fix:** Create `osl/src/syscall_nr.rs` with named constants:

```rust
pub const SYS_READ: u64 = 0;
pub const SYS_WRITE: u64 = 1;
pub const SYS_OPEN: u64 = 2;
pub const SYS_CLOSE: u64 = 3;
pub const SYS_FSTAT: u64 = 5;
pub const SYS_LSEEK: u64 = 8;
pub const SYS_MMAP: u64 = 9;
pub const SYS_MPROTECT: u64 = 10;
pub const SYS_MUNMAP: u64 = 11;
pub const SYS_BRK: u64 = 12;
pub const SYS_IOCTL: u64 = 16;
pub const SYS_WRITEV: u64 = 20;
pub const SYS_EXIT: u64 = 60;
pub const SYS_WAIT4: u64 = 61;
pub const SYS_GETCWD: u64 = 79;
pub const SYS_CHDIR: u64 = 80;
pub const SYS_ARCH_PRCTL: u64 = 158;
pub const SYS_FUTEX: u64 = 202;
pub const SYS_GETDENTS64: u64 = 217;
pub const SYS_SET_TID_ADDRESS: u64 = 218;
pub const SYS_EXIT_GROUP: u64 = 231;
pub const SYS_SET_ROBUST_LIST: u64 = 273;
pub const SYS_SPAWN: u64 = 500;
```

### 1.2 MSR addresses — 12+ inline uses across 4 files

MSR numbers like `0xC000_0100` (FS.BASE), `0xC000_0101` (GS.BASE),
`0xC000_0102` (KERNEL_GS.BASE), `0xC000_0080` (EFER), `0xC000_0081`
(STAR), `0xC000_0082` (LSTAR), `0xC000_0084` (FMASK) appear in:

- `libkernel/src/syscall.rs:65,69,76,79,84,88`
- `libkernel/src/task/scheduler.rs:426,427,555,586`
- `libkernel/src/interrupts.rs:273,274,436`
- `osl/src/dispatch.rs:187`

**Fix:** Create `libkernel/src/msr.rs`:

```rust
pub const IA32_FS_BASE: u32 = 0xC000_0100;
pub const IA32_GS_BASE: u32 = 0xC000_0101;
pub const IA32_KERNEL_GS_BASE: u32 = 0xC000_0102;
pub const IA32_EFER: u32 = 0xC000_0080;
pub const IA32_STAR: u32 = 0xC000_0081;
pub const IA32_LSTAR: u32 = 0xC000_0082;
pub const IA32_FMASK: u32 = 0xC000_0084;
```

### 1.3 Page size — 20+ inline uses

`0x1000`, `0xFFF`, and `4096` are used for page arithmetic in:

- `osl/src/spawn.rs:41,42,43,46,52,69,112`
- `osl/src/dispatch.rs:249,255,259,265,344,345,359,365`
- `libkernel/src/memory/mod.rs:118,120,130,142,144`
- `kernel/src/main.rs:99`
- `kernel/src/ring3.rs:65,71,77`
- `devices/src/virtio/mod.rs:37`

**Fix:** Define once in libkernel, import everywhere:

```rust
pub const PAGE_SIZE: u64 = 0x1000;
pub const PAGE_MASK: u64 = 0xFFF;
```

### 1.4 Stack sizes — 4 locations use `64 * 1024`

- `libkernel/src/syscall.rs:40`
- `libkernel/src/process.rs:81`
- `libkernel/src/task/scheduler.rs:218,266`

**Fix:** `pub const KERNEL_STACK_SIZE: usize = 64 * 1024;` in one place.

### 1.5 I/O port addresses — interrupts.rs

- `0x21`, `0xA1` — PIC data ports (lines 111-112)
- `0x43`, `0x40` — PIT command/channel0 ports (lines 218-220)
- `0x34` — PIT mode command byte (line 220)
- `11932` — PIT reload for 100 Hz (line 216)

**Fix:** Named constants:

```rust
const PIC_MASTER_DATA: u16 = 0x21;
const PIC_SLAVE_DATA: u16 = 0xA1;
const PIT_COMMAND: u16 = 0x43;
const PIT_CHANNEL0: u16 = 0x40;
const PIT_MODE_RATE_GEN: u8 = 0x34;
const PIT_100HZ_RELOAD: u16 = 11932;
```

### 1.6 stat struct layout — `osl/src/dispatch.rs:213-219`

Magic offsets `144`, `24`, `0o020000`, `0o666` for fstat.

**Fix:** Named constants or a `#[repr(C)]` `LinuxStat` struct.

### 1.7 VirtIO vendor/device IDs — `kernel/src/main.rs:149,151`

`0x1AF4`, `0x1042`, `0x1001` used inline in PCI scan.

**Fix:**

```rust
const VIRTIO_VENDOR_ID: u16 = 0x1AF4;
const VIRTIO_BLK_MODERN_DEVICE_ID: u16 = 0x1042;
const VIRTIO_BLK_LEGACY_DEVICE_ID: u16 = 0x1001;
```

### 1.8 Other notable magic numbers

| Location | Value | Suggested name |
|---|---|---|
| `scheduler.rs:138` | `0x202` | `RFLAGS_IF_RESERVED` |
| `scheduler.rs:283,337` | `0x1F80` | `MXCSR_DEFAULT` |
| `vga_buffer.rs:85,306` | `0x20..=0x7e` | `PRINTABLE_ASCII` |
| `vga_buffer.rs:308` | `0xfe` | `NONPRINTABLE_PLACEHOLDER` |
| `memory/mod.rs:333,335` | `0x1FF` | `PAGE_TABLE_INDEX_MASK` |
| `dispatch.rs:298` | `16` | `IOVEC_SIZE` |
| `dispatch.rs:398,480,547` | `4096` | `MAX_PATH_LEN` |
| `gdt.rs:33` | `4096 * 5` | `DOUBLE_FAULT_STACK_SIZE` |

---

## 2. Duplicated Code

### 2.1 FD table retrieval — `osl/src/dispatch.rs` (4 copies)

Lines 151-155, 200-204, 291-295, 447-451 all have this identical block:

```rust
let handle = match libkernel::process::with_process_ref(pid, |p| p.get_fd(fd as usize)) {
    Some(Ok(h)) => h,
    Some(Err(e)) => return errno::file_errno(e),
    None => return -errno::EBADF,
};
```

**Fix:** Extract helper:

```rust
fn get_fd_handle(fd: u64) -> Result<Arc<dyn FileHandle>, i64> {
    let pid = libkernel::process::current_pid();
    match libkernel::process::with_process_ref(pid, |p| p.get_fd(fd as usize)) {
        Some(Ok(h)) => Ok(h),
        Some(Err(e)) => Err(errno::file_errno(e)),
        None => Err(-errno::EBADF),
    }
}
```

### 2.2 Page alloc + zero + map loop — 3 copies

`sys_brk` (dispatch.rs:256-279), `sys_mmap` (dispatch.rs:358-377), and
`spawn_process_full` (spawn.rs:45-92) all have near-identical loops:

```rust
for i in 0..pages_needed {
    let frame = mem.alloc_dma_pages(1)?;
    let dst = phys_off + frame.as_u64();
    unsafe { core::ptr::write_bytes(dst.as_mut_ptr::<u8>(), 0, 0x1000); }
    mem.map_user_page(vaddr, frame, pml4_phys, flags)?;
}
```

**Fix:** Add to `MemoryServices`:

```rust
pub fn alloc_and_map_user_pages(
    &mut self,
    count: usize,
    vaddr_base: u64,
    pml4_phys: PhysAddr,
    flags: PageTableFlags,
) -> Result<(), ()>
```

### 2.3 Page clearing — 8 locations

`core::ptr::write_bytes(ptr, 0, 0x1000)` appears in dispatch.rs (3x),
spawn.rs (3x), ring3.rs (2x).

**Fix:** `pub fn clear_page(addr: VirtAddr)` utility in libkernel.

### 2.4 PageTableFlags construction — 3 copies

This identical 4-line flags expression appears in sys_brk, sys_mmap,
and spawn.rs stack mapping:

```rust
PageTableFlags::PRESENT
    | PageTableFlags::WRITABLE
    | PageTableFlags::USER_ACCESSIBLE
    | PageTableFlags::NO_EXECUTE
```

**Fix:** `pub const USER_DATA_FLAGS: PageTableFlags = ...;`

### 2.5 Path normalization — duplicated between crates

`kernel/src/shell.rs:34-68` and `osl/src/dispatch.rs:105-124` both
implement path normalization (handling `.`, `..`, leading `/`, etc.).

**Fix:** Move to `libkernel/src/path.rs` and import from both.

### 2.6 History entry restoration — `keyboard_actor.rs`

Lines 234-241 (history up) and 262-268 (history down) have identical
buffer-copy logic.

**Fix:** `LineState::restore_from_history(&mut self, entry: &str)`

### 2.7 `read_user_string` → path error wrapping — 2 copies

```rust
let path = match read_user_string(path_ptr, 4096) {
    Some(p) => p,
    None => return -errno::EFAULT,
};
```

**Fix:** `fn get_user_path(ptr: u64) -> Result<String, i64>`

---

## 3. Missing Abstractions / Interface Opportunities

### 3.1 ProcessManager struct

`libkernel/src/process.rs` has free functions `find_zombie_child`,
`has_children`, `mark_zombie`, `reap` that all operate on the global
`PROCESS_TABLE`.  These should be methods on a `ProcessManager` type
that encapsulates the table.

```rust
pub struct ProcessManager {
    table: Mutex<BTreeMap<ProcessId, Process>>,
}

impl ProcessManager {
    pub fn find_zombie_child(&self, parent: ProcessId, target: i64) -> Option<(ProcessId, i32)>;
    pub fn mark_zombie(&self, pid: ProcessId, code: i32);
    pub fn reap(&self, pid: ProcessId);
    pub fn has_children(&self, pid: ProcessId) -> bool;
}
```

### 3.2 FileHandle trait is monolithic

Every `FileHandle` implementor must provide `read`, `write`, `close`,
`kind`, and `getdents64`, even when nonsensical (e.g. `DirHandle::write`
returns `Err`).

**Options:**

- Split into `Readable`, `Writable`, `Directory` traits
- Or add default impls returning appropriate errors so implementors only
  override what they support

### 3.3 MemoryServices is a god object

~500 lines mixing physical allocation, MMIO mapping, user page tables,
address translation, and statistics.

**Fix:** Split into focused sub-types:

- `PhysicalMemoryManager` — frame allocation, phys-to-virt translation
- `MmioMapper` — MMIO region registration and caching
- `UserPageTableManager` — create/map/switch user address spaces

### 3.4 SyscallContext struct

Syscall handlers pass `(rdi, rsi, rdx, r10, r8, r9)` as 6 separate
`u64` parameters.  A context struct would be clearer:

```rust
pub struct SyscallContext {
    pub arg0: u64,
    pub arg1: u64,
    pub arg2: u64,
    pub arg3: u64,
    pub arg4: u64,
    pub arg5: u64,
}
```

This would also be the natural home for the fd-table helper method.

### 3.5 ConsoleInput encapsulation

`libkernel/src/console.rs` has `CONSOLE_INPUT: Mutex<ConsoleInner>` plus
`FOREGROUND_PID: AtomicU64` as separate globals.  These form a single
logical unit that should be one type:

```rust
pub struct ConsoleInput {
    inner: Mutex<ConsoleInner>,
    foreground_pid: AtomicU64,
}
```

### 3.6 Scattered global atomics

These related atomics are standalone statics when they could be
encapsulated in manager types:

| Static | File | Could belong to |
|---|---|---|
| `NEXT_PID`, `CURRENT_PID` | process.rs | `ProcessManager` |
| `NEXT_THREAD_ID`, `CURRENT_THREAD_IDX_ATOMIC` | scheduler.rs | `Scheduler` |
| `FOREGROUND_PID` | console.rs | `ConsoleInput` |
| `LAPIC_EOI_ADDR` | interrupts.rs | Interrupt manager |
| `CONTEXT_SWITCHES` | scheduler.rs | `Scheduler` |

### 3.7 User vs kernel address types

The type system uses `u64` for both user and kernel virtual addresses.
Newtype wrappers would prevent accidental misuse:

```rust
pub struct UserVirtAddr(u64);
pub struct KernelVirtAddr(u64);
```

---

## 4. Long Functions / Deep Nesting

### 4.1 `keyboard_actor.rs:on_key` — 238 lines

A massive match statement inside a mutex lock.  Handles 20+ key types
in one function.

**Fix:** Extract a `LineEditor` state machine with one method per key
type.  The `on_key` method becomes a thin dispatch:

```rust
fn on_key(&self, key: Key) {
    let mut editor = self.editor.lock();
    match key {
        Key::Backspace => editor.backspace(),
        Key::Delete => editor.delete(),
        Key::ArrowLeft => editor.move_left(),
        ...
    }
}
```

### 4.2 `scheduler.rs:preempt_tick` — 102 lines

The timer ISR does everything: tick → save context → select next →
restore context → switch address space → update tracking.

**Fix:** Decompose into:

- `save_current_context(idx: usize)`
- `select_next_thread() -> usize`
- `restore_thread(idx: usize)`
- `switch_address_space_if_needed(prev_pml4, next_pml4)`

### 4.3 `dispatch.rs:sys_mmap` — 68 lines

Validation, allocation, and mapping all in one function.

**Fix:** Break into `validate_mmap_request()` and the shared
`alloc_and_map_user_pages()` from section 2.2.

### 4.4 `dispatch.rs:sys_brk` — 60 lines

Same issue as `sys_mmap` — does too many things.

### 4.5 Deep nesting in `keyboard_actor.rs:159-331`

```
async fn on_key
  └─ if foreground == 0
       └─ let mut st = self.line.lock()
            └─ match key
                 └─ Key::Enter => ...
```

Four levels deep before reaching the actual logic.

---

## 5. Other Code Smells

### 5.1 Repeated runnable-state check

`scheduler.rs` lines 543 and 559 both check:

```rust
if cur_state != ThreadState::Dead && cur_state != ThreadState::Blocked { ... }
```

**Fix:** `fn is_runnable(state: ThreadState) -> bool`

### 5.2 VFS blocking wrappers

`osl/src/dispatch.rs:129-141` has `vfs_read_file` and `vfs_list_dir`
with identical structure (allocate String, call `blocking()` with async
VFS call).

**Fix:** Macro or generic wrapper to eliminate the boilerplate.

### 5.3 Process exit + parent wake pattern

`sys_exit` (dispatch.rs:168-176) does get-parent → mark_zombie →
unblock-parent as separate steps.  This should be a single
`ProcessManager::exit_and_notify(pid, code)` method.

---

## Summary — Recommended Priority

### Tier 1: Easy wins with high readability payoff

1. Named constants for syscall numbers, MSRs, page sizes
2. Extract `get_fd_handle()` helper (eliminates 4 copies)
3. Extract `alloc_and_map_user_pages()` (eliminates 3 copies)
4. `const USER_DATA_FLAGS` for page table flags
5. `clear_page()` utility (eliminates 8 copies)

### Tier 2: Structural improvements

6. Share path normalization between kernel shell and osl
7. `ProcessManager` struct to encapsulate process table
8. Decompose `on_key` into a `LineEditor` state machine
9. Decompose `preempt_tick` into smaller functions
10. Break `sys_brk` / `sys_mmap` into validation + mapping

### Tier 3: Architectural refinements

11. Split `MemoryServices` into focused sub-managers
12. `SyscallContext` struct for cleaner parameter passing
13. `ConsoleInput` encapsulation
14. `UserVirtAddr` / `KernelVirtAddr` newtypes
15. `FileHandle` trait restructuring (split or default impls)
