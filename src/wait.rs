use crate::loom::{
    atomic::{AtomicUsize, Ordering::*},
    UnsafeCell,
};

#[cfg(feature = "std")]
use crate::loom::thread;

/// An atomically registered waiter (`Waker` or `Thread`).
pub(crate) struct WaitCell<T> {
    lock: AtomicUsize,
    waiter: UnsafeCell<Option<T>>,
}

#[derive(Debug)]
pub(crate) enum WaitResult {
    Wait,
    Notified,
    TxClosed,
}

pub(crate) trait Notify {
    fn notify(self);
}

// === impl WaitCell ===

impl<T: Notify> WaitCell<T> {
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
                return WaitResult::Notified;
            }

            Err(actual) => {
                debug_assert!(actual == Self::PARKING || actual == Self::PARKING | Self::NOTIFYING);
                return WaitResult::Wait;
            }
            Ok(_) => {}
        }

        test_println!("-> locked!");
        let prev_waiter = self
            .waiter
            .with_mut(|waiter| unsafe { (*waiter).replace(f()) });

        match self
            .lock
            .compare_exchange(Self::PARKING, Self::WAITING, AcqRel, Acquire)
        {
            Ok(_) => WaitResult::Wait,
            Err(actual) => {
                test_println!("-> was notified; state={:#b}", actual);
                // debug_assert_eq!(actual, Self::PARKING | Self::NOTIFYING);
                let _ = self.waiter.with_mut(|waiter| unsafe { (*waiter).take() });

                if let Some(prev_waiter) = prev_waiter {
                    prev_waiter.notify();
                }

                if actual & Self::TX_CLOSED == Self::TX_CLOSED {
                    return WaitResult::TxClosed;
                }

                WaitResult::Notified
            }
        }
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

            if let Some(waiter) = waiter {
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
        // XXX(eliza): loom doesn't like the yield_now here because the unpark
        // may *immediately* switch execution to the unparked thread, lol.
        // thread::yield_now();
    }
}
