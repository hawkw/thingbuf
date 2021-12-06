//! Multi-producer, single-consumer channels using [`ThingBuf`](crate::ThingBuf).
//!
//! The default MPSC channel returned by the [`channel`] function is
//! _asynchronous_: receiving from the channel is an `async fn`, and the
//! receiving task willwait when there are no messages in the channel.
//!
//! If the "std" feature flag is enabled, this module also provides a
//! synchronous channel, in the [`sync`] module. The synchronous  receiver will
//! instead wait for new messages by blocking the current thread. Naturally,
//! this requires the Rust standard library. A synchronous channel
//! can be constructed using the [`sync::channel`] function.

use crate::{
    loom::{atomic::AtomicUsize, hint},
    util::{
        wait::{Notify, WaitCell, WaitResult},
        Backoff,
    },
    Ref, ThingBuf,
};
use core::fmt;
use core::task::Poll;

#[cfg(feature = "alloc")]
use crate::util::wait::{NotifyOnDrop, WaitQueue};

#[derive(Debug)]
#[non_exhaustive]
pub enum TrySendError<T = ()> {
    Full(T),
    Closed(T),
}

#[derive(Debug)]
pub struct Closed<T = ()>(T);

#[derive(Debug)]
struct Inner<T, N: Notify> {
    thingbuf: ThingBuf<T>,
    rx_wait: WaitCell<N>,
    tx_count: AtomicUsize,
    #[cfg(feature = "alloc")]
    tx_wait: WaitQueue<NotifyOnDrop<N>>,
}

struct SendRefInner<'a, T, N: Notify> {
    inner: &'a Inner<T, N>,
    slot: Ref<'a, T>,
}

// ==== impl TrySendError ===

impl TrySendError {
    fn with_value<T>(self, value: T) -> TrySendError<T> {
        match self {
            Self::Full(()) => TrySendError::Full(value),
            Self::Closed(()) => TrySendError::Closed(value),
        }
    }
}

// ==== impl Inner ====
impl<T, N: Notify> Inner<T, N> {
    fn new(thingbuf: ThingBuf<T>) -> Self {
        Self {
            thingbuf,
            rx_wait: WaitCell::new(),
            tx_count: AtomicUsize::new(1),
            #[cfg(feature = "alloc")]
            tx_wait: WaitQueue::new(),
        }
    }

    fn close_rx(&self) {
        if self.thingbuf.core.close() {
            crate::loom::hint::spin_loop();
            test_println!("draining_queue");
            self.tx_wait.drain();
        }
    }
}

impl<T: Default, N: Notify> Inner<T, N> {
    fn try_send_ref(&self) -> Result<SendRefInner<'_, T, N>, TrySendError> {
        self.thingbuf
            .core
            .push_ref(self.thingbuf.slots.as_ref())
            .map(|slot| SendRefInner { inner: self, slot })
    }

    fn try_send(&self, val: T) -> Result<(), TrySendError<T>> {
        match self.try_send_ref() {
            Ok(mut slot) => {
                slot.with_mut(|slot| *slot = val);
                Ok(())
            }
            Err(e) => Err(e.with_value(val)),
        }
    }

    /// Performs one iteration of the `send_ref` loop.
    ///
    /// The loop itself has to be written in the actual `send` method's
    /// implementation, rather than on `inner`, because it might be async and
    /// may yield, or might park the thread.
    fn poll_send_ref(
        &self,
        mk_waiter: impl Fn() -> N,
    ) -> Poll<Result<SendRefInner<'_, T, N>, Closed>> {
        let mut backoff = Backoff::new();
        // try to send a few times in a loop, in case the receiver notifies us
        // right before we park.
        loop {
            // try to reserve a send slot, returning if we succeeded or if the
            // queue was closed.
            match self.try_send_ref() {
                Ok(slot) => return Poll::Ready(Ok(slot)),
                Err(TrySendError::Closed(_)) => return Poll::Ready(Err(Closed(()))),
                Err(_) => {}
            }

            // try to push a waiter
            let pushed_waiter = self.tx_wait.push_waiter(|| {
                let current = mk_waiter();
                test_println!("parking sender ({:?})", current);
                NotifyOnDrop::new(current)
            });

            match test_dbg!(pushed_waiter) {
                WaitResult::TxClosed => {
                    // the channel closed while we were registering the waiter!
                    return Poll::Ready(Err(Closed(())));
                }
                WaitResult::Wait => {
                    // okay, we are now queued to wait. gotosleep!
                    return Poll::Pending;
                }
                WaitResult::Notified => {
                    // we consumed a queued notification. try again...
                    backoff.spin_yield();
                }
            }
        }
    }

    /// Performs one iteration of the `recv_ref` loop.
    ///
    /// The loop itself has to be written in the actual `send` method's
    /// implementation, rather than on `inner`, because it might be async and
    /// may yield, or might park the thread.
    fn poll_recv_ref(&self, mk_waiter: impl Fn() -> N) -> Poll<Option<Ref<'_, T>>> {
        macro_rules! try_poll_recv {
            () => {
                // If we got a value, return it!
                match self.thingbuf.core.pop_ref(self.thingbuf.slots.as_ref()) {
                    Ok(slot) => return Poll::Ready(Some(slot)),
                    Err(TrySendError::Closed(_)) => return Poll::Ready(None),
                    _ => {}
                }
            };
        }

        test_println!("poll_recv_ref");
        loop {
            test_println!("poll_recv_ref => loop");

            // try to receive a reference, returning if we succeeded or the
            // channel is closed.
            try_poll_recv!();

            // otherwise, gotosleep
            match test_dbg!(self.rx_wait.wait_with(&mk_waiter)) {
                WaitResult::Wait => {
                    // we successfully registered a waiter! try polling again,
                    // just in case someone sent a message while we were
                    // registering the waiter.
                    try_poll_recv!();
                    test_dbg!(self.tx_wait.notify());
                    return Poll::Pending;
                }
                WaitResult::TxClosed => {
                    // the channel is closed (all the receivers are dropped).
                    // however, there may be messages left in the queue. try
                    // popping from the queue until it's empty.
                    return Poll::Ready(self.thingbuf.pop_ref());
                }
                WaitResult::Notified => {
                    // we were notified while we were trying to register the
                    // waiter. loop and try polling again.
                    hint::spin_loop();
                }
            }
        }
    }
}

