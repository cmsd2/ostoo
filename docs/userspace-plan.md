# Plan: User Space and Process Isolation

## Context

The kernel currently runs everything — drivers, shell, filesystem — in a single
ring-0 address space as async Rust tasks.  This document outlines the path from
that baseline to a system where untrusted programs run in isolated ring-3
processes with their own virtual address spaces, communicating with the kernel
through system calls, and eventually linked against a ported musl libc.

---

## Progress Summary

Phases 0–3 are **complete**. The kernel can parse ELF binaries, create isolated
per-process address spaces, drop to ring 3 via `iretq`, handle SYSCALL/SYSRET,
and preemptively schedule user processes alongside kernel threads. Ring-3 page
faults kill the offending process without crashing the kernel.

| Phase | Status | Milestone |
|-------|--------|-----------|
| 0 — Toolchain | **Done** | Hand-crafted assembly blobs and static ELF binaries load and run |
| 1 — Ring-3 + SYSCALL | **Done** | GDT has ring-3 segments; SYSCALL/SYSRET works; `sys_write`, `sys_exit`, `sys_arch_prctl` implemented |
| 2 — Per-process page tables | **Done** | `create_user_page_table`, `map_user_page`, CR3 switching on context switch; ring-3 page faults kill the process |
| 3 — Process abstraction | **Done** | `Process` struct, process table, ELF loader, `exec` shell command, zombie reaping |
| 4 — System call layer | **Not started** | Need `read`, `mmap`, `brk`, `fstat`, `close`, `munmap` for musl hello-world |
| 5 — Cross-compiler + musl | **Not started** | Need Phase 4 syscalls first |
| 6 — Fork / wait / user shell | **Not started** | Requires CoW page faults, `waitpid` |
| 7 — Signals | **Not started** | Requires signal frame push/pop, `rt_sigaction`, `rt_sigreturn` |

### What works today

- Shell command `exec <path>` reads an ELF from the VFS and spawns it as a
  ring-3 process with its own PML4.
- User code can call `sys_write(1, buf, len)` to print to the VGA console.
- User code calls `sys_exit(code)` to terminate; the process is marked zombie
  and reaped on the next `exec` or `spawn_blob`.
- `sys_arch_prctl(ARCH_SET_FS, addr)` writes `IA32_FS_BASE` (needed by musl TLS).
- Ring-3 page faults (e.g. `test pagefault`) log the fault, mark the process
  zombie, restore kernel GS polarity, and kill the thread — no kernel panic.
- `test isolation` verifies two independently-created PML4s have genuinely
  independent user-space mappings at the same virtual address.
- System info commands (cpuinfo, meminfo, memmap, pmap, threads, tasks, idt,
  pci, lapic, ioapic, drivers, uptime) are exposed as `/proc` virtual files
  accessible via `cat /proc/<file>`.

### Key implementation files

| File | Role |
|------|------|
| `libkernel/src/gdt.rs` | GDT with kernel + user code/data segments, TSS, `set_kernel_stack` for rsp0 |
| `libkernel/src/syscall.rs` | SYSCALL MSR init, assembly entry stub, `syscall_dispatch`, per-CPU data |
| `libkernel/src/process.rs` | `Process` struct, `ProcessId`, process table, zombie marking/reaping |
| `libkernel/src/elf.rs` | Minimal ELF64 parser (static `ET_EXEC`, x86-64) |
| `libkernel/src/memory/mod.rs` | `create_user_page_table`, `map_user_page`, `switch_address_space` |
| `libkernel/src/task/scheduler.rs` | `spawn_user_thread`, `process_trampoline`, CR3 switching in `preempt_tick` |
| `libkernel/src/interrupts.rs` | Ring-3-aware page fault handler |
| `kernel/src/ring3.rs` | `spawn_process` (ELF), `spawn_blob` (raw code), test helpers |
| `devices/src/vfs/proc_vfs.rs` | ProcVfs with 12 virtual files |

---

## Virtual Address Space Layout

The kernel's heap, APIC, and MMIO window live in the high canonical half
(≥ `0xFFFF_8000_0000_0000`), so the entire lower canonical half is available
for user process address spaces.  The kernel/user boundary is enforced at the
PML4 level: entries 0–255 (lower half) are user-private; entries 256–510
(high half) are kernel-shared; entry 511 is the per-PML4 recursive
self-mapping.

