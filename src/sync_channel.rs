use super::*;
use crate::loom::{
    sync::Arc,
    thread::{self, Thread},
};

pub struct Sender<T> {
    inner: Arc<Inner<T>>,
}

pub struct Receiver<T> {
    inner: Arc<Inner<T>>,
}

struct Inner<T> {
    thingbuf: ThingBuf<T>,
    rx_thread: UnsafeCell<Option<Thread>>,
    rx_lock: AtomicUsize,
}

impl<T> Inner<T> {
    const RX_WAITING: usize = 0b00;
    const RX_PARKING: usize = 0b01;
    const RX_UNPARKING: usize = 0b10;

    fn park_rx(&self) {
        // this is based on tokio's AtomicWaker synchronization strategy
        match self.rx_lock.compare_exchange(
            Self::RX_WAITING,
            Self::RX_PARKING,
            Ordering::Acquire,
            Ordering::Acquire,
        ) {
            // someone else is unparking the receiver, so don't park!
            Err(Self::RX_UNPARKING) => return,
            Err(actual) => {
                debug_assert!(
                    actual == Self::RX_PARKING || actual == Self::RX_PARKING | Self::RX_UNPARKING
                );
                return;
            }
            Ok(_) => {}
        }

        let prev_rx = self
            .rx_thread
            .with_mut(|thread| unsafe { (*thread).replace(thread::current()) });

        match self.rx_lock.compare_exchange(
            Self::RX_PARKING,
            Self::RX_WAITING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                #[cfg(not(test))]
                thread::park();
                #[cfg(test)]
                {
                    // Loom doesn't have `Thread::unpark`, but because loom
                    // tests will only have a limited number of threads,
                    // calling `yield_now` should be roughly equivalent...i
                    // hope...lol.
                    test_println!("parking {:?}", thread::current());
                    loom::thread::yield_now();
                }
            }
            Err(actual) => {
                debug_assert_eq!(actual, Self::RX_PARKING | Self::RX_UNPARKING);
                let mut rx = self
                    .rx_thread
                    .with_mut(|thread| unsafe { (*thread).take() });

                if let Some(prev_rx) = prev_rx {
                    #[cfg(not(test))]
                    prev_rx.unpark();
                    #[cfg(test)]
                    {
                        // Loom doesn't have `Thread::unpark`, but because loom
                        // tests will only have a limited number of threads,
                        // calling `yield_now` should be roughly equivalent...i
                        // hope...lol.
                        test_println!("unparking {:?}", prev_rx);
                        loom::thread::yield_now();
                    }
                }
            }
        }
    }

    fn unpark_rx(&self) {
        match self.rx_lock.fetch_or(Self::RX_UNPARKING, Ordering::AcqRel) {
            Self::RX_WAITING => {
                // we have the lock!
                let rx = self
                    .rx_thread
                    .with_mut(|thread| unsafe { (*thread).take() });
                self.rx_lock
                    .fetch_and(!Self::RX_UNPARKING, Ordering::Release);

                if let Some(rx) = rx {
                    #[cfg(not(test))]
                    rx.unpark();
                    #[cfg(test)]
                    {
                        // Loom doesn't have `Thread::unpark`, but because loom
                        // tests will only have a limited number of threads,
                        // calling `yield_now` should be roughly equivalent...i
                        // hope...lol.
                        test_println!("unparking {:?}", rx);
                        loom::thread::yield_now();
                    }
                }
            }
            actual => {
                debug_assert!(
                    actual == Self::RX_PARKING
                        || actual == Self::RX_PARKING | Self::RX_UNPARKING
                        || actual == Self::RX_UNPARKING
                )
            }
        }
    }
}
