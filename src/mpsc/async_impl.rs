use super::*;
use crate::{
    loom::atomic::{self, AtomicBool, Ordering},
    wait::queue,
    Ref,
};
use core::{
    fmt,
    future::Future,
    pin::Pin,
    task::{Context, Poll, Waker},
};

pub struct StaticChannel<T, const CAPACITY: usize> {
    core: ChannelCore<Waker>,
    slots: [Slot<T>; CAPACITY],
    is_split: AtomicBool,
}

feature! {
    #![feature = "alloc"]

    use crate::loom::sync::Arc;

    /// Returns a new synchronous multi-producer, single consumer channel.
    pub fn channel<T: Default>(capacity: usize) -> (Sender<T>, Receiver<T>) {
        assert!(capacity > 0);
        let slots = (0..capacity).map(|_| Slot::empty()).collect();
        let inner = Arc::new(Inner {
            core: ChannelCore::new(capacity),
            slots,
        });
        let tx = Sender {
            inner: inner.clone(),
        };
        let rx = Receiver { inner };
        (tx, rx)
    }

    #[derive(Debug)]

    pub struct Receiver<T> {
        inner: Arc<Inner<T>>,
    }

    #[derive(Debug)]
    pub struct Sender<T> {
        inner: Arc<Inner<T>>,
    }

    struct Inner<T> {
        core: super::ChannelCore<Waker>,
        slots: Box<[Slot<T>]>,
    }
}

pub struct StaticSender<T: 'static> {
    core: &'static ChannelCore<Waker>,
    slots: &'static [Slot<T>],
}

pub struct StaticReceiver<T: 'static> {
    core: &'static ChannelCore<Waker>,
    slots: &'static [Slot<T>],
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
    core: &'a ChannelCore<Waker>,
    slots: &'a [Slot<T>],
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
    core: &'a ChannelCore<Waker>,
    slots: &'a [Slot<T>],
}

#[pin_project::pin_project(PinnedDrop)]
struct SendRefFuture<'sender, T> {
    core: &'sender ChannelCore<Waker>,
    slots: &'sender [Slot<T>],
    state: State,
    #[pin]
    waiter: queue::Waiter<Waker>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum State {
    Start,
    Waiting,
    Done,
}

// === impl StaticChannel ===

impl<T, const CAPACITY: usize> StaticChannel<T, CAPACITY> {
    const SLOT: Slot<T> = Slot::empty();
    #[cfg(not(all(loom, test)))]
    pub const fn new() -> Self {
        Self {
            core: ChannelCore::new(CAPACITY),
            slots: [Self::SLOT; CAPACITY],
            is_split: AtomicBool::new(false),
        }
    }

    pub fn split(&'static self) -> (StaticSender<T>, StaticReceiver<T>) {
        self.try_split().expect("channel already split")
    }

    pub fn try_split(&'static self) -> Option<(StaticSender<T>, StaticReceiver<T>)> {
        self.is_split
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()?;
        let tx = StaticSender {
            core: &self.core,
            slots: &self.slots[..],
        };
        let rx = StaticReceiver {
            core: &self.core,
            slots: &self.slots[..],
        };
        Some((tx, rx))
    }
}
// === impl Sender ===

#[cfg(feature = "alloc")]
impl<T: Default> Sender<T> {
    pub fn try_send_ref(&self) -> Result<SendRef<'_, T>, TrySendError> {
        self.inner
            .core
            .try_send_ref(self.inner.slots.as_ref())
            .map(SendRef)
    }

    pub fn try_send(&self, val: T) -> Result<(), TrySendError<T>> {
        self.inner.core.try_send(self.inner.slots.as_ref(), val)
    }

    pub async fn send_ref(&self) -> Result<SendRef<'_, T>, Closed> {
        SendRefFuture {
            core: &self.inner.core,
            slots: self.inner.slots.as_ref(),
            state: State::Start,
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

#[cfg(feature = "alloc")]
impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        test_dbg!(self.inner.core.tx_count.fetch_add(1, Ordering::Relaxed));
        Self {
            inner: self.inner.clone(),
        }
    }
}

