use core::ops::{Deref, DerefMut};
use spin::MutexGuard;

/// A spinlock that automatically disables local interrupts while held.
///
/// Equivalent to Linux's `spin_lock_irqsave` / `spin_unlock_irqrestore`.
/// Use this for any lock that might be contested between thread context
/// and interrupt context (e.g. the VGA writer).
pub struct IrqMutex<T>(spin::Mutex<T>);

/// RAII guard: releases the spinlock and restores the interrupt flag on drop.
pub struct IrqMutexGuard<'a, T> {
    guard: Option<MutexGuard<'a, T>>,
    was_enabled: bool,
}

// Safety: same as spin::Mutex — the inner data is only accessed through the guard.
unsafe impl<T: Send> Send for IrqMutex<T> {}
unsafe impl<T: Send> Sync for IrqMutex<T> {}

impl<T> IrqMutex<T> {
    pub const fn new(val: T) -> Self {
        IrqMutex(spin::Mutex::new(val))
    }

    pub fn lock(&self) -> IrqMutexGuard<'_, T> {
        let was_enabled = x86_64::instructions::interrupts::are_enabled();
        x86_64::instructions::interrupts::disable();
        IrqMutexGuard {
            guard: Some(self.0.lock()),
            was_enabled,
        }
    }
}

impl<T> Deref for IrqMutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.guard.as_ref().unwrap()
    }
}

impl<T> DerefMut for IrqMutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.guard.as_mut().unwrap()
    }
}

impl<T> Drop for IrqMutexGuard<'_, T> {
    fn drop(&mut self) {
        // Release the spinlock first, then restore interrupts.
        drop(self.guard.take());
        if self.was_enabled {
            x86_64::instructions::interrupts::enable();
        }
    }
}
