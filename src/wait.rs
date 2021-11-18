use crate::loom::{
    atomic::{AtomicUsize, Ordering},
    UnsafeCell,
};

#[cfg(feature = "std")]
use crate::loom::thread;

/// An atomically registered waiter (`Waker` or `Thread`).
pub(crate) struct WaitCell<T> {
    lock: AtomicUsize,
    waiter: UnsafeCell<Option<T>>,
}

pub(crate) trait Notify {
    fn notify(self);
}

// === impl WaitCell ===

impl<T: Notify> WaitCell<T> {
    const WAITING: usize = 0b00;
    const PARKING: usize = 0b01;
    const NOTIFYING: usize = 0b10;

    pub(crate) fn new() -> Self {
        Self {
            lock: AtomicUsize::new(Self::WAITING),
            waiter: UnsafeCell::new(None),
        }
    }

    pub(crate) fn wait_with(&self, f: impl FnOnce() -> T) -> bool {
        test_println!("registering waiter");

        // this is based on tokio's AtomicWaker synchronization strategy
        match self.lock.compare_exchange(
            Self::WAITING,
            Self::PARKING,
            Ordering::Acquire,
            Ordering::Acquire,
        ) {
            // someone else is notifying the receiver, so don't park!
            Err(Self::NOTIFYING) => {
                test_println!("-> state = NOTIFYING");
                return false;
            }
            Err(actual) => {
                debug_assert!(actual == Self::PARKING || actual == Self::PARKING | Self::NOTIFYING);
                return true;
            }
            Ok(_) => {}
        }

        test_println!("-> locked!");
        let prev_waiter = self
            .waiter
            .with_mut(|waiter| unsafe { (*waiter).replace(f()) });

        match self.lock.compare_exchange(
            Self::PARKING,
            Self::WAITING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => true,
            Err(actual) => {
                test_println!("-> was notified; state={:#b}", actual);
                debug_assert_eq!(actual, Self::PARKING | Self::NOTIFYING);
                let _ = self.waiter.with_mut(|waiter| unsafe { (*waiter).take() });

                if let Some(prev_waiter) = prev_waiter {
                    prev_waiter.notify();
                }

                false
            }
        }
    }

    pub(crate) fn notify(&self) {
        test_println!("notifying");
        match self.lock.fetch_or(Self::NOTIFYING, Ordering::AcqRel) {
            Self::WAITING => {
                // we have the lock!
                let waiter = self.waiter.with_mut(|thread| unsafe { (*thread).take() });
                self.lock.fetch_and(!Self::NOTIFYING, Ordering::Release);

                if let Some(waiter) = waiter {
                    waiter.notify();
                }
            }
            actual => {
                debug_assert!(
                    actual == Self::PARKING
                        || actual == Self::PARKING | Self::NOTIFYING
                        || actual == Self::NOTIFYING
                )
            }
        }
    }
}

#[cfg(feature = "std")]
impl Notify for thread::Thread {
    fn notify(self) {
        #[cfg(not(test))]
        self.unpark();
        #[cfg(test)]
        {
            // Loom doesn't have `Thread::unpark`, but because loom
            // tests will only have a limited number of threads,
            // calling `yield_now` should be roughly equivalent...i
            // hope...lol.
            debug_assert_ne!(
                thread::current().id(),
                self.id(),
                "attempted to unpark a thread from itself"
            );
            test_println!("NOTIFYING {:?}", self);
            thread::yield_now();
        }
    }
}
