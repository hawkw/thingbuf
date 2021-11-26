//! A synchronous multi-producer, single-consumer channel.
//!
//! This provides an equivalent API to the [`mpsc`](crate::mpsc) module, but the
//! [`Receiver`] type in this module waits by blocking the current thread,
//! rather than asynchronously yielding.
use super::{Closed, TrySendError};
use crate::{
    loom::{
        self,
        atomic::{AtomicUsize, Ordering},
        sync::Arc,
        thread::{self, Thread},
    },
    util::wait::{WaitCell, WaitResult},
    Ref, ThingBuf,
};
use core::fmt;

/// Returns a new asynchronous multi-producer, single consumer channel.
pub fn channel<T>(thingbuf: ThingBuf<T>) -> (Sender<T>, Receiver<T>) {
    let inner = Arc::new(Inner {
        thingbuf,
        rx_wait: WaitCell::new(),
        tx_count: AtomicUsize::new(1),
    });
    let tx = Sender {
        inner: inner.clone(),
    };
    let rx = Receiver { inner };
    (tx, rx)
}

pub struct Sender<T> {
    inner: Arc<Inner<T>>,
}

pub struct Receiver<T> {
    inner: Arc<Inner<T>>,
}

pub struct SendRef<'a, T> {
    inner: &'a Inner<T>,
    slot: Ref<'a, T>,
}

struct Inner<T> {
    thingbuf: ThingBuf<T>,
    rx_wait: WaitCell<Thread>,
    tx_count: AtomicUsize,
}

// === impl Sender ===

impl<T: Default> Sender<T> {
    pub fn try_send_ref(&self) -> Result<SendRef<'_, T>, TrySendError> {
        self.inner
            .thingbuf
            .push_ref()
            .map(|slot| SendRef {
                inner: &*self.inner,
                slot,
            })
            .map_err(|e| {
                if self.inner.rx_wait.is_rx_closed() {
                    TrySendError::Closed(Closed(()))
                } else {
                    self.inner.rx_wait.notify();
                    TrySendError::AtCapacity(e)
                }
            })
    }

    pub fn try_send(&self, val: T) -> Result<(), TrySendError> {
        self.try_send_ref()?.with_mut(|slot| {
            *slot = val;
        });
        Ok(())
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        test_dbg!(self.inner.tx_count.fetch_add(1, Ordering::Relaxed));
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        if test_dbg!(self.inner.tx_count.fetch_sub(1, Ordering::Release)) > 1 {
            return;
        }

        // if we are the last sender, synchronize
        test_dbg!(self.inner.tx_count.load(Ordering::SeqCst));
        self.inner.rx_wait.close_tx();
    }
}

// === impl Receiver ===

impl<T: Default> Receiver<T> {
    pub fn recv_ref(&self) -> Option<Ref<'_, T>> {
        loop {
            // If we got a value, return it!
            if let Some(r) = self.inner.thingbuf.pop_ref() {
                return Some(r);
            }

            // otherwise, gotosleep
            match test_dbg!(self.inner.rx_wait.wait_with(thread::current)) {
                WaitResult::TxClosed => {
                    // All senders have been dropped, but the channel might
                    // still have messages in it...
                    return self.inner.thingbuf.pop_ref();
                }
                WaitResult::Wait => {
                    // make sure nobody sent a message while we were registering
                    // the waiter...
                    // XXX(eliza): a nicer solution _might_ just be to pack the
                    // waiter state into the tail idx or something or something
                    // but that kind of defeats the purpose of just having a
                    // nice "wrap a queue into a channel" API...
                    if let Some(val) = self.inner.thingbuf.pop_ref() {
                        return Some(val);
                    }
                    test_println!("parking ({:?})", thread::current());
                    thread::park();
                }
                WaitResult::Notified => {
                    loom::hint::spin_loop();
                }
            }
        }
    }

    pub fn try_recv_ref(&self) -> Option<Ref<'_, T>> {
        self.inner.thingbuf.pop_ref()
    }

    pub fn recv(&self) -> Option<T> {
        let val = self.recv_ref()?.with_mut(core::mem::take);
        Some(val)
    }

    pub fn is_closed(&self) -> bool {
        test_dbg!(self.inner.tx_count.load(Ordering::SeqCst)) <= 1
    }
}

impl<'a, T: Default> Iterator for &'a Receiver<T> {
    type Item = Ref<'a, T>;

    fn next(&mut self) -> Option<Self::Item> {
        self.recv_ref()
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.inner.rx_wait.close_rx();
    }
}

// === impl SendRef ===

impl<T> SendRef<'_, T> {
    #[inline]
    pub fn with<U>(&self, f: impl FnOnce(&T) -> U) -> U {
        self.slot.with(f)
    }

    #[inline]
    pub fn with_mut<U>(&mut self, f: impl FnOnce(&mut T) -> U) -> U {
        self.slot.with_mut(f)
    }
}

impl<T> Drop for SendRef<'_, T> {
    #[inline]
    fn drop(&mut self) {
        test_println!("drop SendRef");
        self.inner.rx_wait.notify();
    }
}

impl<T: fmt::Debug> fmt::Debug for SendRef<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.with(|val| fmt::Debug::fmt(val, f))
    }
}

impl<T: fmt::Display> fmt::Display for SendRef<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.with(|val| fmt::Display::fmt(val, f))
    }
}

impl<T: fmt::Write> fmt::Write for SendRef<'_, T> {
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