```
0x0000_0000_0000_0000  ← canonical zero (null pointer trap page, unmapped)
0x0000_0000_0040_0000  ← ELF load address (4 MiB, standard x86-64)
         ↓ text, data, BSS
         ↓ brk heap (grows up)
0x0000_0010_0000_0000
         ↓ mmap region (grows down from stack)
0x0000_3FFF_FF00_0000  ← user stack top (grows down)
0x0000_7FFF_FFFF_FFFF  ← top of lower canonical half (entire range = user)
                         (non-canonical gap)
0xFFFF_8000_0000_0000  ← kernel heap        (HEAP_START, 256 KiB)
0xFFFF_8001_0000_0000  ← Local APIC MMIO    (APIC_BASE)
0xFFFF_8001_0001_0000  ← IO APIC(s)
0xFFFF_8002_0000_0000  ← MMIO window        (MMIO_VIRT_BASE, 512 GiB)
phys_mem_offset         ← bootloader physical memory identity map (high half)
0xFFFF_FF80_0000_0000  ← recursive PT window (PML4[511])
0xFFFF_FFFF_FFFF_F000  ← PML4 self-mapping
```

Kernel entries (PML4 indices 256–510) are copied into every process page table
without `USER_ACCESSIBLE`; they are invisible to ring-3 code.

---

## Phase 0 — Toolchain and Build Infrastructure  ✅ COMPLETE

**Goal:** produce user-space ELF binaries that the kernel can load, without
needing musl yet.

### 0a. Custom linker script

Write `user/link.ld`:

```ld
ENTRY(_start)
SECTIONS {
  . = 0x400000;
  .text   : { *(.text*) }
  .rodata : { *(.rodata*) }
  .data   : { *(.data*) }
  .bss    : { *(.bss*) COMMON }
}
```

### 0b. Rust `no_std` user target

Add a custom target JSON `x86_64-ostoo-user.json` with:
- `"os": "none"`, `"env": ""`, `"vendor": "unknown"`
- `"pre-link-args"`: pass the linker script
- `"panic-strategy": "abort"` (no unwinding in user space initially)
- `"disable-redzone": true` (same requirement as kernel)

A minimal `user/` crate can implement `_start` in assembly, call a `main`, then
invoke the `exit` syscall.

### 0c. Assembly user programs

Before the ELF loader exists, a hand-crafted binary blob (or raw ELF built from
a few lines of NASM) is enough to verify the ring-3 transition and basic
syscalls work.

---

## Phase 1 — Ring-3 GDT Segments and SYSCALL Infrastructure  ✅ COMPLETE

**Goal:** the kernel can jump to ring 3 and come back via SYSCALL/SYSRET.
No process isolation yet — user code runs in the kernel's own address space.

**What was implemented:**
- GDT extended with kernel data, user data, and user code segments in the order
  required by `IA32_STAR` (`libkernel/src/gdt.rs`).
- `TSS.rsp0` updated via `set_kernel_stack()` on every context switch to a user
  process.
- SYSCALL MSRs (`STAR`, `LSTAR`, `FMASK`, `EFER.SCE`) configured in
  `libkernel/src/syscall.rs::init()`.
- Assembly entry stub with `swapgs`, per-CPU kernel/user RSP swap, and SysV64
  argument shuffle before calling `syscall_dispatch`.
- Three syscalls: `write` (fd 1/2 to VGA), `exit`/`exit_group` (mark zombie +
  kill thread), `arch_prctl(ARCH_SET_FS)` (write `IA32_FS_BASE` MSR).
- Ring-3 test (`test ring3`): drops to user mode, writes "Hello from ring 3!"
  via syscall, exits cleanly.

### 1a. GDT additions (`libkernel/src/gdt.rs`)

Add four new descriptors in the order required by `IA32_STAR`:

```
Index  Selector  Descriptor
  0    0x00      Null
  1    0x08      Kernel code (ring 0, already exists)
  2    0x10      Kernel data (ring 0) ← new; SYSRET expects it at STAR[47:32]+8
  3    0x18      (padding / null for SYSRET alignment)
  4    0x20      User   code (ring 3) ← new; STAR[63:48]
  5    0x28      User   data (ring 3) ← new; at STAR[63:48]+8
  6    0x30+     TSS (2 slots for the 16-byte system descriptor)
```

`IA32_STAR` layout: bits 47:32 = kernel CS (SYSCALL), bits 63:48 = user CS − 16
(SYSRET uses this+16 for CS and +8 for SS).