#[cfg(feature = "alloc")]
impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        if test_dbg!(self.inner.core.tx_count.fetch_sub(1, Ordering::Release)) > 1 {
            return;
        }

        // if we are the last sender, synchronize
        test_dbg!(atomic::fence(Ordering::SeqCst));
        self.inner.core.core.close();
        self.inner.core.rx_wait.close_tx();
    }
}

// === impl Receiver ===

#[cfg(feature = "alloc")]
impl<T: Default> Receiver<T> {
    pub fn recv_ref(&self) -> RecvRefFuture<'_, T> {
        RecvRefFuture {
            core: &self.inner.core,
            slots: self.inner.slots.as_ref(),
        }
    }

    pub fn recv(&self) -> RecvFuture<'_, T> {
        RecvFuture {
            core: &self.inner.core,
            slots: self.inner.slots.as_ref(),
        }
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
        poll_recv_ref(&self.inner.core, &self.inner.slots, cx)
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
        test_dbg!(self.inner.core.tx_count.load(Ordering::SeqCst)) <= 1
    }
}

#[cfg(feature = "alloc")]
impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.inner.core.close_rx();
    }
}

// === impl StaticSender ===

impl<T: Default> StaticSender<T> {
    pub fn try_send_ref(&self) -> Result<SendRef<'_, T>, TrySendError> {
        self.core.try_send_ref(self.slots).map(SendRef)
    }

    pub fn try_send(&self, val: T) -> Result<(), TrySendError<T>> {
        self.core.try_send(self.slots, val)
    }

    pub async fn send_ref(&self) -> Result<SendRef<'_, T>, Closed> {
        SendRefFuture {
            core: self.core,
            slots: self.slots,
            state: State::Start,
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

impl<T> Clone for StaticSender<T> {
    fn clone(&self) -> Self {
        test_dbg!(self.core.tx_count.fetch_add(1, Ordering::Relaxed));
        Self {
            core: self.core,
            slots: self.slots,
        }
    }
}

impl<T> Drop for StaticSender<T> {
    fn drop(&mut self) {
        if test_dbg!(self.core.tx_count.fetch_sub(1, Ordering::Release)) > 1 {
            return;
        }

        // if we are the last sender, synchronize
        test_dbg!(atomic::fence(Ordering::SeqCst));
        self.core.core.close();
        self.core.rx_wait.close_tx();
    }
}

// === impl StaticReceiver ===

impl<T: Default> StaticReceiver<T> {
    pub fn recv_ref(&self) -> RecvRefFuture<'_, T> {
        RecvRefFuture {
            core: self.core,
            slots: self.slots,
        }
    }

    pub fn recv(&self) -> RecvFuture<'_, T> {
        RecvFuture {
            core: self.core,
            slots: self.slots,
        }
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
        poll_recv_ref(self.core, self.slots, cx)
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
        test_dbg!(self.core.tx_count.load(Ordering::SeqCst)) <= 1
    }
}

impl<T> Drop for StaticReceiver<T> {
    fn drop(&mut self) {
        self.core.close_rx();
    }
}

// === impl RecvRefFuture ===

#[inline]
fn poll_recv_ref<'a, T: Default>(
    core: &'a ChannelCore<Waker>,
    slots: &'a [Slot<T>],
    cx: &mut Context<'_>,
) -> Poll<Option<RecvRef<'a, T>>> {
    core.poll_recv_ref(slots, || cx.waker().clone())
        .map(|some| {
            some.map(|slot| RecvRef {
                _notify: super::NotifyTx(&core.tx_wait),
                slot,
            })
        })
}

