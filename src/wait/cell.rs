use crate::{
    loom::{
        atomic::{
            AtomicUsize,
            Ordering::{self, *},
        },
        cell::UnsafeCell,
    },
    util::{
        panic::{self, RefUnwindSafe, UnwindSafe},
        CachePadded,
    },
    wait::{Notify, WaitResult},
};
use core::{fmt, ops};

/// An atomically registered waiter ([`Waker`] or [`Thread`]).
///
/// This is inspired by the [`AtomicWaker` type] used in Tokio's
/// synchronization primitives, with the following modifications:
///
/// - Unlike [`AtomicWaker`], a `WaitCell` is generic over the type of the
///   waiting value. This means it can be used in both asynchronous code (with
///   [`Waker`]s), or in synchronous, multi-threaded code (with a [`Thread`]).
/// - An additional bit of state is added to allow setting a "close" bit. This
///   is so that closing a channel can be tracked in the same atomic as the
///   receiver's notification state, reducing the number of separate atomic RMW
///   ops that have to be synchronized between.
/// - A `WaitCell` is always woken by value. This is just because I didn't
///   actually need separate "take waiter" and "wake" steps for any of the uses
///   in `ThingBuf`...
///
/// [`AtomicWaker`]: https://github.com/tokio-rs/tokio/blob/09b770c5db31a1f35631600e1d239679354da2dd/tokio/src/sync/task/atomic_waker.rs
/// [`Waker`]: core::task::Waker
/// [`Thread`]: std::thread::Thread
pub(crate) struct WaitCell<T> {
    lock: CachePadded<AtomicUsize>,
    waiter: UnsafeCell<Option<T>>,
}

#[derive(Eq, PartialEq, Copy, Clone)]
struct State(usize);

// === impl WaitCell ===

impl<T> WaitCell<T> {
    #[cfg(not(all(loom, test)))]
    pub(crate) const fn new() -> Self {
        Self {
            lock: CachePadded(AtomicUsize::new(State::WAITING.0)),
            waiter: UnsafeCell::new(None),
        }
    }

    #[cfg(all(loom, test))]
    pub(crate) fn new() -> Self {
        Self {
            lock: CachePadded(AtomicUsize::new(State::WAITING.0)),
            waiter: UnsafeCell::new(None),
        }
    }
}

impl<T: Notify> WaitCell<T> {
    pub(crate) fn wait_with(&self, f: impl FnOnce() -> T) -> WaitResult {
        test_println!("registering waiter");

        // this is based on tokio's AtomicWaker synchronization strategy
        match test_dbg!(self.compare_exchange(State::WAITING, State::PARKING, Acquire)) {
            // someone else is notifying the receiver, so don't park!
            Err(actual) if test_dbg!(actual.contains(State::TX_CLOSED)) => {
                return WaitResult::Closed;
            }
            Err(actual) if test_dbg!(actual.contains(State::NOTIFYING)) => {
                f().notify();
                crate::loom::hint::spin_loop();
                return WaitResult::Notified;
            }

            Err(actual) => {
                debug_assert!(
                    actual == State::PARKING || actual == State::PARKING | State::NOTIFYING
                );
                return WaitResult::Wait;
            }
            Ok(_) => {}
        }

        test_println!("-> locked!");
        let (panicked, prev_waiter) = match panic::catch_unwind(panic::AssertUnwindSafe(f)) {
            Ok(new_waiter) => {
                let new_waiter = test_dbg!(new_waiter);
                let prev_waiter = self
                    .waiter
                    .with_mut(|waiter| unsafe { (*waiter).replace(new_waiter) });
                (None, test_dbg!(prev_waiter))
            }
            Err(panic) => (Some(panic), None),
        };

        let result = match test_dbg!(self.compare_exchange(State::PARKING, State::WAITING, AcqRel))
        {
            Ok(_) => {
                // XXX(eliza): it's kind of sad we have to use
                // `AssertUnwindSafe` here, because `std::thread::Thread`
                // contains a `Condvar` on macOS :(
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(move || drop(prev_waiter)));

                WaitResult::Wait
            }
            Err(actual) => {
                test_println!("-> was notified; state={:?}", actual);
                let waiter = self.waiter.with_mut(|waiter| unsafe { (*waiter).take() });
                // Reset to the WAITING state by clearing everything *except*
                // the closed bits (which must remain set).
                let state = test_dbg!(self.fetch_and(State::TX_CLOSED, AcqRel));
                // The only valid state transition while we were parking is to
                // add the TX_CLOSED bit.
                debug_assert!(
                    state == actual || state == actual | State::TX_CLOSED,
                    "state changed unexpectedly while parking!"
                );

                if let Some(prev_waiter) = prev_waiter {
                    // XXX(eliza): similarly, it's necessary to assert unwind
                    // safety due to `std::thread::Thread` on macOS here...
                    let _ = panic::catch_unwind(panic::AssertUnwindSafe(move || {
                        prev_waiter.notify();
                    }));
                }

                if let Some(waiter) = waiter {
                    debug_assert!(panicked.is_none());
                    waiter.notify();
                }

                if test_dbg!(state.contains(State::TX_CLOSED)) {
                    WaitResult::Closed
                } else {
                    WaitResult::Notified
                }
            }
        };

