//! Drop-in `spin::Mutex` replacement with deadlock detection.
//!
//! `SpinMutex<T>` counts spin iterations and panics (with serial diagnostic)
//! when a threshold is exceeded, turning silent hangs into actionable panics.

/// Spin limit for `SpinMutex` (interrupts may be enabled, preemption possible).
/// ~100ms at 1 GHz — well beyond the 10ms scheduler quantum.
pub const SPIN_LIMIT: u32 = 100_000_000;

/// Spin limit for `IrqMutex` (interrupts disabled, no preemption on single-core).
/// Any contention with interrupts off means true deadlock; detect quickly.
pub const IRQ_SPIN_LIMIT: u32 = 10_000_000;

/// A spinlock wrapper around `spin::Mutex<T>` with deadlock detection.
///
/// API-compatible with `spin::Mutex`: exposes `const fn new()`, `lock()`,
/// and `try_lock()`, returning `spin::MutexGuard`.
pub struct SpinMutex<T>(spin::Mutex<T>);

// Safety: same as spin::Mutex — inner data only accessed through the guard.
unsafe impl<T: Send> Send for SpinMutex<T> {}
unsafe impl<T: Send> Sync for SpinMutex<T> {}

impl<T> SpinMutex<T> {
    pub const fn new(val: T) -> Self {
        SpinMutex(spin::Mutex::new(val))
    }

    pub fn lock(&self) -> spin::MutexGuard<'_, T> {
        let mut spins: u32 = 0;
        loop {
            if let Some(guard) = self.0.try_lock() {
                return guard;
            }
            spins += 1;
            if spins >= SPIN_LIMIT {
                deadlock_panic("SpinMutex");
            }
            core::hint::spin_loop();
        }
    }

    pub fn try_lock(&self) -> Option<spin::MutexGuard<'_, T>> {
        self.0.try_lock()
    }
}

/// Emergency panic: write diagnostic directly to serial port 0x3F8,
/// bypassing `SERIAL1`'s lock to avoid recursive deadlock.
pub fn deadlock_panic(label: &str) -> ! {
    unsafe {
        let mut port = x86_64::instructions::port::Port::<u8>::new(0x3F8);
        for &b in b"DEADLOCK DETECTED: " {
            port.write(b);
        }
        for &b in label.as_bytes() {
            port.write(b);
        }
        port.write(b'\n');
    }
    panic!("deadlock detected in {}", label);
}
