use super::Notify;
use crate::{
    loom::{
        atomic::{AtomicUsize, Ordering::*},
        UnsafeCell,
    },
    util::Backoff,
};
use alloc::collections::VecDeque;
use core::fmt;

// TODO(eliza): this can almost certainly be replaced with an intrusive list of
// some kind, but crossbeam uses a spinlock + vec, so it's _probably_ fine...
pub(crate) struct WaitQueue<T: Notify> {
    locked: AtomicUsize,
    queue: UnsafeCell<VecDeque<T>>,
}

pub(crate) struct Locked<'a, T: Notify>(&'a WaitQueue<T>);

const UNLOCKED: usize = 0b00;
const LOCKED: usize = 0b01;
const CLOSED: usize = 0b10;

impl<T: Notify> WaitQueue<T> {
    #[cfg(not(test))]
    pub(crate) const fn new() -> Self {
        Self {
            locked: AtomicUsize::new(UNLOCKED),
            queue: UnsafeCell::new(VecDeque::new()),
        }
    }

    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self {
            locked: AtomicUsize::new(UNLOCKED),
            queue: UnsafeCell::new(VecDeque::new()),
        }
    }

    pub(crate) fn lock(&self) -> Option<Locked<'_, T>> {
        let mut backoff = Backoff::new();
        loop {
            match self
                .locked
                .compare_exchange_weak(UNLOCKED, LOCKED, Acquire, Relaxed)
            {
                Ok(_) => return Some(Locked(self)),
                Err(LOCKED) => {
                    while self.locked.load(Relaxed) == LOCKED {
                        backoff.spin_yield();
                    }
                }
                Err(_actual) => {
                    debug_assert_eq!(_actual, CLOSED);
                    return None;
                }
            }
        }
    }
}

impl<T: Notify> Drop for WaitQueue<T> {
    fn drop(&mut self) {
        if let Some(lock) = self.lock() {
            self.locked.fetch_or(CLOSED, Release);
            self.queue.with_mut(|q| {
                let mut waiters = unsafe { (*q).drain(..) };
                for waiter in waiters {
                    waiter.notify();
                }
            })
        }
    }
}

impl<T: Notify> fmt::Debug for WaitQueue<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WaitQueue(..)")
    }
}

impl<T: Notify> Locked<'_, T> {
    pub(crate) fn push_waiter(&mut self, waiter: T) -> usize {
        self.0.queue.with_mut(|q| unsafe {
            (*q).push_back(waiter);
            (*q).len()
        })
    }

    pub(crate) fn notify(&mut self) -> bool {
        self.0.queue.with_mut(|q| {
            if let Some(mut waiter) = unsafe { (*q).pop_front() } {
                waiter.notify();
                true
            } else {
                false
            }
        })
    }

    pub(crate) fn remove(&mut self, i: usize) -> Option<T> {
        self.0.queue.with_mut(|q| unsafe { (*q).remove(i) })
    }
}

impl<T: Notify> Drop for Locked<'_, T> {
    fn drop(&mut self) {
        self.0.locked.fetch_and(!LOCKED, Release);
    }
}