        if let Some(panic) = panicked {
            panic::resume_unwind(panic);
        }

        result
    }

    pub(crate) fn notify(&self) -> bool {
        self.notify2(State::WAITING)
    }

    pub(crate) fn close_tx(&self) {
        self.notify2(State::TX_CLOSED);
    }

    fn notify2(&self, close: State) -> bool {
        test_println!("notifying; close={:?};", close);
        let bits = State::NOTIFYING | close;
        test_dbg!(bits);
        if test_dbg!(self.fetch_or(bits, AcqRel)) == State::WAITING {
            // we have the lock!
            let waiter = self.waiter.with_mut(|thread| unsafe { (*thread).take() });

            test_dbg!(self.fetch_and(!State::NOTIFYING, AcqRel));

            if let Some(waiter) = test_dbg!(waiter) {
                waiter.notify();
                return true;
            }
        }
        false
    }
}

impl<T> WaitCell<T> {
    #[inline(always)]
    fn compare_exchange(
        &self,
        State(curr): State,
        State(new): State,
        success: Ordering,
    ) -> Result<State, State> {
        self.lock
            .compare_exchange(curr, new, success, Acquire)
            .map(State)
            .map_err(State)
    }

    #[inline(always)]
    fn fetch_and(&self, State(state): State, order: Ordering) -> State {
        State(self.lock.fetch_and(state, order))
    }

    #[inline(always)]
    fn fetch_or(&self, State(state): State, order: Ordering) -> State {
        State(self.lock.fetch_or(state, order))
    }

    #[inline(always)]
    fn current_state(&self) -> State {
        State(self.lock.load(Acquire))
    }
}

impl<T: UnwindSafe> UnwindSafe for WaitCell<T> {}
impl<T: RefUnwindSafe> RefUnwindSafe for WaitCell<T> {}
unsafe impl<T: Send> Send for WaitCell<T> {}
unsafe impl<T: Send> Sync for WaitCell<T> {}

impl<T> fmt::Debug for WaitCell<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WaitCell")
            .field("state", &self.current_state())
            .finish()
    }
}

// === impl State ===

impl State {
    const WAITING: Self = Self(0b00);
    const PARKING: Self = Self(0b01);
    const NOTIFYING: Self = Self(0b10);
    const TX_CLOSED: Self = Self(0b100);

    fn contains(self, Self(state): Self) -> bool {
        self.0 & state == state
    }
}

impl ops::BitOr for State {
    type Output = Self;

    fn bitor(self, Self(rhs): Self) -> Self::Output {
        Self(self.0 | rhs)
    }
}

impl ops::Not for State {
    type Output = Self;

