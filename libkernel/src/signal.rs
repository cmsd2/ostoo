//! POSIX signal data structures and constants.

/// Number of signals supported (1-based: signals 1..64).
pub const NUM_SIGNALS: usize = 64;

// Standard signal numbers.
pub const SIGHUP: u8 = 1;
pub const SIGINT: u8 = 2;
pub const SIGQUIT: u8 = 3;
pub const SIGILL: u8 = 4;
pub const SIGTRAP: u8 = 5;
pub const SIGABRT: u8 = 6;
pub const SIGBUS: u8 = 7;
pub const SIGFPE: u8 = 8;
pub const SIGKILL: u8 = 9;
pub const SIGUSR1: u8 = 10;
pub const SIGSEGV: u8 = 11;
pub const SIGUSR2: u8 = 12;
pub const SIGPIPE: u8 = 13;
pub const SIGALRM: u8 = 14;
pub const SIGTERM: u8 = 15;
pub const SIGCHLD: u8 = 17;
pub const SIGCONT: u8 = 18;
pub const SIGSTOP: u8 = 19;

// Signal handler special values.
pub const SIG_DFL: u64 = 0;
pub const SIG_IGN: u64 = 1;

// sa_flags bits.
pub const SA_NOCLDSTOP: u64 = 0x0000_0001;
pub const SA_NOCLDWAIT: u64 = 0x0000_0002;
pub const SA_SIGINFO: u64 = 0x0000_0004;
pub const SA_RESTORER: u64 = 0x0400_0000;

// sigprocmask `how` values.
pub const SIG_BLOCK: u64 = 0;
pub const SIG_UNBLOCK: u64 = 1;
pub const SIG_SETMASK: u64 = 2;

/// Per-signal disposition (matches musl's `struct kernel_sigaction` layout).
#[derive(Clone, Copy)]
pub struct SigAction {
    /// Handler function pointer (SA_HANDLER or SA_SIGACTION).
    pub handler: u64,
    /// Flags (SA_SIGINFO, SA_RESTORER, etc.).
    pub flags: u64,
    /// sa_restorer — address of `__restore_rt` trampoline in userspace.
    pub restorer: u64,
    /// Signal mask to block during handler execution.
    pub mask: u64,
}

impl SigAction {
    pub const fn default() -> Self {
        SigAction {
            handler: SIG_DFL,
            flags: 0,
            restorer: 0,
            mask: 0,
        }
    }
}

/// Per-process signal state.
#[derive(Clone)]
pub struct SignalState {
    /// Per-signal dispositions (indexed by signal number - 1).
    /// Boxed to avoid bloating Process struct and overflowing kernel stacks
    /// in debug builds (64 × 32 bytes = 2 KiB inline).
    pub actions: alloc::boxed::Box<[SigAction; NUM_SIGNALS]>,
    /// Bitmask of pending signals (bit N = signal N+1).
    pub pending: u64,
    /// Bitmask of blocked signals (signal mask).
    pub blocked: u64,
}

impl SignalState {
    pub fn new() -> Self {
        SignalState {
            actions: alloc::boxed::Box::new([SigAction::default(); NUM_SIGNALS]),
            pending: 0,
            blocked: 0,
        }
    }

    /// Queue a signal as pending.
    pub fn queue(&mut self, signum: u8) {
        if signum >= 1 && (signum as usize) <= NUM_SIGNALS {
            self.pending |= 1u64 << (signum - 1);
        }
    }

    /// Dequeue the lowest-numbered deliverable (pending & !blocked) signal.
    /// Returns the signal number (1-based) or None.
    pub fn dequeue(&mut self) -> Option<u8> {
        let deliverable = self.pending & !self.blocked;
        if deliverable == 0 {
            return None;
        }
        let bit = deliverable.trailing_zeros() as u8; // 0-based bit index
        self.pending &= !(1u64 << bit);
        Some(bit + 1) // 1-based signal number
    }

    /// Check whether a signal's default action is to terminate.
    pub fn is_default_terminate(signum: u8) -> bool {
        matches!(
            signum,
            SIGHUP | SIGINT | SIGQUIT | SIGILL | SIGTRAP | SIGABRT | SIGBUS | SIGFPE
                | SIGKILL | SIGUSR1 | SIGSEGV | SIGUSR2 | SIGPIPE | SIGALRM | SIGTERM
        )
    }

    /// Check whether a signal's default action is to ignore.
    pub fn is_default_ignore(signum: u8) -> bool {
        matches!(signum, SIGCHLD | SIGCONT)
    }
}