Update the `Selectors` struct and `init()` in `gdt.rs`.

### 1b. TSS kernel-stack field

When the CPU delivers a ring-3 interrupt it loads RSP from `TSS.rsp0`.  This
must point to the current process's kernel stack top.  For now a single global
TSS is fine; when processes exist, `rsp0` is updated on every context switch.

### 1c. SYSCALL MSR setup (`libkernel/src/interrupts.rs` or new `libkernel/src/syscall.rs`)

```rust
pub fn init_syscall() {
    // IA32_STAR: kernel CS at bits 47:32, user CS-16 at bits 63:48
    let star: u64 = ((KERNEL_CS as u64) << 32) | ((USER_CS as u64 - 16) << 48);
    unsafe { Msr::new(0xC000_0081).write(star); }         // STAR

    // IA32_LSTAR: entry point for 64-bit SYSCALL
    unsafe { Msr::new(0xC000_0082).write(syscall_entry as u64); }

    // IA32_FMASK: clear IF, DF on SYSCALL (but keep other flags)
    unsafe { Msr::new(0xC000_0084).write(0x0000_0300); }  // IF | DF

    // Enable SCE bit in EFER
    let efer = unsafe { Msr::new(0xC000_0080).read() };
    unsafe { Msr::new(0xC000_0080).write(efer | 1); }
}
```

### 1d. Assembly syscall entry stub

`libkernel/src/syscall_entry.asm` (or `global_asm!` in `syscall.rs`):

```asm
syscall_entry:
    swapgs                  ; switch to kernel GS (store user GS)
    mov  [gs:USER_RSP], rsp ; save user RSP into per-cpu area
    mov  rsp, [gs:KERN_RSP] ; load kernel RSP

    push rcx                ; user RIP (SYSCALL saves it here)
    push r11                ; user RFLAGS

    ; push all scratch registers
    push rax
    push rdi
    push rsi
    push rdx
    push r10
    push r8
    push r9

    ; rax = syscall number, rdi/rsi/rdx/r10/r8/r9 = arguments
    mov  rdi, rax
    call syscall_dispatch   ; -> rax = return value

    pop  r9
    pop  r8
    pop  r10
    pop  rdx
    pop  rsi
    pop  rdi
    ; leave rax as return value

    pop  r11                ; restore RFLAGS
    pop  rcx                ; restore user RIP
    mov  rsp, [gs:USER_RSP] ; restore user RSP
    swapgs
    sysretq
```

`swapgs` requires a per-CPU data block holding the kernel stack pointer.
Implement as a small struct at a known virtual address (or via `GS_BASE` MSR).

### 1e. Minimal syscall dispatch table

Start with just three numbers (matching Linux x86-64 for musl compatibility):

| Number | Name | Action |
|--------|------|--------|
| 0 | `read` | stub → return −ENOSYS |
| 1 | `write` | write to VGA console if fd==1/2 |
| 60 | `exit` | terminate current process |

### 1f. First ring-3 test

Write a tiny inline assembly test in `kernel/src/main.rs` that:
1. Pushes a fake user-mode iret frame (SS, RSP, RFLAGS with IF, CS ring-3, RIP).
2. `iretq` into ring 3.
3. User code executes `syscall` with `rax=1` (write), prints one character.
4. Kernel writes it to VGA and returns to ring 3.
5. User code executes `syscall` with `rax=60` (exit).

This verifies the GDT, SYSCALL, and basic ABI without an ELF loader or address
space isolation.

---

## Phase 2 — Per-Process Page Tables and Address Space Isolation  ✅ COMPLETE

**Goal:** each process has its own PML4; kernel mappings are shared; user
mappings are private.

**What was implemented:**
- `MemoryServices::create_user_page_table()` allocates a fresh PML4, copies
  kernel entries (indices 256–510) without `USER_ACCESSIBLE`, and sets the
  recursive self-mapping at index 511.
- `MemoryServices::map_user_page()` maps individual 4 KiB pages in a non-active
  page table given its PML4 physical address.
- `unsafe switch_address_space(pml4_phys)` writes CR3.
- Page fault handler (`libkernel/src/interrupts.rs`) checks
  `stack_frame.code_segment.rpl()` — ring-3 faults mark the process zombie
  (exit code -11 / SIGSEGV), restore kernel GS via `swapgs`, and call
  `kill_current_thread()`.  Kernel faults still panic.
