# FPU / SSE State Management

## x86-64 Floating-Point & SIMD Instruction Sets

| Family | Registers | Width | Notes |
|--------|-----------|-------|-------|
| x87 FPU | ST(0)–ST(7) | 80-bit | Legacy; used by some libm implementations |
| MMX | MM0–MM7 | 64-bit | Aliases x87 registers |
| SSE/SSE2 | XMM0–XMM15 | 128-bit | Baseline for x86-64; musl uses SSE2 |
| AVX/AVX2 | YMM0–YMM15 | 256-bit | Extends XMM to 256-bit upper halves |
| AVX-512 | ZMM0–ZMM31 | 512-bit | Not relevant for this kernel |

SSE2 is part of the x86-64 baseline — every long-mode CPU supports it, and
the System V AMD64 ABI uses XMM0–XMM7 for floating-point arguments/returns.
musl libc is compiled with SSE2 and will use XMM registers in user-space code.

---

## Kernel Target Configuration

The kernel's custom target (`x86_64-os.json`) specifies:

```json
"features": "-mmx,-sse,+soft-float"
```

This tells LLVM to never emit SSE/MMX instructions in kernel Rust code.
All floating-point operations (if any) use soft-float emulation.  This means
the kernel never touches XMM registers, so:

- **Syscall path**: No SSE save/restore needed — the kernel executes entirely
  with GPRs, and `syscall`/`sysret` returns to the same process.
- **Interrupt handlers**: Safe as long as they don't use SSE (guaranteed by
  the target config).
- **Timer preemption**: The only path that switches between different user
  processes' register contexts — **requires SSE save/restore**.

---

## CR0/CR4 Setup (`enable_sse`)

SSE instructions will fault unless the CPU's control registers are configured:

```rust
pub fn enable_sse() {
    unsafe {
        // CR0: clear EM (bit 2, x87 emulation), set MP (bit 1, monitor coprocessor)
        let mut cr0 = Cr0::read_raw();
        cr0 &= !(1 << 2); // clear CR0.EM
        cr0 |= 1 << 1;    // set CR0.MP
        Cr0::write_raw(cr0);

        // CR4: set OSFXSR (bit 9) and OSXMMEXCPT (bit 10)
        let mut cr4 = Cr4::read_raw();
        cr4 |= (1 << 9) | (1 << 10);
        Cr4::write_raw(cr4);
    }
}
```

- **CR0.EM = 0**: Do not trap x87/SSE instructions.
- **CR0.MP = 1**: Enable WAIT/FWAIT monitoring.
- **CR4.OSFXSR = 1**: Enable FXSAVE/FXRSTOR and SSE instructions.
- **CR4.OSXMMEXCPT = 1**: Enable unmasked SSE exception handling via #XM.

Called once during boot, before any user processes are spawned.

---

## Eager FXSAVE/FXRSTOR Context Switch

We use the **eager** strategy: save and restore FPU/SSE state on every
timer-driven context switch, unconditionally.

### Timer stub flow

```
interrupt fires → CPU pushes iretq frame (40 bytes)
               → stub pushes 15 GPRs (120 bytes)
               → sub rsp, 512; fxsave [rsp]   ← save SSE state
               → call preempt_tick             ← may switch RSP
               → fxrstor [rsp]                 ← restore SSE state
               → add rsp, 512
               → pop GPRs; iretq
```

### Stack layout during preemption

```
high address  ┌──────────────────────────┐
              │  SS / RSP / RFLAGS       │  iretq frame (40 bytes)
              │  CS / RIP                │
              ├──────────────────────────┤
              │  rax, rbx, ... r15       │  15 GPRs (120 bytes)
              ├──────────────────────────┤
              │  FXSAVE area             │  512 bytes (16-byte aligned)
              │  (x87/MMX/SSE state)     │  MXCSR at offset +24
              │                          │  XMM0-15 at offset +160
low address   └──────────────────────────┘  ← saved_rsp points here
```

Total: 672 bytes = 42 x 16, preserving 16-byte alignment for both `fxsave`
(requires 16-byte aligned operand) and the SysV ABI `call` convention.

### New thread initialization

`spawn_thread` and `spawn_user_thread` allocate the FXSAVE area below the
SwitchFrame and initialize MXCSR at offset +24 to `0x1F80` (the Intel
default: all SSE exceptions masked, round-to-nearest mode).  XMM registers
start zeroed.

---

## FXSAVE Memory Layout (512 bytes)

| Offset | Size | Field |
|--------|------|-------|
| 0 | 2 | FCW (x87 control word) |
| 2 | 2 | FSW (x87 status word) |
| 4 | 1 | FTW (abridged x87 tag word) |
| 6 | 1 | Reserved |
| 8 | 2 | FOP (last x87 opcode) |
| 10 | 8 | FIP (x87 instruction pointer) |
| 18 | 8 | FDP (x87 data pointer) |
| 24 | 4 | **MXCSR** (SSE control/status) |
| 28 | 4 | MXCSR_MASK |
| 32 | 128 | ST(0)–ST(7) / MM0–MM7 (8 x 16 bytes) |
| 160 | 256 | **XMM0–XMM15** (16 x 16 bytes) |
| 416 | 96 | Reserved (must be zero for FXRSTOR) |

The MXCSR default value `0x1F80` means:
- Bits 12:7 = `0b111111` — all six SSE exception masks set (no traps)
- Bits 14:13 = `0b00` — round-to-nearest-even
- All exception flags (bits 5:0) cleared

---

## Why Syscalls Don't Need SSE Saves

The SYSCALL instruction does not change the process — it transitions from
ring 3 to ring 0 within the same thread.  Since the kernel target has
`-sse,+soft-float`, no kernel code will modify XMM registers.  When the
syscall handler returns via SYSRETQ, XMM registers still hold the user
process's values.

The timer preemption path is different: it can switch from process A's
context to process B's context, so process A's XMM state would be
overwritten by process B if not saved.

---

## Future Considerations

### Lazy FPU switching (CR0.TS)

Instead of saving/restoring on every context switch, set CR0.TS = 1 after
switching away from a thread.  The next SSE instruction triggers a #NM
(Device Not Available) fault, at which point the handler saves the old
thread's state and loads the new thread's state, then clears CR0.TS.

**Pros**: Avoids the 512-byte save/restore overhead when threads don't use
SSE (e.g., kernel threads).
**Cons**: More complex, #NM handler latency, modern CPUs make FXSAVE fast
enough that eager switching is preferred (Linux switched to eager in 3.15).

### XSAVE for AVX

If AVX support is needed in the future, FXSAVE/FXRSTOR only covers
XMM0–XMM15.  XSAVE/XRSTOR can save the full YMM/ZMM state, but the save
area size varies by CPU (queried via CPUID leaf 0xD).  This would require:

1. `CPUID.0xD.0:EBX` to determine XSAVE area size
2. CR4.OSXSAVE = 1 and XCR0 configuration
3. Dynamic allocation of per-thread XSAVE areas
4. Replace FXSAVE/FXRSTOR with XSAVE/XRSTOR in the timer stub