impl<T, N: Notify> SendRefInner<'_, T, N> {
    #[inline]
    pub fn with<U>(&self, f: impl FnOnce(&T) -> U) -> U {
        self.slot.with(f)
    }

    #[inline]
    pub fn with_mut<U>(&mut self, f: impl FnOnce(&mut T) -> U) -> U {
        self.slot.with_mut(f)
    }
}

impl<T, N: Notify> Drop for SendRefInner<'_, T, N> {
    #[inline]
    fn drop(&mut self) {
        test_println!("drop SendRef<T, {}>", std::any::type_name::<N>());
        self.slot.release();
        self.inner.rx_wait.notify();
    }
}

impl<T: fmt::Debug, N: Notify> fmt::Debug for SendRefInner<'_, T, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.with(|val| fmt::Debug::fmt(val, f))
    }
}

impl<T: fmt::Display, N: Notify> fmt::Display for SendRefInner<'_, T, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.with(|val| fmt::Display::fmt(val, f))
    }
}

impl<T: fmt::Write, N: Notify> fmt::Write for SendRefInner<'_, T, N> {
    #[inline]
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.with_mut(|val| val.write_str(s))
    }

    #[inline]
    fn write_char(&mut self, c: char) -> fmt::Result {
        self.with_mut(|val| val.write_char(c))
    }

    #[inline]
    fn write_fmt(&mut self, f: fmt::Arguments<'_>) -> fmt::Result {
        self.with_mut(|val| val.write_fmt(f))
    }
}

macro_rules! impl_send_ref {
    (pub struct $name:ident<$notify:ty>;) => {
        pub struct $name<'sender, T>(SendRefInner<'sender, T, $notify>);

        impl<T> $name<'_, T> {
            #[inline]
            pub fn with<U>(&self, f: impl FnOnce(&T) -> U) -> U {
                self.0.with(f)
            }

            #[inline]
            pub fn with_mut<U>(&mut self, f: impl FnOnce(&mut T) -> U) -> U {
                self.0.with_mut(f)
            }
        }

        impl<T: fmt::Debug> fmt::Debug for $name<'_, T> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl<T: fmt::Display> fmt::Display for $name<'_, T> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl<T: fmt::Write> fmt::Write for $name<'_, T> {
            #[inline]
            fn write_str(&mut self, s: &str) -> fmt::Result {
                self.0.write_str(s)
            }

            #[inline]
            fn write_char(&mut self, c: char) -> fmt::Result {
                self.0.write_char(c)
            }

            #[inline]
            fn write_fmt(&mut self, f: fmt::Arguments<'_>) -> fmt::Result {
                self.0.write_fmt(f)
            }
        }
    };
}

macro_rules! impl_recv_ref {
    (pub struct $name:ident<$notify:ty>;) => {
        pub struct $name<'recv, T> {
            slot: Ref<'recv, T>,
            inner: &'recv Inner<T, $notify>,
        }

        impl<T> $name<'_, T> {
            #[inline]
            pub fn with<U>(&self, f: impl FnOnce(&T) -> U) -> U {
                self.slot.with(f)
            }

            #[inline]
            pub fn with_mut<U>(&mut self, f: impl FnOnce(&mut T) -> U) -> U {
                self.slot.with_mut(f)
            }
        }

        impl<T: fmt::Debug> fmt::Debug for $name<'_, T> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.slot.fmt(f)
            }
        }

        impl<T: fmt::Display> fmt::Display for $name<'_, T> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.slot.fmt(f)
            }
        }

        impl<T: fmt::Write> fmt::Write for $name<'_, T> {
            #[inline]
            fn write_str(&mut self, s: &str) -> fmt::Result {
                self.slot.write_str(s)
            }

            #[inline]
            fn write_char(&mut self, c: char) -> fmt::Result {
                self.slot.write_char(c)
            }

            #[inline]
            fn write_fmt(&mut self, f: fmt::Arguments<'_>) -> fmt::Result {
                self.slot.write_fmt(f)
            }
        }

        impl<T> Drop for RecvRef<'_, T> {
            fn drop(&mut self) {
                test_println!("drop RecvRef<T, {}>", stringify!($notify));
                self.slot.release();
                self.inner.tx_wait.notify();
            }
        }
    };
}

mod async_impl;
pub use self::async_impl::*;

feature! {
    #![feature = "std"]
    pub mod sync;
}

#[cfg(test)]
mod tests;