- `test isolation` shell command verifies two PML4s map the same user virtual
  address to different physical frames.
- Scheduler `preempt_tick` saves/restores CR3 when switching between threads
  with different page tables.

### 2a. Page table creation (`libkernel/src/memory/`)

Add to `MemoryServices`:

```rust
/// Allocate a fresh PML4, copy all kernel PML4 entries (indices where
/// virtual_address >= KERNEL_SPLIT) into it, and return the physical
/// address of the new PML4 frame.
pub fn create_user_page_table(&mut self) -> PhysAddr;

/// Map a single 4 KiB page in a specific (possibly non-active) page table.
pub fn map_user_page(
    &mut self,
    pml4_phys: PhysAddr,
    virt: VirtAddr,
    phys: PhysAddr,
    flags: PageTableFlags,   // USER_ACCESSIBLE | PRESENT | WRITABLE | NO_EXECUTE as needed
) -> Result<(), MapToError<Size4KiB>>;

/// Switch the active address space.  Must be called with interrupts disabled.
pub unsafe fn switch_address_space(&self, pml4_phys: PhysAddr);
```

### 2b. Kernel/user PML4 split

The layout gives a clean hardware-level split:

- **PML4 indices 0–255** (lower canonical half, `0x0000_*`) — user-private.
  Left empty at process creation; populated by the ELF loader and `mmap`.
- **PML4 indices 256–510** (high canonical half, `0xFFFF_8000_*` through
  `0xFFFF_FF7F_*`) — kernel-shared.  Copied from the kernel PML4 at process
  creation; marked present but never `USER_ACCESSIBLE`.
