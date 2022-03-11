use super::*;
use crate::{
    loom::atomic::{self, Ordering},
    recycling::{self, Recycle},
    wait::queue,
    Ref,
};
use core::{
    fmt,
    future::Future,
    pin::Pin,
    task::{Context, Poll, Waker},
};

feature! {
    #![feature = "alloc"]

    use crate::loom::sync::Arc;

    /// Returns a new asynchronous multi-producer, single consumer channel.
    pub fn channel<T: Default + Clone>(capacity: usize) -> (Sender<T>, Receiver<T>) {
        with_recycle(capacity, recycling::DefaultRecycle::new())
    }

    /// Returns a new asynchronous multi-producer, single consumer channel with
    /// the provided [recycling policy].
    ///
    /// [recycling policy]: crate::recycling::Recycle
    pub fn with_recycle<T, R: Recycle<T>>(capacity: usize, recycle: R) -> (Sender<T, R>, Receiver<T, R>) {
        assert!(capacity > 0);
        let inner = Arc::new(Inner {
            core: ChannelCore::new(capacity),
            slots: Slot::make_boxed_array(capacity),
            recycle,
        });
        let tx = Sender {
            inner: inner.clone(),
        };
        let rx = Receiver { inner };
        (tx, rx)
    }

    #[derive(Debug)]
    pub struct Receiver<T, R = recycling::DefaultRecycle> {
        inner: Arc<Inner<T, R>>,
    }

    #[derive(Debug)]
    pub struct Sender<T, R = recycling::DefaultRecycle> {
        inner: Arc<Inner<T, R>>,
    }

    struct Inner<T, R> {
        core: super::ChannelCore<Waker>,
        slots: Box<[Slot<T>]>,
        recycle: R,
    }

    // === impl Sender ===

    impl<T, R> Sender<T, R>
    where
        R: Recycle<T>,
    {
        pub fn try_send_ref(&self) -> Result<SendRef<'_, T>, TrySendError> {
            self.inner
                .core
                .try_send_ref(self.inner.slots.as_ref(), &self.inner.recycle)
                .map(SendRef)
        }

        pub fn try_send(&self, val: T) -> Result<(), TrySendError<T>> {
            self.inner
                .core
                .try_send(self.inner.slots.as_ref(), val, &self.inner.recycle)
        }

        pub async fn send_ref(&self) -> Result<SendRef<'_, T>, Closed> {
            SendRefFuture {
                core: &self.inner.core,
                slots: self.inner.slots.as_ref(),
                recycle: &self.inner.recycle,
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

    impl<T, R> Clone for Sender<T, R> {
        fn clone(&self) -> Self {
            test_dbg!(self.inner.core.tx_count.fetch_add(1, Ordering::Relaxed));
            Self {
                inner: self.inner.clone(),
            }
        }
    }

    impl<T, R> Drop for Sender<T, R> {
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

    impl<T, R> Receiver<T, R> {
        pub fn recv_ref(&self) -> RecvRefFuture<'_, T> {
            RecvRefFuture {
                core: &self.inner.core,
                slots: self.inner.slots.as_ref(),
            }
        }

        pub fn recv(&self) -> RecvFuture<'_, T, R>
        where
            R: Recycle<T>,
        {
            RecvFuture {
                core: &self.inner.core,
                slots: self.inner.slots.as_ref(),
                recycle: &self.inner.recycle,
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
        pub fn poll_recv(&self, cx: &mut Context<'_>) -> Poll<Option<T>>
        where
            R: Recycle<T>,
        {
            self.poll_recv_ref(cx)
                .map(|opt| opt.map(|mut r| recycling::take(&mut *r, &self.inner.recycle)))
        }

        pub fn is_closed(&self) -> bool {
            test_dbg!(self.inner.core.tx_count.load(Ordering::SeqCst)) <= 1
        }
    }

    impl<T, R> Drop for Receiver<T, R> {
        fn drop(&mut self) {
            self.inner.core.close_rx();
        }
    }

    impl<T, R: fmt::Debug> fmt::Debug for Inner<T, R> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("Inner")
                .field("core", &self.core)
                .field("slots", &format_args!("Box<[..]>"))
                .field("recycle", &self.recycle)
                .finish()
        }
    }

    impl<T, R> Drop for Inner<T, R> {
        fn drop(&mut self) {
            self.core.core.drop_slots(&mut self.slots[..])
        }
    }
}

#[cfg(not(all(loom, test)))]
feature! {
    #![feature = "static"]
    use crate::loom::atomic::AtomicBool;

    /// A statically-allocated, asynchronous bounded MPSC channel.
    ///
    /// A statically-allocated channel allows using a MPSC channel without
    /// requiring _any_ heap allocations, and can be used in environments that
    /// don't support `liballoc`.
    ///
    /// In order to use a statically-allocated channel, a `StaticChannel` must
    /// be constructed in a `static` initializer. This reserves storage for the
    /// channel's message queue at compile-time. Then, at runtime, the channel
    /// is [`split`] into a [`StaticSender`]/[`StaticReceiver`] pair in order to
    /// be used.
    ///
    /// # Examples
    ///
    /// ```
    /// use thingbuf::mpsc::StaticChannel;
    ///
    /// // Construct a statically-allocated channel of `usize`s with a capacity
    /// // of 16 messages.
    /// static MY_CHANNEL: StaticChannel<usize, 16> = StaticChannel::new();
    ///
    /// fn main() {
    ///     // Split the `StaticChannel` into a sender-receiver pair.
    ///     let (tx, rx) = MY_CHANNEL.split();
    ///
    ///     // Now, `tx` and `rx` can be used just like any other async MPSC
    ///     // channel...
    /// # drop(tx); drop(rx);
    /// }
    /// ```
    /// [`split`]: StaticChannel::split
    pub struct StaticChannel<T, const CAPACITY: usize, R = recycling::DefaultRecycle> {
        core: ChannelCore<Waker>,
        recycle: R,
        slots: [Slot<T>; CAPACITY],
        is_split: AtomicBool,
    }

    pub struct StaticSender<T: 'static, R: 'static = recycling::DefaultRecycle> {
        core: &'static ChannelCore<Waker>,
        recycle: &'static R,
        slots: &'static [Slot<T>],
    }

    pub struct StaticReceiver<T: 'static, R: 'static = recycling::DefaultRecycle> {
        core: &'static ChannelCore<Waker>,
        recycle: &'static R,
        slots: &'static [Slot<T>],
    }

    // === impl StaticChannel ===

    impl<T, const CAPACITY: usize> StaticChannel<T, CAPACITY> {
        /// Constructs a new statically-allocated, asynchronous bounded MPSC channel.
        ///
        /// A statically-allocated channel allows using a MPSC channel without
        /// requiring _any_ heap allocations, and can be used in environments that
        /// don't support `liballoc`.
        ///
        /// In order to use a statically-allocated channel, a `StaticChannel` must
        /// be constructed in a `static` initializer. This reserves storage for the
        /// channel's message queue at compile-time. Then, at runtime, the channel
        /// is [`split`] into a [`StaticSender`]/[`StaticReceiver`] pair in order to
        /// be used.
        ///
        /// # Examples
        ///
        /// ```
        /// use thingbuf::mpsc::StaticChannel;
        ///
        /// // Construct a statically-allocated channel of `usize`s with a capacity
        /// // of 16 messages.
        /// static MY_CHANNEL: StaticChannel<usize, 16> = StaticChannel::new();
        ///
        /// fn main() {
        ///     // Split the `StaticChannel` into a sender-receiver pair.
        ///     let (tx, rx) = MY_CHANNEL.split();
        ///
        ///     // Now, `tx` and `rx` can be used just like any other async MPSC
        ///     // channel...
        /// # drop(tx); drop(rx);
        /// }
        /// ```
        /// [`split`]: StaticChannel::split
        pub const fn new() -> Self {
            Self {
                core: ChannelCore::new(CAPACITY),
                slots: Slot::make_static_array::<CAPACITY>(),
                is_split: AtomicBool::new(false),
                recycle: recycling::DefaultRecycle::new(),
            }
        }
    }

    impl<T, R, const CAPACITY: usize> StaticChannel<T, CAPACITY, R> {
        /// Split a [`StaticChannel`] into a [`StaticSender`]/[`StaticReceiver`]
        /// pair.
        ///
        /// A static channel can only be split a single time. If
        /// [`StaticChannel::split`] or [`StaticChannel::try_split`] have been
        /// called previously, this method will panic. For a non-panicking version
        /// of this method, see [`StaticChannel::try_split`].
        ///
        /// # Panics
        ///
        /// If the channel has already been split.
        pub fn split(&'static self) -> (StaticSender<T, R>, StaticReceiver<T, R>) {
            self.try_split().expect("channel already split")
        }

        /// Try to split a [`StaticChannel`] into a [`StaticSender`]/[`StaticReceiver`]
        /// pair, returning `None` if it has already been split.
        ///
        /// A static channel can only be split a single time. If
        /// [`StaticChannel::split`] or [`StaticChannel::try_split`] have been
        /// called previously, this method returns `None`.
        pub fn try_split(&'static self) -> Option<(StaticSender<T, R>, StaticReceiver<T, R>)> {
            self.is_split
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .ok()?;
            let tx = StaticSender {
                core: &self.core,
                recycle: &self.recycle,
                slots: &self.slots[..],
            };
            let rx = StaticReceiver {
                core: &self.core,
                recycle: &self.recycle,
                slots: &self.slots[..],
            };
            Some((tx, rx))
        }
    }

    // === impl StaticSender ===

    impl<T, R> StaticSender<T, R>
    where
        R: Recycle<T>,
    {
        pub fn try_send_ref(&self) -> Result<SendRef<'_, T>, TrySendError> {
            self.core
                .try_send_ref(self.slots, self.recycle)
                .map(SendRef)
        }

        pub fn try_send(&self, val: T) -> Result<(), TrySendError<T>> {
            self.core.try_send(self.slots, val, self.recycle)
        }

        pub async fn send_ref(&self) -> Result<SendRef<'_, T>, Closed> {
            SendRefFuture {
                core: self.core,
                slots: self.slots,
                recycle: self.recycle,
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
                recycle: self.recycle,
            }
        }
    }

    impl<T, R> Drop for StaticSender<T, R> {
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

    impl<T, R: fmt::Debug> fmt::Debug for StaticSender<T, R> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("StaticSender")
                .field("core", &self.core)
                .field("slots", &format_args!("&[..]"))
                .field("recycle", self.recycle)
                .finish()
        }
    }

    // === impl StaticReceiver ===

    impl<T, R> StaticReceiver<T, R> {
        pub fn recv_ref(&self) -> RecvRefFuture<'_, T> {
            RecvRefFuture {
                core: self.core,
                slots: self.slots,
            }
        }

        pub fn recv(&self) -> RecvFuture<'_, T, R>
        where
            R: Recycle<T>,
        {
            RecvFuture {
                core: self.core,
                slots: self.slots,
                recycle: self.recycle,
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
        pub fn poll_recv(&self, cx: &mut Context<'_>) -> Poll<Option<T>>
        where
            R: Recycle<T>,
        {
            self.poll_recv_ref(cx)
                .map(|opt| opt.map(|mut r| recycling::take(&mut *r, self.recycle)))
        }

        pub fn is_closed(&self) -> bool {
            test_dbg!(self.core.tx_count.load(Ordering::SeqCst)) <= 1
        }
    }

    impl<T, R> Drop for StaticReceiver<T, R> {
        fn drop(&mut self) {
            self.core.close_rx();
        }
    }

    impl<T, R: fmt::Debug> fmt::Debug for StaticReceiver<T, R> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("StaticReceiver")
                .field("core", &self.core)
                .field("slots", &format_args!("&[..]"))
                .field("recycle", &self.recycle)
                .finish()
        }
    }
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
///
/// [`ThingBuf`]: crate::ThingBuf
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct RecvFuture<'a, T, R = recycling::DefaultRecycle> {
    core: &'a ChannelCore<Waker>,
    slots: &'a [Slot<T>],
    recycle: &'a R,
}