impl<'a, T: Default> Future for RecvRefFuture<'a, T> {
    type Output = Option<RecvRef<'a, T>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        poll_recv_ref(self.core, self.slots, cx)
    }
}

// === impl Recv ===

impl<'a, T: Default> Future for RecvFuture<'a, T> {
    type Output = Option<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        poll_recv_ref(self.core, self.slots, cx)
            .map(|opt| opt.map(|mut r| r.with_mut(core::mem::take)))
    }
}

// === impl SendRefFuture ===

impl<'sender, T: Default + 'sender> Future for SendRefFuture<'sender, T>
where
    T: Default + 'sender,
{
    type Output = Result<SendRef<'sender, T>, Closed>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        test_println!("SendRefFuture::poll({:p})", self);

        loop {
            let this = self.as_mut().project();
            let node = this.waiter;
            match test_dbg!(*this.state) {
                State::Start => {
                    match this.core.try_send_ref(this.slots) {
                        Ok(slot) => return Poll::Ready(Ok(SendRef(slot))),
                        Err(TrySendError::Closed(_)) => return Poll::Ready(Err(Closed(()))),
                        Err(_) => {}
                    }

                    let start_wait = this.core.tx_wait.start_wait(node, cx.waker());

                    match test_dbg!(start_wait) {
                        WaitResult::Closed => {
                            // the channel closed while we were registering the waiter!
                            *this.state = State::Done;
                            return Poll::Ready(Err(Closed(())));
                        }
                        WaitResult::Wait => {
                            // okay, we are now queued to wait.
                            // gotosleep!
                            *this.state = State::Waiting;
                            return Poll::Pending;
                        }
                        WaitResult::Notified => continue,
                    }
                }
                State::Waiting => {
                    let continue_wait = this.core.tx_wait.continue_wait(node, cx.waker());

                    match test_dbg!(continue_wait) {
                        WaitResult::Closed => {
                            *this.state = State::Done;
                            return Poll::Ready(Err(Closed(())));
                        }
                        WaitResult::Wait => return Poll::Pending,
                        WaitResult::Notified => {
                            *this.state = State::Done;
                        }
                    }
                }
                State::Done => match this.core.try_send_ref(this.slots) {
                    Ok(slot) => return Poll::Ready(Ok(SendRef(slot))),
                    Err(TrySendError::Closed(_)) => return Poll::Ready(Err(Closed(()))),
                    Err(_) => {
                        *this.state = State::Start;
                    }
                },
            }
        }
    }
}

#[pin_project::pinned_drop]
impl<T> PinnedDrop for SendRefFuture<'_, T> {
    fn drop(self: Pin<&mut Self>) {
        test_println!("SendRefFuture::drop({:p})", self);
        let this = self.project();
        if test_dbg!(*this.state) == State::Waiting && test_dbg!(this.waiter.is_linked()) {
            this.waiter.remove(&this.core.tx_wait)
        }
    }
}

#[cfg(feature = "alloc")]
impl<T> fmt::Debug for Inner<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Inner")
            .field("core", &self.core)
            .field("slots", &format_args!("Box<[..]>"))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn _assert_sync<T: Sync>(_: T) {}
    fn _assert_send<T: Send>(_: T) {}

    #[test]
    fn recv_ref_future_is_send() {
        fn _compiles() {
            let (_, rx) = channel::<usize>(10);
            _assert_send(rx.recv_ref());
        }
    }

    #[test]
    fn recv_ref_future_is_sync() {
        fn _compiles() {
            let (_, rx) = channel::<usize>(10);
            _assert_sync(rx.recv_ref());
        }
    }

    #[test]
    fn send_ref_future_is_send() {
        fn _compiles() {
            let (tx, _) = channel::<usize>(10);
            _assert_send(tx.send_ref());
        }
    }

    #[test]
    fn send_ref_future_is_sync() {
        fn _compiles() {
            let (tx, _) = channel::<usize>(10);
            _assert_sync(tx.send_ref());
        }
    }
}