    fn not(self) -> Self::Output {
        Self(!self.0)
    }
}

impl fmt::Debug for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut has_states = false;

        fmt_bits!(self, f, has_states, PARKING, NOTIFYING, TX_CLOSED);

        if !has_states {
            if *self == Self::WAITING {
                return f.write_str("WAITING");
            }

            f.debug_tuple("UnknownState")
                .field(&format_args!("{:#b}", self.0))
                .finish()?;
        }

        Ok(())
    }
}
#[cfg(all(loom, test))]
mod tests {
    use super::*;
    use crate::loom::{
        future,
        sync::atomic::{AtomicUsize, Ordering::Relaxed},
        thread,
    };
    #[cfg(feature = "alloc")]
    use alloc::sync::Arc;
    use core::task::{Poll, Waker};

    struct Chan {
        num: AtomicUsize,
        task: WaitCell<Waker>,
    }

    const NUM_NOTIFY: usize = 2;

    async fn wait_on(chan: Arc<Chan>) {
        futures_util::future::poll_fn(move |cx| {
            let res = test_dbg!(chan.task.wait_with(|| cx.waker().clone()));

            if NUM_NOTIFY == chan.num.load(Relaxed) {
                return Poll::Ready(());
            }

            if res == WaitResult::Notified || res == WaitResult::Closed {
                return Poll::Ready(());
            }

            Poll::Pending
        })
        .await
    }

    #[test]
    #[cfg(feature = "alloc")]
    fn basic_notification() {
        crate::loom::model(|| {
            let chan = Arc::new(Chan {
                num: AtomicUsize::new(0),
                task: WaitCell::new(),
            });

            for _ in 0..NUM_NOTIFY {
                let chan = chan.clone();

                thread::spawn(move || {
                    chan.num.fetch_add(1, Relaxed);
                    chan.task.notify();
                });
            }

            future::block_on(wait_on(chan));
        });
    }

    #[test]
    #[cfg(feature = "alloc")]
    fn tx_close() {
        crate::loom::model(|| {
            let chan = Arc::new(Chan {
                num: AtomicUsize::new(0),
                task: WaitCell::new(),
            });

            thread::spawn({
                let chan = chan.clone();
                move || {
                    chan.num.fetch_add(1, Relaxed);
                    chan.task.notify();
                }
            });

            thread::spawn({
                let chan = chan.clone();
                move || {
                    chan.num.fetch_add(1, Relaxed);
                    chan.task.close_tx();
                }
            });

            future::block_on(wait_on(chan));
        });
    }

    // #[test]
    // #[cfg(feature = "std")]
    // fn test_panicky_waker() {
    //     use std::panic;
    //     use std::ptr;
    //     use std::task::{RawWaker, RawWakerVTable, Waker};

    //     static PANICKING_VTABLE: RawWakerVTable =
    //         RawWakerVTable::new(|_| panic!("clone"), |_| (), |_| (), |_| ());

    //     let panicking = unsafe { Waker::from_raw(RawWaker::new(ptr::null(), &PANICKING_VTABLE)) };

    //     loom::model(move || {
    //         let chan = Arc::new(Chan {
    //             num: AtomicUsize::new(0),
    //             task: WaitCell::new(),
    //         });

    //         for _ in 0..NUM_NOTIFY {
    //             let chan = chan.clone();

    //             thread::spawn(move || {
    //                 chan.num.fetch_add(1, Relaxed);
    //                 chan.task.notify();
    //             });
    //         }

    //         // Note: this panic should have no effect on the overall state of the
    //         // waker and it should proceed as normal.
    //         //
    //         // A thread above might race to flag a wakeup, and a WAKING state will
    //         // be preserved if this expected panic races with that so the below
    //         // procedure should be allowed to continue uninterrupted.
    //         let _ = panic::catch_unwind(|| chan.task.wait_with(|| panicking.clone()));

    //         future::block_on(wait_on(chan));
    //     });
    // }
}
