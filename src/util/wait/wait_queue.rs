use super::{Notify, WaitResult};
use crate::{
    loom::{
        atomic::{
            AtomicUsize,
            Ordering::{self, *},
        },
        cell::UnsafeCell,
    },
    util::{panic, Backoff, CachePadded},
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
    locked: CachePadded<AtomicUsize>,
    queue: UnsafeCell<VecDeque<T>>,
}

pub(crate) struct Locked<'a, T> {
    queue: &'a WaitQueue<T>,
    state: State,
}

#[derive(Copy, Clone)]
struct State(usize);

impl<T> WaitQueue<T> {
    pub(crate) fn new() -> Self {
        Self {
            locked: CachePadded(AtomicUsize::new(State::UNLOCKED.0 | State::EMPTY.0)),
            queue: UnsafeCell::new(VecDeque::new()),
        }
    }

    fn compare_exchange_weak(
        &self,
        curr: State,
        next: State,
        success: Ordering,
        failure: Ordering,
    ) -> Result<State, State> {
        let res = self
            .locked
            .compare_exchange_weak(curr.0, next.0, success, failure)
            .map(State)
            .map_err(State);
        test_println!(
            "self.state.compare_exchange_weak({:?}, {:?}, {:?}, {:?}) = {:?}",
            curr,
            next,
            success,
            failure,
            res
        );
        res
    }

    fn fetch_clear(&self, state: State, order: Ordering) -> State {
        let res = State(self.locked.fetch_and(!state.0, order));
        test_println!(
            "self.state.fetch_clear({:?}, {:?}) = {:?}",
            state,
            order,
            res
        );
        res
    }

    fn lock(&self) -> Result<Locked<'_, T>, State> {
        let mut backoff = Backoff::new();
        let mut state = State(self.locked.load(Ordering::Relaxed));
        loop {
            test_dbg!(&state);
            if test_dbg!(state.contains(State::CLOSED)) {
                return Err(state);
            }

            if !test_dbg!(state.contains(State::LOCKED)) {
                match self.compare_exchange_weak(
                    state,
                    State(state.0 | State::LOCKED.0),
                    AcqRel,
                    Acquire,
                ) {
                    Ok(_) => return Ok(Locked { queue: self, state }),
                    Err(actual) => {
                        state = actual;
                        backoff.spin();
                    }
                }
            } else {
                state = State(self.locked.load(Ordering::Relaxed));
                backoff.spin_yield();
            }
        }
    }

    pub(crate) fn push_waiter(&self, mk_waiter: impl FnOnce() -> T) -> WaitResult {
        if let Ok(mut lock) = self.lock() {
            if lock.state.queued() > 0 {
                lock.state = lock.state.sub_queued();
                return WaitResult::Notified;
            }
            lock.queue.queue.with_mut(|q| unsafe {
                (*q).push_back(mk_waiter());
            });
            WaitResult::Wait
        } else {
            WaitResult::TxClosed
        }
    }

    pub(crate) fn drain(&self) {
        if let Ok(lock) = self.lock() {
            // if test_dbg!(lock.state.contains(State::EMPTY)) {
            //     return;
            // }
            lock.queue.queue.with_mut(|q| {
                let waiters = unsafe { (*q).drain(..) };
                for waiter in waiters {
                    drop(waiter);
                }
            })
        }
    }
}

impl<T: Notify> WaitQueue<T> {
    pub(crate) fn notify(&self) -> bool {
        test_println!("notifying tx");

        if let Ok(mut lock) = self.lock() {
            return lock.notify();
        }

        false
    }
}

impl<T> Drop for WaitQueue<T> {
    fn drop(&mut self) {
        self.drain();
    }
}

impl<T> fmt::Debug for WaitQueue<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WaitQueue(..)")
    }
}

impl<T: Notify> Locked<'_, T> {
    fn notify(&mut self) -> bool {
        // if test_dbg!(self.state.contains(State::EMPTY)) {
        //     self.state = self.state.add_queued();
        //     return false;
        // }
        self.queue.queue.with_mut(|q| {
            let q = unsafe { &mut *q };
            if let Some(waiter) = q.pop_front() {
                waiter.notify();
                if q.is_empty() {
                    self.queue.fetch_clear(State::EMPTY, Release);
                }
                true
            } else {
                self.state = self.state.add_queued();
                false
            }
        })
    }

    // TODO(eliza): future cancellation nonsense...
    #[allow(dead_code)]
    pub(crate) fn remove(&mut self, i: usize) -> Option<T> {
        self.queue.queue.with_mut(|q| unsafe { (*q).remove(i) })
    }
}

impl<T> Drop for Locked<'_, T> {
    fn drop(&mut self) {
        test_dbg!(State(self.queue.locked.swap(self.state.0, Release)));
    }
}

impl<T: panic::UnwindSafe> panic::UnwindSafe for WaitQueue<T> {}
impl<T: panic::RefUnwindSafe> panic::RefUnwindSafe for WaitQueue<T> {}
unsafe impl<T: Send> Send for WaitQueue<T> {}
unsafe impl<T: Send> Sync for WaitQueue<T> {}

// === impl State ===

impl State {
    const UNLOCKED: Self = Self(0b00);
    const LOCKED: Self = Self(0b01);
    const EMPTY: Self = Self(0b10);
    const CLOSED: Self = Self(0b100);

    const FLAG_BITS: usize = Self::LOCKED.0 | Self::EMPTY.0 | Self::CLOSED.0;
    const QUEUED_SHIFT: usize = Self::FLAG_BITS.trailing_ones() as usize;
    const QUEUED_ONE: usize = 1 << Self::QUEUED_SHIFT;

    fn queued(self) -> usize {
        self.0 >> Self::QUEUED_SHIFT
    }

    fn add_queued(self) -> Self {
        Self(self.0 + Self::QUEUED_ONE)
    }

    fn contains(self, Self(state): Self) -> bool {
        self.0 & state == state
    }

    fn sub_queued(self) -> Self {
        let flags = self.0 & Self::FLAG_BITS;
        Self(self.0 & (!Self::FLAG_BITS).saturating_sub(Self::QUEUED_ONE) | flags)
    }
}

impl fmt::Debug for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("State(")?;
        let mut has_flags = false;

        fmt_bits!(self, f, has_flags, LOCKED, EMPTY, CLOSED);

        if !has_flags {
            f.write_str("UNLOCKED")?;
        }

        let queued = self.queued();
        if queued > 0 {
            write!(f, ", queued: {})", queued)?;
        } else {
            f.write_str(")")?;
        }

        Ok(())
    }
}
