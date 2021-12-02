use super::Notify;
use crate::{
    loom::{
        atomic::{AtomicUsize, Ordering::*},
        UnsafeCell,
    },
    util::{panic, Backoff},
};
use alloc::collections::VecDeque;
use core::fmt;

/// A mediocre wait queue, implemented as a spinlock around a `VecDeque` of
/// waiters.
// TODO(eliza): this can almost certainly be replaced with an intrusive list of
// some kind, but crossbeam uses a spinlock + vec, so it's _probably_ fine...
// XXX(eliza): the biggest downside of this is that it can't be used without
// `liballoc`, which is sad for `no-std` async-await users...
pub(crate) struct WaitQueue<T> {
    locked: AtomicUsize,
    queue: UnsafeCell<VecDeque<T>>,
}

pub(crate) struct Locked<'a, T>(&'a WaitQueue<T>);

const UNLOCKED: usize = 0b00;
const LOCKED: usize = 0b01;
const CLOSED: usize = 0b10;

impl<T> WaitQueue<T> {
    pub(crate) fn new() -> Self {
        Self {
            locked: AtomicUsize::new(UNLOCKED),
            queue: UnsafeCell::new(VecDeque::new()),
        }
    }

    pub(crate) fn lock(&self) -> Option<Locked<'_, T>> {
        let mut backoff = Backoff::new();
        loop {
            match test_dbg!(self
                .locked
                .compare_exchange_weak(UNLOCKED, LOCKED, Acquire, Relaxed))
            {
                Ok(_) => return Some(Locked(self)),
                Err(LOCKED) => {
                    while test_dbg!(self.locked.load(Relaxed) == LOCKED) {
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

impl<T> Drop for WaitQueue<T> {
    fn drop(&mut self) {
        if let Some(lock) = self.lock() {
            self.locked.fetch_or(CLOSED, Release);
            lock.0.queue.with_mut(|q| {
                let waiters = unsafe { (*q).drain(..) };
                for waiter in waiters {
                    drop(waiter);
                }
            })
        }
    }
}

impl<T> fmt::Debug for WaitQueue<T> {
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
            if let Some(waiter) = unsafe { (*q).pop_front() } {
                waiter.notify();
                true
            } else {
                false
            }
        })
    }

    // TODO(eliza): future cancellation nonsense...
    #[allow(dead_code)]
    pub(crate) fn remove(&mut self, i: usize) -> Option<T> {
        self.0.queue.with_mut(|q| unsafe { (*q).remove(i) })
    }
}

impl<T> Drop for Locked<'_, T> {
    fn drop(&mut self) {
        test_dbg!(self.0.locked.fetch_and(!LOCKED, Release));
    }
}

impl<T: panic::UnwindSafe> panic::UnwindSafe for WaitQueue<T> {}
impl<T: panic::RefUnwindSafe> panic::RefUnwindSafe for WaitQueue<T> {}
unsafe impl<T: Send> Send for WaitQueue<T> {}
unsafe impl<T: Send> Sync for WaitQueue<T> {}
