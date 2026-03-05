use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};
use x86_64::instructions::interrupts;

#[derive(Debug)]
pub struct SpinLock<T> {
    lock: AtomicBool,
    data: UnsafeCell<T>,
}

unsafe impl<T: Send> Send for SpinLock<T> {}
unsafe impl<T: Send> Sync for SpinLock<T> {}

impl<T> SpinLock<T> {
    pub const fn new(data: T) -> Self {
        SpinLock {
            lock: AtomicBool::new(false),
            data: UnsafeCell::new(data),
        }
    }

    pub fn lock(&self) -> SpinLockGuard<'_, T> {
        let interrupts_enabled = interrupts::are_enabled();
        interrupts::disable();
        while self
            .lock
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }

        SpinLockGuard {
            lock: self,
            interrupts_state: interrupts_enabled,
        }
    }

    pub fn unlock(&self) {
        self.lock.store(false, Ordering::Release);
    }
}

/// RAII guard that unlocks when dropped
pub struct SpinLockGuard<'a, T> {
    lock: &'a SpinLock<T>,
    interrupts_state: bool,
}

impl<'a, T> core::ops::Deref for SpinLockGuard<'a, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.lock.data.get() }
    }
}

impl<'a, T> core::ops::DerefMut for SpinLockGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<'a, T> Drop for SpinLockGuard<'a, T> {
    fn drop(&mut self) {
        self.lock.unlock();
        if self.interrupts_state {
            interrupts::enable();
        }
    }
}
