#![cfg_attr(feature = "std", allow(dead_code))]
use crate::{
    loom::{
        atomic::{AtomicBool, Ordering::*},
        cell::{MutPtr, UnsafeCell},
    },
    util::Backoff,
};
use core::{fmt, ops};

#[derive(Debug)]
pub(crate) struct Mutex<T> {
    locked: AtomicBool,
    data: UnsafeCell<T>,
}

pub(crate) struct MutexGuard<'lock, T> {
    locked: &'lock AtomicBool,
    data: MutPtr<T>,
}

#[cfg(not(loom))]
pub(crate) const fn const_mutex<T>(data: T) -> Mutex<T> {
    Mutex {
        locked: AtomicBool::new(false),
        data: UnsafeCell::new(data),
    }
}

impl<T> Mutex<T> {
    #[cfg(loom)]
    pub(crate) fn new(data: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            data: UnsafeCell::new(data),
        }
    }

    #[inline]
    pub(crate) fn lock(&self) -> MutexGuard<'_, T> {
        test_println!("locking {}...", core::any::type_name::<T>());
        let mut backoff = Backoff::new();
        while test_dbg!(self.locked.compare_exchange(false, true, AcqRel, Acquire)).is_err() {
            while self.locked.load(Relaxed) {
                backoff.spin_yield();
            }
        }

        test_println!("-> locked {}!", core::any::type_name::<T>());
        MutexGuard {
            locked: &self.locked,
            data: self.data.get_mut(),
        }
    }
}

impl<T> MutexGuard<'_, T> {
    // this is factored out into its own function so that the debug impl is a
    // little easier lol
    #[inline]
    fn get_ref(&self) -> &T {
        unsafe {
            // Safety: the mutex is locked, so we cannot create a concurrent
            // mutable access, and we have a borrow on the lock's state boolean,
            // so it will not be dropped while the guard exists.
            &*self.data.deref()
        }
    }
}

impl<T> ops::Deref for MutexGuard<'_, T> {
    type Target = T;
    #[inline]
    fn deref(&self) -> &T {
        self.get_ref()
    }
}

impl<T> ops::DerefMut for MutexGuard<'_, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        unsafe {
            // Safety: the mutex is locked, so we cannot create a concurrent
            // mutable access, and we have a borrow on the lock's state boolean,
            // so it will not be dropped while the guard exists.
            &mut *self.data.deref()
        }
    }
}

impl<T> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        test_dbg!(self.locked.store(false, Release));
        test_println!("unlocked!");
    }
}

impl<T: fmt::Debug> fmt::Debug for MutexGuard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.get_ref().fmt(f)
    }
}

unsafe impl<T: Send> Send for Mutex<T> {}
unsafe impl<T: Send> Sync for Mutex<T> {}
unsafe impl<T: Send> Send for MutexGuard<'_, T> {}
unsafe impl<T: Send> Sync for MutexGuard<'_, T> {}
