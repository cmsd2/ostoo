# Code Quality Audit

A review of code smells, magic numbers, duplicated code, and missing
abstractions across the codebase.  Companion to `unsafe-audit.md` which
covers `unsafe` specifically.

Date: 2026-03-19

---

## 1. Magic Numbers

### ~~1.1 Syscall numbers — `osl/src/dispatch.rs`~~ ✅ DONE

**Fixed in** `95da4c0`.  Named constants in `osl/src/syscall_nr.rs`;
dispatch match now uses `SYS_READ`, `SYS_WRITE`, etc.

### ~~1.2 MSR addresses~~ ✅ DONE

**Fixed in** `95da4c0`.  Named constants in `libkernel/src/msr.rs`
(`IA32_FS_BASE`, `IA32_EFER`, etc.); all 12+ inline uses replaced.

### ~~1.3 Page size~~ ✅ DONE

**Fixed in** `95da4c0`.  `PAGE_SIZE` and `PAGE_MASK` in
`libkernel/src/consts.rs`; all 20+ inline uses replaced.

### ~~1.4 Stack sizes~~ ✅ DONE

**Fixed in** `95da4c0`.  `KERNEL_STACK_SIZE` in `libkernel/src/consts.rs`;
all 4 locations updated.

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

### ~~1.6 stat struct layout~~ ✅ DONE (partial)

**Fixed in** `95da4c0`.  `STAT_SIZE` and `S_IFCHR` are now named constants
in `sys_fstat`.  `0o666` (permission mode) remains inline — acceptable as a
well-known octal literal.

### 1.7 VirtIO vendor/device IDs — `kernel/src/main.rs`

`0x1AF4`, `0x1042`, `0x1001` used inline in the virtio-blk PCI scan.
Now also `0x1049`, `0x1009` for virtio-9p.

**Fix:**

```rust
const VIRTIO_VENDOR_ID: u16 = 0x1AF4;
const VIRTIO_BLK_MODERN_DEVICE_ID: u16 = 0x1042;
const VIRTIO_BLK_LEGACY_DEVICE_ID: u16 = 0x1001;
const VIRTIO_9P_MODERN_DEVICE_ID: u16 = 0x1049;
const VIRTIO_9P_LEGACY_DEVICE_ID: u16 = 0x1009;
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

### ~~2.1 FD table retrieval~~ ✅ DONE

**Fixed in** `95da4c0`.  `get_fd_handle()` helper in `osl/src/dispatch.rs`
eliminates 4 identical fd-lookup blocks.

### ~~2.2 Page alloc + zero + map loop~~ ✅ DONE

**Fixed in** `95da4c0`.  `MemoryServices::alloc_and_map_user_pages()` in
`libkernel/src/memory/mod.rs` replaces the alloc+zero+map loops in
`sys_brk` and `sys_mmap`.  (The `spawn.rs` loop is slightly different —
it writes ELF segment data — so it was not collapsed.)

### ~~2.3 Page clearing~~ ✅ DONE

**Fixed in** `95da4c0`.  `clear_page()` in `libkernel/src/consts.rs`
replaces 6 inline `write_bytes` calls.  (Some calls in `spawn.rs` that
write non-zero data were not replaced.)

### ~~2.4 PageTableFlags construction~~ ✅ DONE

**Fixed in** `95da4c0`.  `USER_DATA_FLAGS` constant in
`osl/src/dispatch.rs` replaces 3 identical flag expressions.

### ~~2.5 Path normalization — duplicated between crates~~ ✅ DONE

**Fixed in** `libkernel/src/path.rs`.  `normalize()` and `resolve()` are
now shared; `kernel/src/shell.rs` and `osl/src/dispatch.rs` both delegate
to `libkernel::path`.

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

### Tier 1: Easy wins with high readability payoff — ✅ ALL DONE

All Tier 1 items were completed in `95da4c0`:

1. ~~Named constants for syscall numbers, MSRs, page sizes~~ ✅
2. ~~Extract `get_fd_handle()` helper (eliminates 4 copies)~~ ✅
3. ~~Extract `alloc_and_map_user_pages()` (eliminates 3 copies)~~ ✅
4. ~~`const USER_DATA_FLAGS` for page table flags~~ ✅
5. ~~`clear_page()` utility (eliminates 8 copies)~~ ✅

### Tier 2: Structural improvements

6. ~~Share path normalization between kernel shell and osl~~ ✅
7. ~~`ProcessManager` struct to encapsulate process table~~ ✅
8. Decompose `on_key` into a `LineEditor` state machine
9. Decompose `preempt_tick` into smaller functions
10. Break `sys_brk` / `sys_mmap` into validation + mapping

### Tier 3: Architectural refinements

11. Split `MemoryServices` into focused sub-managers
12. `SyscallContext` struct for cleaner parameter passing
13. `ConsoleInput` encapsulation
14. `UserVirtAddr` / `KernelVirtAddr` newtypes
15. `FileHandle` trait restructuring (split or default impls)
