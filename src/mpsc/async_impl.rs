use super::*;
use crate::{
    loom::{
        atomic::{self, Ordering},
        sync::Arc,
    },
    wait::queue,
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

impl_recv_ref! {
    pub struct RecvRef<Waker>;
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

    pub async fn send_ref(&self) -> Result<SendRef<'_, T>, Closed> {
        #[pin_project::pin_project(PinnedDrop)]
        struct SendRefFuture<'sender, T> {
            tx: &'sender Sender<T>,
            queued: bool,
            #[pin]
            waiter: queue::Waiter<Waker>,
        }

        impl<'sender, T: Default + 'sender> Future for SendRefFuture<'sender, T> {
            type Output = Result<SendRef<'sender, T>, Closed>;

            fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                test_println!("SendRefFuture::poll({:p})", self);
                // perform one send ref loop iteration

                let this = self.as_mut().project();
                this.tx
                    .inner
                    .poll_send_ref(this.waiter, |waker| {
                        let my_waker = cx.waker();
                        if let Some(waker) = waker.as_mut() {
                            if test_dbg!(!waker.will_wake(my_waker)) {
                                test_println!(
                                    "poll_send_ref -> re-registering waker {:?}",
                                    my_waker
                                );
                                *waker = my_waker.clone();
                            }
                            false
                        } else {
                            test_println!(
                                "poll_send_ref -> registering initial waker {:?}",
                                my_waker
                            );
                            *waker = Some(my_waker.clone());
                            *this.queued = true;
                            true
                        }
                    })
                    .map(|ready| {
                        *this.queued = false;
                        ready.map(SendRef)
                    })
            }
        }

        #[pin_project::pinned_drop]
        impl<T> PinnedDrop for SendRefFuture<'_, T> {
            fn drop(self: Pin<&mut Self>) {
                test_println!("SendRefFuture::drop({:p})", self);
                if test_dbg!(self.queued) {
                    let this = self.project();
                    this.waiter.remove(&this.tx.inner.tx_wait)
                }
            }
        }

        SendRefFuture {
            tx: self,
            queued: false,
            waiter: queue::Waiter::new(),
        }
        .await
    }

    pub async fn send(&self, val: T) -> Result<(), Closed<T>> {
        match self.send_ref().await {
            Err(Closed(())) => Err(Closed(val)),
            Ok(mut slot) => {
                slot.with_mut(|slot| *slot = val);
                Ok(())
            }
        }
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
        test_dbg!(atomic::fence(Ordering::SeqCst));
        self.inner.thingbuf.core.close();
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
    pub fn poll_recv_ref(&self, cx: &mut Context<'_>) -> Poll<Option<RecvRef<'_, T>>> {
        self.inner.poll_recv_ref(|| cx.waker().clone()).map(|some| {
            some.map(|slot| RecvRef {
                _notify: super::NotifyTx(&self.inner.tx_wait),
                slot,
            })
        })
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

    pub fn is_closed(&self) -> bool {
        test_dbg!(self.inner.tx_count.load(Ordering::SeqCst)) <= 1
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.inner.close_rx();
    }
}

// === impl RecvRefFuture ===

impl<'a, T: Default> Future for RecvRefFuture<'a, T> {
    type Output = Option<RecvRef<'a, T>>;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ThingBuf;

    fn _assert_sync<T: Sync>(_: T) {}
    fn _assert_send<T: Send>(_: T) {}

    #[test]
    fn recv_ref_future_is_send() {
        fn _compiles() {
            let (_, rx) = channel::<usize>(ThingBuf::new(10));
            _assert_send(rx.recv_ref());
        }
    }

    #[test]
    fn recv_ref_future_is_sync() {
        fn _compiles() {
            let (_, rx) = channel::<usize>(ThingBuf::new(10));
            _assert_sync(rx.recv_ref());
        }
    }

    #[test]
    fn send_ref_future_is_send() {
        fn _compiles() {
            let (tx, _) = channel::<usize>(ThingBuf::new(10));
            _assert_send(tx.send_ref());
        }
    }

    #[test]
    fn send_ref_future_is_sync() {
        fn _compiles() {
            let (tx, _) = channel::<usize>(ThingBuf::new(10));
            _assert_sync(tx.send_ref());
        }
    }
}
