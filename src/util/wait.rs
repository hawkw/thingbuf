use crate::{
    loom::{
        atomic::{AtomicUsize, Ordering::*},
        UnsafeCell,
    },
    util::panic::{self, RefUnwindSafe, UnwindSafe},
};
use core::{fmt, task::Waker};

#[cfg(feature = "std")]
use crate::loom::thread;

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
pub(crate) struct WaitCell<T> {
    lock: AtomicUsize,
    waiter: UnsafeCell<Option<T>>,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum WaitResult {
    Wait,
    Notified,
    TxClosed,
}

pub(crate) trait Notify {
    fn notify(self);
}

// === impl WaitCell ===

impl<T: Notify + UnwindSafe + fmt::Debug> WaitCell<T> {
    const WAITING: usize = 0b00;
    const PARKING: usize = 0b01;
    const NOTIFYING: usize = 0b10;
    const TX_CLOSED: usize = 0b100;
    const RX_CLOSED: usize = 0b1000;

    pub(crate) fn new() -> Self {
        Self {
            lock: AtomicUsize::new(Self::WAITING),
            waiter: UnsafeCell::new(None),
        }
    }

    pub(crate) fn close_rx(&self) {
        self.lock.fetch_or(Self::RX_CLOSED, AcqRel);
    }

    pub(crate) fn is_rx_closed(&self) -> bool {
        test_dbg!(self.lock.load(Acquire) & Self::RX_CLOSED == Self::RX_CLOSED)
    }

    pub(crate) fn wait_with(&self, f: impl FnOnce() -> T) -> WaitResult {
        test_println!("registering waiter");

        // this is based on tokio's AtomicWaker synchronization strategy
        match test_dbg!(self
            .lock
            .compare_exchange(Self::WAITING, Self::PARKING, AcqRel, Acquire,))
        {
            // someone else is notifying the receiver, so don't park!
            Err(actual) if test_dbg!(actual & Self::TX_CLOSED) == Self::TX_CLOSED => {
                test_println!("-> state = TX_CLOSED");
                return WaitResult::TxClosed;
            }
            Err(actual) if test_dbg!(actual & Self::NOTIFYING) == Self::NOTIFYING => {
                test_println!("-> state = NOTIFYING");
                // f().notify();
                // loom::hint::spin_loop();
                return WaitResult::Notified;
            }

            Err(actual) => {
                debug_assert!(actual == Self::PARKING || actual == Self::PARKING | Self::NOTIFYING);
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

        let result = match test_dbg!(self.lock.compare_exchange(
            Self::PARKING,
            Self::WAITING,
            AcqRel,
            Acquire
        )) {
            Ok(_) => {
                let _ = panic::catch_unwind(move || drop(prev_waiter));

                WaitResult::Wait
            }
            Err(actual) => {
                test_println!("-> was notified; state={:#b}", actual);
                let waiter = self.waiter.with_mut(|waiter| unsafe { (*waiter).take() });
                // Reset to the WAITING state by clearing everything *except*
                // the closed bits (which must remain set).
                let state = test_dbg!(self
                    .lock
                    .fetch_and(Self::TX_CLOSED | Self::RX_CLOSED, AcqRel));
                // The only valid state transition while we were parking is to
                // add the TX_CLOSED bit.
                debug_assert!(
                    state == actual || state == actual | Self::TX_CLOSED,
                    "state changed unexpectedly while parking!"
                );

                if let Some(prev_waiter) = prev_waiter {
                    let _ = panic::catch_unwind(move || {
                        prev_waiter.notify();
                    });
                }

                if let Some(waiter) = waiter {
                    debug_assert!(panicked.is_none());
                    waiter.notify();
                }

                if state & Self::TX_CLOSED == Self::TX_CLOSED {
                    WaitResult::TxClosed
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

    pub(crate) fn notify(&self) {
        self.notify2(false)
    }

    pub(crate) fn close_tx(&self) {
        self.notify2(true)
    }

    fn notify2(&self, close: bool) {
        test_println!("notifying; close={:?};", close);
        let bits = if close {
            Self::NOTIFYING | Self::TX_CLOSED
        } else {
            Self::NOTIFYING
        };
        if test_dbg!(self.lock.fetch_or(bits, AcqRel)) == Self::WAITING {
            // we have the lock!
            let waiter = self.waiter.with_mut(|thread| unsafe { (*thread).take() });

            self.lock.fetch_and(!Self::NOTIFYING, Release);

            if let Some(waiter) = test_dbg!(waiter) {
                waiter.notify();
            }
        }
    }
}

#[cfg(feature = "std")]
impl Notify for thread::Thread {
    fn notify(self) {
        test_println!("NOTIFYING {:?} (from {:?})", self, thread::current());
        self.unpark();
    }
}

impl Notify for Waker {
    fn notify(self) {
        test_println!("WAKING TASK {:?} (from {:?})", self, thread::current());
        self.wake();
    }
}

impl<T: UnwindSafe> UnwindSafe for WaitCell<T> {}
impl<T: RefUnwindSafe> RefUnwindSafe for WaitCell<T> {}
unsafe impl<T: Send> Send for WaitCell<T> {}
unsafe impl<T: Send> Sync for WaitCell<T> {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loom::{
        self, future,
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

            if res == WaitResult::Notified || res == WaitResult::TxClosed {
                return Poll::Ready(());
            }

            Poll::Pending
        })
        .await
    }

    #[test]
    #[cfg(feature = "alloc")]
    fn basic_notification() {
        loom::model(|| {
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
        loom::model(|| {
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

    #[test]
    #[cfg(feature = "std")]
    fn test_panicky_waker() {
        use std::panic;
        use std::ptr;
        use std::task::{RawWaker, RawWakerVTable, Waker};

        static PANICKING_VTABLE: RawWakerVTable =
            RawWakerVTable::new(|_| panic!("clone"), |_| (), |_| (), |_| ());

        let panicking = unsafe { Waker::from_raw(RawWaker::new(ptr::null(), &PANICKING_VTABLE)) };

        loom::model(move || {
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

            // Note: this panic should have no effect on the overall state of the
            // waker and it should proceed as normal.
            //
            // A thread above might race to flag a wakeup, and a WAKING state will
            // be preserved if this expected panic races with that so the below
            // procedure should be allowed to continue uninterrupted.
            let _ = panic::catch_unwind(|| chan.task.wait_with(|| panicking.clone()));

            future::block_on(wait_on(chan));
        });
    }
}
