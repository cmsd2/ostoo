# arch_prctl (nr 158)

## Linux Signature

```c
int arch_prctl(int code, unsigned long addr);
```

## Description

Sets or gets architecture-specific thread state. On x86-64, primarily used to set the FS and GS segment base registers for thread-local storage (TLS).

## Current Implementation

- **`ARCH_SET_FS` (0x1002):** Writes `addr` to the `IA32_FS_BASE` MSR (0xC000_0100). This is how musl sets up its TLS pointer during C runtime initialisation. Returns 0 on success.
- **All other codes:** Returns `-EINVAL` (-22).

**Source:** `libkernel/src/syscall.rs` — `sys_arch_prctl`

## Future Work

- Implement `ARCH_GET_FS` (0x1003) to read back the current FS base.
- Implement `ARCH_SET_GS` (0x1001) and `ARCH_GET_GS` (0x1004) for GS-based TLS.
- Save/restore FS_BASE across context switches if multiple user processes use TLS concurrently (currently each process sets it fresh via the trampoline, but preemption during a syscall could lose the value).
