use super::*;
use crate::{
    loom::{self, atomic::Ordering, sync::Arc},
    util::wait::WaitResult,
    Ref, ThingBuf,
};
use core::{
    fmt,
    future::Future,
    pin::Pin,
    task::{Context, Poll, Waker},
};

/// Returns a new synchronous multi-producer, single consumer channel.
pub fn channel<T>(thingbuf: ThingBuf<T>) -> (Sender<T>, Receiver<T>) {
    let inner = Arc::new(Inner::new(thingbuf));
    let tx = Sender {
        inner: inner.clone(),
    };
    let rx = Receiver { inner };
    (tx, rx)
}

#[derive(Debug)]
pub struct Sender<T> {
    inner: Arc<Inner<T, Waker>>,
}

#[derive(Debug)]
pub struct Receiver<T> {
    inner: Arc<Inner<T, Waker>>,
}

impl_send_ref! {
    pub struct SendRef<Waker>;
}

/// A [`Future`] that tries to receive a reference from a [`Receiver`].
///
/// This type is returned by [`Receiver::recv_ref`].
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct RecvRefFuture<'a, T> {
    rx: &'a Receiver<T>,
}

/// A [`Future`] that tries to receive a value from a [`Receiver`].
///
/// This type is returned by [`Receiver::recv`].
///
/// This is equivalent to the [`RecvRefFuture`] future, but the value is moved out of
/// the [`ThingBuf`] after it is received. This means that allocations are not
/// reused.
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct RecvFuture<'a, T> {
    rx: &'a Receiver<T>,
}

// === impl Sender ===

impl<T: Default> Sender<T> {
    pub fn try_send_ref(&self) -> Result<SendRef<'_, T>, TrySendError> {
        self.inner.try_send_ref().map(SendRef)
    }

    pub fn try_send(&self, val: T) -> Result<(), TrySendError<T>> {
        self.inner.try_send(val)
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
    pub fn recv_ref(&self) -> RecvRefFuture<'_, T> {
        RecvRefFuture { rx: self }
    }

    pub fn recv(&self) -> RecvFuture<'_, T> {
        RecvFuture { rx: self }
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
    /// `poll_recv_ref`, only the [`Waker`] from the [`Context`] passed to the most
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
    /// `poll_recv`, only the [`Waker`] from the [`Context`] passed to the most
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

// === impl RecvRefFuture ===

impl<'a, T: Default> Future for RecvRefFuture<'a, T> {
    type Output = Option<Ref<'a, T>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.rx.poll_recv_ref(cx)
    }
}

// === impl Recv ===

impl<'a, T: Default> Future for RecvFuture<'a, T> {
    type Output = Option<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.rx.poll_recv(cx)
    }
}