#[pin_project::pin_project(PinnedDrop)]
struct SendRefFuture<'sender, T, R> {
    core: &'sender ChannelCore<Waker>,
    slots: &'sender [Slot<T>],
    recycle: &'sender R,
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

// === impl RecvRefFuture ===

#[inline]
fn poll_recv_ref<'a, T>(
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

impl<'a, T> Future for RecvRefFuture<'a, T> {
    type Output = Option<RecvRef<'a, T>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        poll_recv_ref(self.core, self.slots, cx)
    }
}

// === impl Recv ===

impl<'a, T, R> Future for RecvFuture<'a, T, R>
where
    R: Recycle<T>,
{
    type Output = Option<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        poll_recv_ref(self.core, self.slots, cx)
            .map(|opt| opt.map(|mut r| recycling::take(&mut *r, self.recycle)))
    }
}

// === impl SendRefFuture ===

impl<'sender, T, R> Future for SendRefFuture<'sender, T, R>
where
    R: Recycle<T> + 'sender,
    T: 'sender,
{
    type Output = Result<SendRef<'sender, T>, Closed>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        test_println!("SendRefFuture::poll({:p})", self);

        loop {
            let this = self.as_mut().project();
            let node = this.waiter;
            match test_dbg!(*this.state) {
                State::Start => {
                    match this.core.try_send_ref(this.slots, *this.recycle) {
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
                State::Done => match this.core.try_send_ref(this.slots, *this.recycle) {
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
impl<T, R> PinnedDrop for SendRefFuture<'_, T, R> {
    fn drop(self: Pin<&mut Self>) {
        test_println!("SendRefFuture::drop({:p})", self);
        let this = self.project();
        if test_dbg!(*this.state) == State::Waiting && test_dbg!(this.waiter.is_linked()) {
            this.waiter.remove(&this.core.tx_wait)
        }
    }
}

#[cfg(feature = "alloc")]
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
