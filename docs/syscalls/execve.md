# execve (nr 59)

## Linux Signature

```c
int execve(const char *pathname, char *const argv[], char *const envp[]);
```

## Description

Replaces the current process image with a new ELF binary. On success, the calling process's address space, stack, and brk are replaced; the process continues execution at the new program's entry point. On failure, the original process is unchanged.

## Current Implementation

1. **Copy arguments from userspace:** Reads `pathname` (null-terminated string), `argv` (NULL-terminated array of string pointers), and `envp` (NULL-terminated array of string pointers) into kernel buffers before destroying the address space.
2. **Resolve path:** Resolves relative to the process's `cwd`.
3. **Read ELF from VFS:** Loads the entire ELF binary via `devices::vfs::read_file()`.
4. **Parse ELF:** Extracts PT_LOAD segments, entry point, and program headers via `libkernel::elf::parse`.
5. **Create fresh PML4:** Allocates a new user page table (kernel entries 256–510 are copied from the active PML4). The old PML4 and its user-half page tables are freed after switching CR3 (skipped for `CLONE_VM` shared PML4s).
6. **Map ELF segments:** Maps each PT_LOAD segment into the new PML4 with correct permissions (R/W/X).
7. **Map user stack:** 8 pages (32 KiB) at `0x0000_7FFF_F000_0000`.
8. **Build initial stack:** Writes `argc`, `argv` pointers, `envp` pointers, and auxiliary vector (`AT_PHDR`, `AT_PHENT`, `AT_PHNUM`, `AT_PAGESZ`, `AT_ENTRY`, `AT_UID`, `AT_RANDOM`) onto the user stack.
9. **Update process:** Sets new `pml4_phys`, `entry_point`, `user_stack_top`, `brk_base`/`brk_current`, resets `mmap_next`/`mmap_regions`. Calls `close_cloexec_fds()` to close all file descriptors with `FD_CLOEXEC` set. Resets `FS_BASE` to 0 (new program's libc will set up TLS).
10. **Unblock vfork parent:** If this process was created by `clone(CLONE_VFORK)`, unblocks the parent thread.
11. **Jump to userspace:** Switches CR3 to the new PML4 and does `iretq` to the new entry point. Never returns.

On any error before step 9, returns a negative errno — the original process is unchanged.

**Source:** `osl/src/exec.rs` — `sys_execve`

## Errors

| Errno | Condition |
|-------|-----------|
| `-EFAULT` (-14) | Invalid pathname, argv, or envp pointer |
| `-ENOENT` (-2) | File not found on VFS |
| `-ENOEXEC` (-8) | Invalid ELF binary or no loadable segments |
| `-EINVAL` (-22) | Too many arguments (>256) |

## Future Work

- Support `#!` (shebang) script execution.
- Proper `AT_RANDOM` with real randomness instead of a fixed address.
