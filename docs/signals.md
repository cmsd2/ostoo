# Signal Support

## Current state

Phase 1 of POSIX signal support: basic signal data structures, `rt_sigaction`,
`rt_sigprocmask`, signal delivery on SYSCALL return, `rt_sigreturn`, and `kill`.

### What works

- `rt_sigaction` (syscall 13): install/query signal handlers with SA_SIGINFO and SA_RESTORER
- `rt_sigprocmask` (syscall 14): SIG_BLOCK, SIG_UNBLOCK, SIG_SETMASK
- `kill` (syscall 62): send signals to specific pids
- Signal delivery on SYSCALL return path via `check_pending_signals`
- `rt_sigreturn` (syscall 15): restore context after signal handler returns
- Default actions: SIG_DFL (terminate or ignore depending on signal), SIG_IGN
- `sigaltstack` (syscall 131): stub returning 0

### Signal delivery mechanism

The SYSCALL assembly stub saves 8 registers onto the kernel stack and stores the
stack pointer into `PerCpuData.saved_frame_ptr` (GS offset 40). After `syscall_dispatch`
returns, `check_pending_signals()` is called:

1. Peek at process's `pending & !blocked` — early return if empty
2. Dequeue lowest pending signal
3. If SIG_DFL: terminate (SIGKILL, SIGTERM, etc.) or ignore (SIGCHLD, SIGCONT)
4. If SIG_IGN: return
5. If handler installed: construct `rt_sigframe` on user stack, rewrite saved frame

The rt_sigframe on the user stack contains:
- `pretcode` (8B): `sa_restorer` address (musl's `__restore_rt`)
- `siginfo_t` (128B): signal number, errno, code
- `ucontext_t` (224B): saved registers (sigcontext), fpstate ptr, signal mask

The saved SYSCALL frame is rewritten so `sysretq` "returns" into the handler:
- RCX (→ RIP) = handler address
- RDI = signal number
- RSI = &siginfo (if SA_SIGINFO)
- RDX = &ucontext (if SA_SIGINFO)
- User RSP = rt_sigframe base

When the handler returns, `__restore_rt` calls `rt_sigreturn` (syscall 15),
which reads the saved context from the rt_sigframe and restores the original
registers and signal mask.

## Architecture

### Key files

| File | Purpose |
|---|---|
| `libkernel/src/signal.rs` | Signal constants, SigAction, SignalState |
| `libkernel/src/syscall.rs` | PerCpuData.saved_frame_ptr, SyscallSavedFrame, check_pending_signals, deliver_signal |
| `libkernel/src/process.rs` | Process.signal field |
| `osl/src/signal.rs` | sys_rt_sigreturn, sys_kill |
| `osl/src/signal.rs` | sys_rt_sigaction, sys_rt_sigprocmask |

### PerCpuData layout

| Offset | Field | Purpose |
|---|---|---|
| 0 | kernel_rsp | Loaded on SYSCALL entry |
| 8 | user_rsp | Saved by entry stub |
| 16 | user_rip | RCX saved by entry stub |
| 24 | user_rflags | R11 saved by entry stub |
| 32 | user_r9 | R9 saved (for clone) |
| 40 | saved_frame_ptr | RSP after register pushes (for signal delivery) |

### saved_frame_ptr is not saved/restored per-thread

`saved_frame_ptr` lives in a single per-CPU slot and is **not** saved/restored
during context switches. This is safe today because it is set and consumed
entirely within the SYSCALL entry/exit path with interrupts disabled:

1. The assembly stub pushes registers, writes `mov gs:40, rsp`, then calls
   `syscall_dispatch` followed by `check_pending_signals` — all before the
   register pops and `sysretq`.
2. `rt_sigreturn` is itself a syscall, so the stub sets `saved_frame_ptr`
   at the start of the same SYSCALL path before `sys_rt_sigreturn` reads it.

No preemption can occur between setting and consuming the pointer.

**If signal delivery is ever needed from interrupt context** (e.g. delivering
SIGSEGV from a page-fault handler or SIGINT from a keyboard ISR), this design
must be revisited — either by saving/restoring `saved_frame_ptr` per-thread in
the scheduler, or by using a different mechanism to locate the interrupted
frame (e.g. the interrupt stack frame pushed by the CPU).

## Future work

- **Phase 2**: Exception-generated signals (SIGSEGV, SIGILL, SIGFPE from ring-3 faults)
- **Phase 3**: SIGINT from Ctrl+C (keyboard handler → foreground process)
- **Phase 4**: SIGCHLD (queued when child exits)
- FPU state save/restore in signal frames
- Signal queuing (currently only one instance per signal — standard signals)
