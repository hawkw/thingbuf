use super::*;
use crate::{
    error::TrySendError,
    loom::{
        atomic::{AtomicUsize, Ordering},
        sync::Arc,
    },
    util::wait::{WaitCell, WaitResult},
};
use core::{
    future::Future,
    pin::Pin,
    task::{Context, Poll, Waker},
};

#[cfg(test)]
mod tests;

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

/// A [`Future`] that tries to receive a reference from a [`Receiver`].
///
/// This type is returned by [`Receiver::recv_ref`].
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct RecvRef<'a, T> {
    rx: &'a Receiver<T>,
}

/// A [`Future`] that tries to receive a value from a [`Receiver`].
///
/// This type is returned by [`Receiver::recv`].
///
/// This is equivalent to the [`RecvRef`] future, but the value is moved out of
/// the [`ThingBuf`] after it is received. This means that allocations are not
/// reused.
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct Recv<'a, T> {
    rx: &'a Receiver<T>,
}

struct Inner<T> {
    thingbuf: ThingBuf<T>,
    rx_wait: WaitCell<Waker>,
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
                    TrySendError::Closed(error::Closed(()))
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
    pub fn recv_ref(&self) -> RecvRef<'_, T> {
        RecvRef { rx: self }
    }

    pub fn recv(&self) -> Recv<'_, T> {
        Recv { rx: self }
    }

    /// # Returns
    ///
    ///  * `Poll::Pending` if no messages are available but the channel is not
    ///    closed, or if a spurious failure happens.
    ///  * `Poll::Ready(Some(Ref<T>))` if a message is available.
    ///  * `Poll::Ready(None)` if the channel has been closed and all messages
    ///    sent before it was closed have been received.
    ///
    /// When the method returns [`Poll::Pending`], the [`Waker`] in the provided
    /// [`Context`] is scheduled to receive a wakeup when a message is sent on any
    /// sender, or when the channel is closed.  Note that on multiple calls to
    /// [`poll_recv_ref`], only the [`Waker`] from the [`Context`] passed to the most
    /// recent call is scheduled to receive a wakeup.
    pub fn poll_recv_ref(&self, cx: &mut Context<'_>) -> Poll<Option<Ref<'_, T>>> {
        loop {
            if let Some(r) = self.try_recv_ref() {
                return Poll::Ready(Some(r));
            }

            // Okay, no value is ready --- try to wait.
            match test_dbg!(self.inner.rx_wait.wait_with(|| cx.waker().clone())) {
                WaitResult::TxClosed => {
                    // All senders have been dropped, but the channel might
                    // still have messages in it. Return `Ready`; if the recv'd ref
                    // is `None` then we've popped everything.
                    return Poll::Ready(self.try_recv_ref());
                }
                WaitResult::Wait => {
                    // make sure nobody sent a message while we were registering
                    // the waiter...
                    // XXX(eliza): a nicer solution _might_ just be to pack the
                    // waiter state into the tail idx or something or something
                    // but that kind of defeats the purpose of just having a
                    // nice "wrap a queue into a channel" API...
                    if let Some(val) = self.try_recv_ref() {
                        return Poll::Ready(Some(val));
                    }
                    return Poll::Pending;
                }
                WaitResult::Notified => {
                    loom::hint::spin_loop();
                }
            };
        }
    }

    /// # Returns
    ///
    ///  * `Poll::Pending` if no messages are available but the channel is not
    ///    closed, or if a spurious failure happens.
    ///  * `Poll::Ready(Some(message))` if a message is available.
    ///  * `Poll::Ready(None)` if the channel has been closed and all messages
    ///    sent before it was closed have been received.
    ///
    /// When the method returns [`Poll::Pending`], the [`Waker`] in the provided
    /// [`Context`] is scheduled to receive a wakeup when a message is sent on any
    /// sender, or when the channel is closed.  Note that on multiple calls to
    /// [`poll_recv_ref`], only the [`Waker`] from the [`Context`] passed to the most
    /// recent call is scheduled to receive a wakeup.
    pub fn poll_recv(&self, cx: &mut Context<'_>) -> Poll<Option<T>> {
        self.poll_recv_ref(cx)
            .map(|opt| opt.map(|mut r| r.with_mut(core::mem::take)))
    }

    fn try_recv_ref(&self) -> Option<Ref<'_, T>> {
        self.inner.thingbuf.pop_ref()
    }

    pub fn is_closed(&self) -> bool {
        test_dbg!(self.inner.tx_count.load(Ordering::SeqCst)) <= 1
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
        test_println!("drop async SendRef");
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

// === impl RecvRef ===

impl<'a, T: Default> Future for RecvRef<'a, T> {
    type Output = Option<Ref<'a, T>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.rx.poll_recv_ref(cx)
    }
}

// === impl Recv ===

impl<'a, T: Default> Future for Recv<'a, T> {
    type Output = Option<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.rx.poll_recv(cx)
    }
}
