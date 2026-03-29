# rt_sigreturn (nr 15)

Restore process context after a signal handler returns.

## Signature

```
rt_sigreturn() → (restores original rax)
```

## Arguments

None.  The kernel reads the saved context from the signal frame on the user
stack.

## Return value

Does not return in the normal sense — restores all registers (including rax)
from the signal frame, resuming execution at the point where the signal was
delivered.

## Description

When the kernel delivers a signal, it pushes a signal frame onto the user
stack containing the interrupted context (all registers, signal mask, RIP,
RSP, RFLAGS) and sets RIP to the user's signal handler.  A trampoline
(`__restore_rt`) is placed on the stack that calls `rt_sigreturn` when the
handler returns.

`rt_sigreturn` reads the saved context from the signal frame, restores the
signal mask, and overwrites the SYSCALL saved registers so that the return
to user space resumes the original interrupted code path.

### Signal frame layout (on user stack)

```
[pretcode]          8 bytes — address of __restore_rt trampoline
[siginfo]           128 bytes — siginfo_t
[ucontext]          variable — contains:
  uc_flags          8 bytes
  uc_link           8 bytes
  uc_stack          24 bytes (ss_sp, ss_flags, ss_size)
  [sigcontext]      256 bytes (32 × u64: r8–r15, rdi, rsi, rbp, rbx, rdx, rax, rcx, rsp, rip, rflags, ...)
  uc_sigmask        8 bytes — saved signal mask
[__restore_rt code] 9 bytes — `mov eax, 15; syscall`
```

## Implementation

`osl/src/signal.rs` — `sys_rt_sigreturn`

## See also

- [rt_sigaction (13)](rt_sigaction.md)
- [Signal Support](../signals.md)