- **PML4 index 511** — the recursive self-mapping.  Each process PML4 must
  have its own entry here pointing to **its own** physical PML4 frame (not
  the kernel's).  `create_user_page_table` must set this explicitly.

### 2c. Page fault handler upgrade

Replace the panic in `page_fault_handler` with:

```rust
extern "x86-interrupt" fn page_fault_handler(frame: InterruptStackFrame, ec: PageFaultErrorCode) {
    let faulting_addr = Cr2::read();
    if frame.code_segment.rpl() == PrivilegeLevel::Ring3 {
        // Fault in user space — kill the process (deliver SIGSEGV later).
        kill_current_process(Signal::Segv);
        schedule_next();       // does not return to faulting instruction
    } else {
        panic!("kernel page fault at {:?}\n{:#?}\n{:?}", faulting_addr, frame, ec);
    }
}
```

This is the minimum needed to prevent a kernel panic when user code accesses
invalid memory; proper CoW / demand paging comes later.

### 2d. Address space switch on context switch

The scheduler's `preempt_tick` function currently saves/restores only kernel
RSP.  Extend it to also write `CR3` when switching between processes with
different page tables.

---

## Phase 3 — Process Abstraction  ✅ COMPLETE

**Goal:** `Process` struct, a process table, and a working `exec`.

**What was implemented:**
- `Process` struct (`libkernel/src/process.rs`) with PID, state (Running/Zombie),
  PML4 physical address, heap-allocated 64 KiB kernel stack, entry point, user
  stack top, thread index, and exit code.
- Global `PROCESS_TABLE: Mutex<BTreeMap<ProcessId, Process>>` and
  `CURRENT_PID: AtomicU64`.
- `insert()`, `current_pid()`, `set_current_pid()`, `with_process()`,
  `mark_zombie()`, `reap()`, `reap_zombies()`.
- Scheduler integration: `SchedulableKind::Kernel | UserProcess(ProcessId)`.
  `spawn_user_thread` creates a thread targeting `process_trampoline` which sets
  up TSS.rsp0, per-CPU kernel RSP, PID tracking, GS polarity, CR3 switch, and
  then does `iretq` into ring-3 user code.
- `kill_current_thread()` marks the thread Dead and spins; timer preemption skips
  dead threads.
- ELF loader (`libkernel/src/elf.rs`): minimal parser for static `ET_EXEC`
  x86-64 binaries.  Returns `ElfInfo { entry, segments: Vec<LoadSegment> }`.
- `kernel/src/ring3.rs::spawn_process(elf_data)` — parses ELF, creates user PML4,
  maps all PT_LOAD segments (with correct R/W/X flags) plus a user stack page,
  creates a Process, and spawns a user thread.  Returns `Ok(ProcessId)`.
- Shell command `exec <path>` reads an ELF from the VFS and calls `spawn_process`.
- `spawn_blob(code)` helper for test commands: maps a raw code blob + stack,
  creates a Process, spawns a user thread.
- Zombie reaping: `reap_zombies()` is called at the start of `spawn_blob` and
  `spawn_process` to free kernel stacks of fully-exited processes.

### 3a. `Process` struct (`libkernel/src/process/mod.rs`)

```rust
pub struct Process {
    pub pid:           ProcessId,
    pub state:         ProcessState,          // Running, Ready, Blocked, Zombie
    pub pml4_phys:     PhysAddr,              // physical address of PML4
    pub kernel_stack:  Vec<u8>,               // 64 KiB kernel stack
    pub saved_rsp:     u64,                   // kernel RSP when not running
    pub user_rsp:      u64,                   // user RSP (restored on ring-3 return)
    pub files:         FileTable,             // open file descriptors
    pub parent:        Option<ProcessId>,
    pub exit_code:     Option<i32>,
}
```

### 3b. Process table

```rust
lazy_static! {
    static ref PROCESSES: Mutex<BTreeMap<ProcessId, Process>> = ...;
}
```

`CURRENT_PID: AtomicU32` — the PID running on each CPU (single-CPU for now).

### 3c. Scheduler integration

Replace the bare `Thread` list in `scheduler.rs` with process-aware scheduling:
- On `preempt_tick`: save user context (if coming from ring 3), switch `CR3`,
  load next process's user context and kernel RSP.
- `TSS.rsp0` updated to point to the new process's kernel stack top.

### 3d. ELF loader (`libkernel/src/elf.rs`)

```rust
pub fn load_elf(
    bytes: &[u8],
    process: &mut Process,
    mem: &mut MemoryServices,
) -> Result<VirtAddr, ElfError>   // returns entry point
```

Steps:
1. Validate ELF magic, `e_machine == EM_X86_64`, `e_type == ET_EXEC` (static) or
   `ET_DYN` (PIE).
2. For each `PT_LOAD` segment: allocate physical frames, map at `p_vaddr` with
   `USER_ACCESSIBLE` and flags derived from `p_flags` (R/W/X).
3. Copy `p_filesz` bytes from the ELF image; zero-fill to `p_memsz`.
4. Allocate and map a user stack (8–16 pages) just below the stack top.
5. Set up the initial stack frame: `argc=0`, `argv=NULL`, `envp=NULL`, `auxv`
   entries for `AT_ENTRY`, `AT_PHDR`, `AT_PAGESZ` (required by musl's `_start`).
6. Return `e_entry`.

### 3e. `sys_execve` syscall

```rust
fn sys_execve(path: *const u8, argv: *const *const u8, envp: *const *const u8) -> ! {
    let bytes = vfs::read_file(path_str).expect("exec: read failed");
    let process = current_process_mut();
    process.reset_address_space();          // drop old page table
    let entry = load_elf(&bytes, process, &mut memory());
    switch_to_user(entry, process.user_stack_top);   // does not return
}
```

---

## Phase 4 — System Call Layer  ⬜ NOT STARTED

**Goal:** a syscall table wide enough to run a static musl binary that prints
"Hello, world!" and exits.

**Current state:** Only `write` (nr 1), `exit`/`exit_group` (nr 60/231), and
`arch_prctl` (nr 158) are implemented.  All other syscall numbers return
`-ENOSYS`.  No file descriptor table exists yet — `sys_write` checks
`fd == 1 || fd == 2` and writes directly to the VGA console.

### 4a. Syscall numbering

Use Linux x86-64 syscall numbers (`/usr/include/asm/unistd_64.h`) so that
binaries built with a standard `x86_64-linux-musl` toolchain work without
patching musl.

Priority subset for "Hello, world!":

| Nr | Name | Notes |
|----|------|-------|
| 0 | `read` | fd 0 (stdin); block until keyboard line |
| 1 | `write` | fd 1/2: write bytes to VGA console via VFS |
| 3 | `close` | no-op for now |
| 5 | `fstat` | return fake stat for stdin/stdout |
| 9 | `mmap` | anonymous: allocate frames, map into process space |
| 11 | `munmap` | unmap range |
| 12 | `brk` | extend heap |
| 60 | `exit` | terminate process, schedule next |
| 158 | `arch_prctl` | `ARCH_SET_FS`: set FS_BASE MSR (required by musl TLS) |
| 231 | `exit_group` | same as exit for single-threaded |

### 4b. `arch_prctl` / FS_BASE

musl sets up thread-local storage via `arch_prctl(ARCH_SET_FS, addr)`.  Handle
this by writing to the `IA32_FS_BASE` MSR (0xC000_0100).  Save/restore the
value on every context switch.

### 4c. File descriptor table

```rust
pub struct FileTable {
    entries: [Option<Arc<dyn VfsFileHandle>>; 64],
}
```

Stdin (fd 0), stdout (fd 1), stderr (fd 2) are wired up on process creation:
- fd 0: a keyboard line buffer implementing `read`
- fd 1/2: a console writer implementing `write`

`sys_open`, `sys_read`, `sys_write`, `sys_close` route through the VFS.

---

## Phase 5 — Cross-Compiler and musl Port  ⬜ NOT STARTED

**Goal:** compile C programs that run as ostoo user processes.

### 5a. Toolchain strategy

The simplest path: **use an existing `x86_64-linux-musl` sysroot unmodified**,
because we implement Linux-compatible syscall numbers (Phase 4).  musl does not
inspect the OS name at runtime — it just issues syscalls.

Option A (quickest): install `x86_64-linux-musl-gcc` from
[musl.cc](https://musl.cc/) or via `brew install x86_64-linux-musl-cross`.
Compile with:
```sh
x86_64-linux-musl-gcc -static -o hello hello.c
```
The resulting fully-static ELF should boot on ostoo once the Phase 4 syscalls
are implemented.

Option B (custom triple): build musl from source with a custom `--target`
configured for ostoo.  This is useful once ostoo diverges from Linux's ABI
(e.g. custom syscall numbers or a different startup convention).

### 5b. musl build recipe (Option B outline)

```sh
# Prerequisites: a bare x86_64-elf-gcc cross-compiler (via crosstool-ng or
# manual binutils + gcc build targeting x86_64-unknown-elf).

git clone https://git.musl-libc.org/cgit/musl
cd musl
./configure \
  --target=x86_64 \
  --prefix=/opt/ostoo-sysroot \
  --syslibdir=/opt/ostoo-sysroot/lib \
  CROSS_COMPILE=x86_64-elf-
make -j$(nproc)
make install
```

Key musl files:
- `arch/x86_64/syscall_arch.h` — `__syscall0`…`__syscall6` use the `syscall`
  instruction; no changes needed if syscall numbers match Linux.
- `crt/x86_64/crt1.o` — `_start` sets up `argc/argv/envp` from the initial stack
  (ABI defined in the ELF auxiliary vector; match what the ELF loader sets up in
  Phase 3d).
- `src/env/__init_tls.c` — calls `arch_prctl(ARCH_SET_FS, ...)`;
  requires the `sys_arch_prctl` syscall (Phase 4b).

### 5c. Rust user programs

For Rust programs targeting ostoo, add a custom target
`x86_64-ostoo-user.json` (from Phase 0b) and a minimal `ostoo-rt` crate that:
- Provides `_start` (sets up a stack frame; calls `main`; calls `sys_exit`).
- Provides `#[panic_handler]` that calls `sys_exit(1)`.
- Wraps the small syscall ABI.

Users can then write:
```rust
#![no_std]
#![no_main]
extern crate ostoo_rt;

#[no_mangle]
pub extern "C" fn main() {
    ostoo_rt::write(1, b"Hello from Rust!\n");
}
```

---

## Phase 6 — Fork, Wait, and a Minimal Shell  ⬜ NOT STARTED

**Goal:** a user-mode shell that can `fork` + `exec` child programs.

### 6a. `fork`

```rust
fn sys_fork() -> ProcessId {
    let child = current_process().clone_for_fork(mem);
    // clone_for_fork: allocate new PML4, copy-on-write all user pages
    //   (mark them read-only in both parent and child; CoW fault handler
    //    does the actual copy when a write occurs)
    enqueue_process(child);
    child.pid    // returned in parent; 0 returned in child
}
```

CoW page fault handler:
```rust
if fault is a write to a CoW page {
    allocate new frame
    copy old frame content
    remap the faulting page as writable in the current process
    resume
}
```

### 6b. `waitpid`

When a process calls `sys_exit(code)`:
1. Mark it `Zombie`, store exit code.
2. Wake any parent blocked in `waitpid`.
3. Parent collects exit code; child's resources are freed.

### 6c. User-mode shell

Once fork/exec/wait work, a minimal C shell is straightforward:
```c
while (1) {
    char line[256];
    write(1, "$ ", 2);
    read(0, line, sizeof line);
    if (fork() == 0) {
        execve(line, argv, envp);
        write(2, "exec failed\n", 12);
        _exit(1);
    }
    waitpid(-1, &status, 0);
}
```

---

## Phase 7 — Signals  ⬜ NOT STARTED

Signals are the last major piece of POSIX plumbing needed for a realistic
user-space environment.

### Minimal signal implementation

```rust
pub struct SigAction { handler: usize, flags: u32, mask: SigSet }
pub struct SigTable  { actions: [SigAction; 32], pending: SigSet, masked: SigSet }
```

- `sys_rt_sigaction` installs handlers.
- Before returning to user space after a syscall or interrupt, check `pending & ~masked`.
- If set: push a signal frame on the user stack (siginfo + ucontext), set RIP to
  the handler, clear the pending bit.
- `sys_rt_sigreturn`: the signal handler calls this when done; the kernel pops the
  ucontext and resumes normal user execution.

---

## Dependency Graph

```
Phase 0  ←─ Phase 1 ←─ Phase 2 ←─ Phase 3 ←─ Phase 4 ←─ Phase 5 (musl)
(toolchain)   (ring-3,      (address     (Process,        (syscall        (C programs)
              syscall)      spaces)      ELF loader)      table)
                                                  ↓
                                              Phase 6 (fork/exec/shell)
                                                  ↓
                                              Phase 7 (signals)
```

---

## Key Risks and Design Decisions

### SYSCALL vs INT 0x80
Use SYSCALL/SYSRET (64-bit, fast path).  INT 0x80 is the 32-bit ABI; musl
uses SYSCALL on x86-64 exclusively.

### Kernel/user split
The kernel lives entirely in the high canonical half (`0xFFFF_8000_*` and
above): heap at `0xFFFF_8000_*`, APIC at `0xFFFF_8001_*`, MMIO window at
`0xFFFF_8002_*`.  The entire lower canonical half is free for user processes.
The split is enforced at the PML4 level — user processes simply have no
mappings at indices 256–510, and the kernel entries they inherit are never
`USER_ACCESSIBLE`.  SMEP (CR4.20) and SMAP (CR4.21) provide the hardware
enforcement layer once ring-3 processes exist.

### SMEP and SMAP
Once ring-3 processes exist, enable SMEP (CR4.20) to prevent the kernel from
accidentally executing user-mapped code, and SMAP (CR4.21) to prevent the
kernel from silently accessing user memory without an explicit `stac`/`clac`
pair.  Any kernel code that copies from user buffers must use a checked copy
function that uses `stac` to temporarily permit access.

### Static-only ELF initially
Dynamic linking requires an in-kernel or user-space ELD interpreter.  Start
with `-static` binaries and the ELF loader described in Phase 3d.  PIE static
binaries (ET_DYN with no INTERP segment) should work with minor adjustments to
the loader.

### Single CPU for now
The process table and scheduler assume a single CPU.  SMP support would require
per-CPU `CURRENT_PID`, per-CPU kernel stacks in the TSS, and IPI-based TLB
shootdown when modifying another process's page table.

### Heap size
The kernel heap is 512 KiB.  Process control blocks each consume 64 KiB
(kernel stack) plus page table frames.  `reap_zombies()` is called before each
`spawn_process` to recover heap, but loading multiple concurrent processes will
still require a larger or demand-paged heap.

---

## Milestones and Test Checkpoints

| Milestone | Observable result | Status |
|-----------|------------------|--------|
| Phase 1 complete | `iretq` drops to ring 3; `syscall` returns to ring 0; "Hello from ring 3!" appears on VGA | ✅ Done |
| Phase 2 complete | Two user processes have separate address spaces; `test isolation` passes | ✅ Done |
| Phase 3 complete | `exec /path/to/elf` reads an ELF from the VFS, loads it into a fresh address space, and runs it | ✅ Done |
| Phase 4 / musl hello world | `hello` compiled with `x86_64-linux-musl-gcc -static` prints and exits cleanly | ⬜ |
| Phase 6 complete | User shell forks, execs, and waits for children | ⬜ |
| Phase 7 complete | `SIGINT` (Ctrl+C) terminates the foreground process | ⬜ |
