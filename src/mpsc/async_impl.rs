use super::*;
use crate::{
    loom::atomic::{self, Ordering},
    recycling::{self, Recycle},
    wait::queue,
};
use core::{
    fmt,
    future::Future,
    pin::Pin,
    task::{Context, Poll, Waker},
};
use errors::*;

feature! {
    #![feature = "alloc"]

    use crate::{MAX_CAPACITY, loom::sync::Arc};
    use alloc::boxed::Box;

    /// Returns a new asynchronous multi-producer, single consumer (MPSC)
    /// channel with the provided capacity.
    ///
    /// This channel will use the [default recycling policy].
    ///
    /// [recycling policy]: crate::recycling::DefaultRecycle
    #[must_use]
    pub fn channel<T: Default + Clone>(capacity: usize) -> (Sender<T>, Receiver<T>) {
        with_recycle(capacity, recycling::DefaultRecycle::new())
    }

    /// Returns a new asynchronous multi-producer, single consumer (MPSC)
    /// channel with the provided capacity and [recycling policy].
    ///
    /// # Panics
    ///
    /// Panics if the capacity exceeds `usize::MAX & !(1 << (usize::BITS - 1))`. This value
    /// represents the highest power of two that can be expressed by a `usize`, excluding the most
    /// significant bit.
    ///
    /// [recycling policy]: crate::recycling::Recycle
    #[must_use]
    pub fn with_recycle<T, R: Recycle<T>>(capacity: usize, recycle: R) -> (Sender<T, R>, Receiver<T, R>) {
        assert!(capacity > 0);
        assert!(capacity <= MAX_CAPACITY);
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


    /// Asynchronously receives values from associated [`Sender`]s.
    ///
    /// Instances of this struct are created by the [`channel`] and
    /// [`with_recycle`] functions.
    #[derive(Debug)]
    pub struct Receiver<T, R = recycling::DefaultRecycle> {
        inner: Arc<Inner<T, R>>,
    }

    /// Asynchronously sends values to an associated [`Receiver`].
    ///
    /// Instances of this struct are created by the [`channel`] and
    /// [`with_recycle`] functions.
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
        /// Reserves a slot in the channel to mutate in place, waiting until
        /// there is a free slot to write to.
        ///
        /// This is similar to the [`send`] method, but, rather than taking a
        /// message by value to write to the channel, this method reserves a
        /// writable slot in the channel, and returns a [`SendRef`] that allows
        /// mutating the slot in place. If the [`Receiver`] end of the channel
        /// uses the [`Receiver::recv_ref`] or [`Receiver::poll_recv_ref`]
        /// methods for receiving from the channel, this allows allocations for
        /// channel messages to be reused in place.
        ///
        /// # Errors
        ///
        /// If the [`Receiver`] end of the channel has been dropped, this
        /// returns a [`Closed`] error.
        ///
        /// # Examples
        ///
        /// Sending formatted strings by writing them directly to channel slots,
        /// in place:
        /// ```
        /// use thingbuf::mpsc;
        /// use std::fmt::Write;
        ///
        /// #[tokio::main]
        /// async fn main() {
        ///     let (tx, rx) = mpsc::channel::<String>(8);
        ///
        ///     // Spawn a task that prints each message received from the channel:
        ///     tokio::spawn(async move {
        ///         for _ in 0..10 {
        ///             let msg = rx.recv_ref().await.unwrap();
        ///             println!("{}", msg);
        ///         }
        ///     });
        ///
        ///     // Until the channel closes, write formatted messages to the channel.
        ///     let mut count = 1;
        ///     while let Ok(mut value) = tx.send_ref().await {
        ///         // Writing to the `SendRef` will reuse the *existing* string
        ///         // allocation in place.
        ///         write!(value, "hello from message {}", count)
        ///             .expect("writing to a `String` should never fail");
        ///         count += 1;
        ///     }
        /// }
        /// ```
        /// [`send`]: Self::send
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

        /// Sends a message by value, waiting until there is a free slot to
        /// write to.
        ///
        /// This method takes the message by value, and replaces any previous
        /// value in the slot. This means that the channel will *not* function
        /// as an object pool while sending messages with `send`. This method is
        /// most appropriate when messages don't own reusable heap allocations,
        /// or when the [`Receiver`] end of the channel must receive messages by
        /// moving them out of the channel by value (using the
        /// [`Receiver::recv`] method). When messages in the channel own
        /// reusable heap allocations (such as `String`s or `Vec`s), and the
        /// [`Receiver`] doesn't need to receive them by value, consider using
        /// [`send_ref`] instead, to enable allocation reuse.
        ///
        /// # Errors
        ///
        /// If the [`Receiver`] end of the channel has been dropped, this
        /// returns a [`Closed`] error containing the sent value.
        ///
        /// # Examples
        ///
        /// ```
        /// use thingbuf::mpsc;
        ///
        /// #[tokio::main]
        /// async fn main() {
        ///     let (tx, rx) = mpsc::channel(8);
        ///
        ///     // Spawn a task that prints each message received from the channel:
        ///     tokio::spawn(async move {
        ///         for _ in 0..10 {
        ///             let msg = rx.recv().await.unwrap();
        ///             println!("received message {}", msg);
        ///         }
        ///     });
        ///
        ///     // Until the channel closes, write the current iteration to the channel.
        ///     let mut count = 1;
        ///     while tx.send(count).await.is_ok() {
        ///         count += 1;
        ///     }
        /// }
        /// ```
        /// [`send_ref`]: Self::send_ref
        pub async fn send(&self, val: T) -> Result<(), Closed<T>> {
            match self.send_ref().await {
                Err(Closed(())) => Err(Closed(val)),
                Ok(mut slot) => {
                    *slot = val;
                    Ok(())
                }
            }
        }

        /// Attempts to reserve a slot in the channel to mutate in place,
        /// without waiting for capacity.
        ///
        /// This method differs from [`send_ref`] by returning immediately if the
        /// channel’s buffer is full or no [`Receiver`] exists. Compared with
        /// [`send_ref`], this method has two failure cases instead of one (one for
        /// disconnection, one for a full buffer), and this method is not
        /// `async`, because it will never wait.
        ///
        /// Like [`send_ref`], this method returns a [`SendRef`] that may be
        /// used to mutate a slot in the channel's buffer in place. Dropping the
        /// [`SendRef`] completes the send operation and makes the mutated value
        /// available to the [`Receiver`].
        ///
        /// # Errors
        ///
        /// If the channel capacity has been reached (i.e., the channel has `n`
        /// buffered values where `n` is the argument passed to
        /// [`channel`]/[`with_recycle`]), then [`TrySendError::Full`] is
        /// returned. In this case, a future call to `try_send` may succeed if
        /// additional capacity becomes available.
        ///
        /// If the receive half of the channel is closed (i.e., the [`Receiver`]
        /// handle was dropped), the function returns [`TrySendError::Closed`].
        /// Once the channel has closed, subsequent calls to `try_send_ref` will
        /// never succeed.
        ///
        /// [`send_ref`]: Self::send_ref
        pub fn try_send_ref(&self) -> Result<SendRef<'_, T>, TrySendError> {
            self.inner
                .core
                .try_send_ref(self.inner.slots.as_ref(), &self.inner.recycle)
                .map(SendRef)
        }

        /// Attempts to send a message by value immediately, without waiting for
        /// capacity.
        ///
        /// This method differs from [`send`] by returning immediately if the
        /// channel’s buffer is full or no [`Receiver`] exists. Compared with
        /// [`send`], this method has two failure cases instead of one (one for
        /// disconnection, one for a full buffer), and this method is not
        /// `async`, because it will never wait.
        ///
        /// # Errors
        ///
        /// If the channel capacity has been reached (i.e., the channel has `n`
        /// buffered values where `n` is the argument passed to
        /// [`channel`]/[`with_recycle`]), then [`TrySendError::Full`] is
        /// returned. In this case, a future call to `try_send` may succeed if
        /// additional capacity becomes available.
        ///
        /// If the receive half of the channel is closed (i.e., the [`Receiver`]
        /// handle was dropped), the function returns [`TrySendError::Closed`].
        /// Once the channel has closed, subsequent calls to `try_send` will
        /// never succeed.
        ///
        /// In both cases, the error includes the value passed to `try_send`.
        ///
        /// [`send`]: Self::send
        pub fn try_send(&self, val: T) -> Result<(), TrySendError<T>> {
            self.inner
                .core
                .try_send(self.inner.slots.as_ref(), val, &self.inner.recycle)
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
        /// Receives the next message for this receiver, **by reference**.
        ///
        /// This method returns `None` if the channel has been closed and there are
        /// no remaining messages in the channel's buffer. This indicates that no
        /// further values can ever be received from this `Receiver`. The channel is
        /// closed when all [`Sender`]s have been dropped.
        ///
        /// If there are no messages in the channel's buffer, but the channel has
        /// not yet been closed, this method will wait until a message is sent or
        /// the channel is closed.
        ///
        /// This method returns a [`RecvRef`] that can be used to read from (or
        /// mutate) the received message by reference. When the [`RecvRef`] is
        /// dropped, the receive operation completes and the slot occupied by
        /// the received message becomes usable for a future [`send_ref`] operation.
        ///
        /// If all [`Sender`]s for this channel write to the channel's slots in
        /// place by using the [`send_ref`] or [`try_send_ref`] methods, this
        /// method allows messages that own heap allocations to be reused in
        /// place.
        ///
        /// # Examples
        ///
        /// ```
        /// use thingbuf::mpsc;
        /// use std::fmt::Write;
        ///
        /// #[tokio::main]
        /// async fn main() {
        ///     let (tx, rx) = mpsc::channel::<String>(100);
        ///     tokio::spawn(async move {
        ///         let mut value = tx.send_ref().await.unwrap();
        ///         write!(value, "hello world!")
        ///             .expect("writing to a `String` should never fail");
        ///     });
        ///
        ///     assert_eq!(Some("hello world!"), rx.recv_ref().await.as_deref().map(String::as_str));
        ///     assert_eq!(None, rx.recv().await.as_deref());
        /// }
        /// ```
        ///
        /// Values are buffered:
        ///
        /// ```
        /// use thingbuf::mpsc;
        /// use std::fmt::Write;
        ///
        /// #[tokio::main]
        /// async fn main() {
        ///     let (tx, rx) = mpsc::channel::<String>(100);
        ///
        ///     write!(tx.send_ref().await.unwrap(), "hello").unwrap();
        ///     write!(tx.send_ref().await.unwrap(), "world").unwrap();
        ///
        ///     assert_eq!("hello", rx.recv_ref().await.unwrap().as_str());
        ///     assert_eq!("world", rx.recv_ref().await.unwrap().as_str());
        /// }
        /// ```
        ///
        /// [`send_ref`]: Sender::send_ref
        /// [`try_send_ref`]: Sender::try_send_ref
        pub fn recv_ref(&self) -> RecvRefFuture<'_, T> {
            RecvRefFuture {
                core: &self.inner.core,
                slots: self.inner.slots.as_ref(),
            }
        }

        /// Receives the next message for this receiver, **by value**.
        ///
        /// This method returns `None` if the channel has been closed and there are
        /// no remaining messages in the channel's buffer. This indicates that no
        /// further values can ever be received from this `Receiver`. The channel is
        /// closed when all [`Sender`]s have been dropped.
        ///
        /// If there are no messages in the channel's buffer, but the channel has
        /// not yet been closed, this method will wait until a message is sent or
        /// the channel is closed.
        ///
        /// When a message is received, it is moved out of the channel by value,
        /// and replaced with a new slot according to the configured [recycling
        /// policy]. If all [`Sender`]s for this channel write to the channel's
        /// slots in place by using the [`send_ref`] or [`try_send_ref`] methods,
        /// consider using the [`recv_ref`] method instead, to enable the
        /// reuse of heap allocations.
        ///
        /// # Examples
        ///
        /// ```
        /// use thingbuf::mpsc;
        ///
        /// #[tokio::main]
        /// async fn main() {
        ///
        ///     let (tx, rx) = mpsc::channel(100);
        ///
        ///     tokio::spawn(async move {
        ///         tx.send(1).await.unwrap();
        ///     });
        ///
        ///     assert_eq!(Some(1), rx.recv().await);
        ///     assert_eq!(None, rx.recv().await);
        /// }
        /// ```
        ///
        /// Values are buffered:
        ///
        /// ```
        /// use thingbuf::mpsc;
        ///
        /// #[tokio::main]
        /// async fn main() {
        ///     let (tx, rx) = mpsc::channel(100);
        ///
        ///     tx.send(1).await.unwrap();
        ///     tx.send(2).await.unwrap();
        ///
        ///     assert_eq!(Some(1), rx.recv().await);
        ///     assert_eq!(Some(2), rx.recv().await);
        /// }
        /// ```
        ///
        /// [`send_ref`]: Sender::send_ref
        /// [`try_send_ref`]: Sender::try_send_ref
        /// [recycling policy]: crate::recycling::Recycle
        /// [`recv_ref`]: Self::recv_ref
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

        /// Attempts to receive the next message for this receiver by reference
        /// without waiting for a new message when the channel is empty.
        ///
        /// This method differs from [`recv_ref`] by returning immediately if the
        /// channel is empty or closed.
        ///
        /// # Errors
        ///
        /// This method returns an error when the channel is closed or there are
        /// no remaining messages in the channel's buffer.
        ///
        /// # Examples
        ///
        /// ```
        /// use thingbuf::mpsc::{channel, errors::TryRecvError};
        ///
        /// let (tx, rx) = channel(100);
        /// assert!(matches!(rx.try_recv_ref(), Err(TryRecvError::Empty)));
        ///
        /// tx.try_send(1).unwrap();
        /// drop(tx);
        ///
        /// assert_eq!(*rx.try_recv_ref().unwrap(), 1);
        /// assert!(matches!(rx.try_recv_ref(), Err(TryRecvError::Closed)));
        /// ```
        ///
        /// [`recv_ref`]: Self::recv_ref
        pub fn try_recv_ref(&self) -> Result<RecvRef<'_, T>, TryRecvError>
        where
            R: Recycle<T>,
        {
            self.inner.core.try_recv_ref(self.inner.slots.as_ref()).map(RecvRef)
        }

        /// Attempts to receive the next message for this receiver by reference
        /// without waiting for a new message when the channel is empty.
        ///
        /// This method differs from [`recv`] by returning immediately if the
        /// channel is empty or closed.
        ///
        /// # Errors
        ///
        /// This method returns an error when the channel is closed or there are
        /// no remaining messages in the channel's buffer.
        ///
        /// # Examples
        ///
        /// ```
        /// use thingbuf::mpsc::{channel, errors::TryRecvError};
        ///
        /// let (tx, rx) = channel(100);
        /// assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
        ///
        /// tx.try_send(1).unwrap();
        /// drop(tx);
        ///
        /// assert_eq!(rx.try_recv().unwrap(), 1);
        /// assert_eq!(rx.try_recv(), Err(TryRecvError::Closed));
        /// ```
        ///
        /// [`recv`]: Self::recv
        pub fn try_recv(&self) -> Result<T, TryRecvError>
        where
            R: Recycle<T>,
        {
            self.inner.core.try_recv(self.inner.slots.as_ref(), &self.inner.recycle)
        }

        /// Attempts to receive a message *by reference* from this channel,
        /// registering the current task for wakeup if the a message is not yet
        /// available, and returning `None` if the channel has closed and all
        /// messages have been received.
        ///
        /// Like [`Receiver::recv_ref`], this method returns a [`RecvRef`] that
        /// can be used to read from (or mutate) the received message by
        /// reference. When the [`RecvRef`] is dropped, the receive operation
        /// completes and the slot occupied by the received message becomes
        /// usable for a future [`send_ref`] operation.
        ///
        /// If all [`Sender`]s for this channel write to the channel's slots in
        /// place by using the [`send_ref`] or [`try_send_ref`] methods, this
        /// method allows messages that own heap allocations to be reused in
        /// place.
        ///
        /// To wait asynchronously until a message becomes available, use the
        /// [`recv_ref`] method instead.
        ///
        /// # Returns
        ///
        ///  * `Poll::Pending` if no messages are available but the channel is not
        ///    closed, or if a spurious failure happens.
        ///  * `Poll::Ready(Some(RecvRef<T>))` if a message is available.
        ///  * `Poll::Ready(None)` if the channel has been closed (i.e., all
        ///    [`Sender`]s have been dropped), and all messages sent before it
        ///    was closed have been received.
        ///
        /// When the method returns [`Poll::Pending`], the [`Waker`] in the provided
        /// [`Context`] is scheduled to receive a wakeup when a message is sent on any
        /// sender, or when the channel is closed.  Note that on multiple calls to
        /// `poll_recv_ref`, only the [`Waker`] from the [`Context`] passed to the most
        /// recent call is scheduled to receive a wakeup.
        ///
        /// [`send_ref`]: Sender::send_ref
        /// [`try_send_ref`]: Sender::try_send_ref
        /// [`recv_ref`]: Self::recv_ref
        pub fn poll_recv_ref(&self, cx: &mut Context<'_>) -> Poll<Option<RecvRef<'_, T>>> {
            poll_recv_ref(&self.inner.core, &self.inner.slots, cx)
        }

        /// Attempts to receive a message *by value* from this channel,
        /// registering the current task for wakeup if the value is not yet
        /// available, and returning `None` if the channel has closed and all
        /// messages have been received.
        ///
        /// When a message is received, it is moved out of the channel by value,
        /// and replaced with a new slot according to the configured [recycling
        /// policy]. If all [`Sender`]s for this channel write to the channel's
        /// slots in place by using the [`send_ref`] or [`try_send_ref`] methods,
        /// consider using the [`poll_recv_ref`] method instead, to enable the
        /// reuse of heap allocations.
        ///
        /// To wait asynchronously until a message becomes available, use the
        /// [`recv`] method instead.
        ///
        /// # Returns
        ///
        ///  * `Poll::Pending` if no messages are available but the channel is not
        ///    closed, or if a spurious failure happens.
        ///  * `Poll::Ready(Some(message))` if a message is available.
        ///  * `Poll::Ready(None)` if the channel has been closed (i.e., all
        ///    [`Sender`]s have been dropped) and all messages sent before it
        ///    was closed have been received.
        ///
        /// When the method returns [`Poll::Pending`], the [`Waker`] in the provided
        /// [`Context`] is scheduled to receive a wakeup when a message is sent on any
        /// sender, or when the channel is closed.  Note that on multiple calls to
        /// `poll_recv`, only the [`Waker`] from the [`Context`] passed to the most
        /// recent call is scheduled to receive a wakeup.
        ///
        /// [`send_ref`]: Sender::send_ref
        /// [`try_send_ref`]: Sender::try_send_ref
        /// [recycling policy]: crate::recycling::Recycle
        /// [`poll_recv_ref`]: Self::poll_recv_ref
        /// [`recv`]: Self::recv
        pub fn poll_recv(&self, cx: &mut Context<'_>) -> Poll<Option<T>>
        where
            R: Recycle<T>,
        {
            self.poll_recv_ref(cx)
                .map(|opt| opt.map(|mut r| recycling::take(&mut *r, &self.inner.recycle)))
        }

        /// Returns `true` if the channel has closed (all corresponding
        /// [`Sender`]s have been dropped).
        ///
        /// If this method returns `true`, no new messages will become available
        /// on this channel. Previously sent messages may still be available.
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

    /// Asynchronously sends values to an associated [`StaticReceiver`].
    ///
    /// Instances of this struct are created by the [`StaticChannel::split`] and
    /// [``StaticChannel::try_split`] functions.
    pub struct StaticSender<T: 'static, R: 'static = recycling::DefaultRecycle> {
        core: &'static ChannelCore<Waker>,
        recycle: &'static R,
        slots: &'static [Slot<T>],
    }

    /// Asynchronously receives values from associated [`StaticSender`]s.
    ///
    /// Instances of this struct are created by the [`StaticChannel::split`] and
    /// [``StaticChannel::try_split`] functions.
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
        #[must_use]
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
        #[must_use]
        pub fn split(&'static self) -> (StaticSender<T, R>, StaticReceiver<T, R>) {
            self.try_split().expect("channel already split")
        }

        /// Try to split a [`StaticChannel`] into a [`StaticSender`]/[`StaticReceiver`]
        /// pair, returning `None` if it has already been split.
        ///
        /// A static channel can only be split a single time. If
        /// [`StaticChannel::split`] or [`StaticChannel::try_split`] have been
        /// called previously, this method returns `None`.
        #[must_use]
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
        /// Reserves a slot in the channel to mutate in place, waiting until
        /// there is a free slot to write to.
        ///
        /// This is similar to the [`send`] method, but, rather than taking a
        /// message by value to write to the channel, this method reserves a
        /// writable slot in the channel, and returns a [`SendRef`] that allows
        /// mutating the slot in place. If the [`StaticReceiver`] end of the
        /// channel uses the [`StaticReceiver::recv_ref`] or
        /// [`StaticReceiver::poll_recv_ref`] methods for receiving from the
        /// channel, this allows allocations for channel messages to be reused in place.
        ///
        /// # Errors
        ///
        /// If the [`StaticReceiver`] end of the channel has been dropped, this
        /// returns a [`Closed`] error.
        ///
        /// # Examples
        ///
        /// Sending formatted strings by writing them directly to channel slots,
        /// in place:
        ///
        /// ```
        /// use thingbuf::mpsc;
        /// use std::fmt::Write;
        ///
        /// static CHANNEL: mpsc::StaticChannel<String, 8> = mpsc::StaticChannel::new();
        ///
        /// #[tokio::main]
        /// async fn main() {
        ///     let (tx, rx) = CHANNEL.split();
        ///
        ///     // Spawn a task that prints each message received from the channel:
        ///     tokio::spawn(async move {
        ///         for _ in 0..10 {
        ///             let msg = rx.recv_ref().await.unwrap();
        ///             println!("{}", msg);
        ///         }
        ///     });
        ///
        ///     // Until the channel closes, write formatted messages to the channel.
        ///     let mut count = 1;
        ///     while let Ok(mut value) = tx.send_ref().await {
        ///         // Writing to the `SendRef` will reuse the *existing* string
        ///         // allocation in place.
        ///         write!(value, "hello from message {}", count)
        ///             .expect("writing to a `String` should never fail");
        ///         count += 1;
        ///     }
        /// }
        /// ```
        /// [`send`]: Self::send
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

        /// Sends a message by value, waiting until there is a free slot to
        /// write to.
        ///
        /// This method takes the message by value, and replaces any previous
        /// value in the slot. This means that the channel will *not* function
        /// as an object pool while sending messages with `send`. This method is
        /// most appropriate when messages don't own reusable heap allocations,
        /// or when the [`StaticReceiver`] end of the channel must receive
        /// messages by moving them out of the channel by value (using the
        /// [`StaticReceiver::recv`] method). When messages in the channel own
        /// reusable heap allocations (such as `String`s or `Vec`s), and the
        /// [`StaticReceiver`] doesn't need to receive them by value, consider
        /// using [`send_ref`] instead, to enable allocation reuse.
        ///
        /// # Errors
        ///
        /// If the [`StaticReceiver`] end of the channel has been dropped, this
        /// returns a [`Closed`] error containing the sent value.
        ///
        /// # Examples
        ///
        /// ```
        /// use thingbuf::mpsc;
        ///
        /// static CHANNEL: mpsc::StaticChannel<i32, 8> = mpsc::StaticChannel::new();
        ///
        /// #[tokio::main]
        /// async fn main() {
        ///     let (tx, rx) = CHANNEL.split();
        ///
        ///     // Spawn a task that prints each message received from the channel:
        ///     tokio::spawn(async move {
        ///         for _ in 0..10 {
        ///             let msg = rx.recv().await.unwrap();
        ///             println!("received message {}", msg);
        ///         }
        ///     });
        ///
        ///     // Until the channel closes, write the current iteration to the channel.
        ///     let mut count = 1;
        ///     while tx.send(count).await.is_ok() {
        ///         count += 1;
        ///     }
        /// }
        /// ```
        /// [`send_ref`]: Self::send_ref
        pub async fn send(&self, val: T) -> Result<(), Closed<T>> {
            match self.send_ref().await {
                Err(Closed(())) => Err(Closed(val)),
                Ok(mut slot) => {
                    *slot = val;
                    Ok(())
                }
            }
        }

        /// Attempts to reserve a slot in the channel to mutate in place,
        /// without waiting for capacity.
        ///
        /// This method differs from [`send_ref`] by returning immediately if the
        /// channel’s buffer is full or no [`Receiver`] exists. Compared with
        /// [`send_ref`], this method has two failure cases instead of one (one for
        /// disconnection, one for a full buffer), and this method is not
        /// `async`, because it will never wait.
        ///
        /// Like [`send_ref`], this method returns a [`SendRef`] that may be
        /// used to mutate a slot in the channel's buffer in place. Dropping the
        /// [`SendRef`] completes the send operation and makes the mutated value
        /// available to the [`Receiver`].
        ///
        /// # Errors
        ///
        /// If the channel capacity has been reached (i.e., the channel has `n`
        /// buffered values where `n` is the `usize` const generic parameter of
        /// the [`StaticChannel`]), then [`TrySendError::Full`] is returned. In
        /// this case, a future call to `try_send` may succeed if additional
        /// capacity becomes available.
        ///
        /// If the receive half of the channel is closed (i.e., the
        /// [`StaticReceiver`] handle was dropped), the function returns
        /// [`TrySendError::Closed`]. Once the channel has closed, subsequent
        /// calls to `try_send_ref` will never succeed.
        ///
        /// [`send_ref`]: Self::send_ref
        pub fn try_send_ref(&self) -> Result<SendRef<'_, T>, TrySendError> {
            self.core
                .try_send_ref(self.slots, self.recycle)
                .map(SendRef)
        }

        /// Attempts to send a message by value immediately, without waiting for
        /// capacity.
        ///
        /// This method differs from [`send`] by returning immediately if the
        /// channel’s buffer is full or no [`StaticReceiver`] exists. Compared with
        /// [`send`], this method has two failure cases instead of one (one for
        /// disconnection, one for a full buffer), and this method is not
        /// `async`, because it will never wait.
        ///
        /// # Errors
        ///
        /// If the channel capacity has been reached (i.e., the channel has `n`
        /// buffered values where `n` is the `usize` const generic parameter of
        /// the [`StaticChannel`]), then [`TrySendError::Full`] is returned. In
        /// this case, a future call to `try_send` may succeed if additional
        /// capacity becomes available.
        ///
        /// If the receive half of the channel is closed (i.e., the
        /// [`StaticReceiver`] handle was dropped), the function returns
        /// [`TrySendError::Closed`]. Once the channel has closed, subsequent
        /// calls to `try_send` will never succeed.
        ///
        /// In both cases, the error includes the value passed to `try_send`.
        ///
        /// [`send`]: Self::send
        pub fn try_send(&self, val: T) -> Result<(), TrySendError<T>> {
            self.core.try_send(self.slots, val, self.recycle)
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
        /// Receives the next message for this receiver, **by reference**.
        ///
        /// This method returns `None` if the channel has been closed and there are
        /// no remaining messages in the channel's buffer. This indicates that no
        /// further values can ever be received from this `StaticReceiver`. The
        /// channel is closed when all [`StaticSender`]s have been dropped.
        ///
        /// If there are no messages in the channel's buffer, but the channel has
        /// not yet been closed, this method will wait until a message is sent or
        /// the channel is closed.
        ///
        /// This method returns a [`RecvRef`] that can be used to read from (or
        /// mutate) the received message by reference. When the [`RecvRef`] is
        /// dropped, the receive operation completes and the slot occupied by
        /// the received message becomes usable for a future [`send_ref`] operation.
        ///
        /// If all [`StaticSender`]s for this channel write to the channel's slots in
        /// place by using the [`send_ref`] or [`try_send_ref`] methods, this
        /// method allows messages that own heap allocations to be reused in
        /// place.
        ///
        /// # Examples
        ///
        /// ```
        /// use thingbuf::mpsc::StaticChannel;
        /// use std::fmt::Write;
        ///
        /// static CHANNEL: StaticChannel<String, 100> = StaticChannel::new();
        ///
        /// #[tokio::main]
        /// async fn main() {
        ///     let (tx, rx) = CHANNEL.split();
        ///
        ///     tokio::spawn(async move {
        ///         let mut value = tx.send_ref().await.unwrap();
        ///         write!(value, "hello world!")
        ///             .expect("writing to a `String` should never fail");
        ///     });
        ///
        ///     assert_eq!(Some("hello world!"), rx.recv_ref().await.as_deref().map(String::as_str));
        ///     assert_eq!(None, rx.recv().await.as_deref());
        /// }
        /// ```
        ///
        /// Values are buffered:
        ///
        /// ```
        /// use thingbuf::mpsc::StaticChannel;
        /// use std::fmt::Write;
        ///
        /// static CHANNEL: StaticChannel<String, 100> = StaticChannel::new();
        ///
        /// #[tokio::main]
        /// async fn main() {
        ///     let (tx, rx) = CHANNEL.split();
        ///
        ///     write!(tx.send_ref().await.unwrap(), "hello").unwrap();
        ///     write!(tx.send_ref().await.unwrap(), "world").unwrap();
        ///
        ///     assert_eq!("hello", rx.recv_ref().await.unwrap().as_str());
        ///     assert_eq!("world", rx.recv_ref().await.unwrap().as_str());
        /// }
        /// ```
        ///
        /// [`send_ref`]: StaticSender::send_ref
        /// [`try_send_ref`]: StaticSender::try_send_ref
        pub fn recv_ref(&self) -> RecvRefFuture<'_, T> {
            RecvRefFuture {
                core: self.core,
                slots: self.slots,
            }
        }

        /// Receives the next message for this receiver, **by value**.
        ///
        /// This method returns `None` if the channel has been closed and there are
        /// no remaining messages in the channel's buffer. This indicates that no
        /// further values can ever be received from this `StaticReceiver`. The
        /// channel is closed when all [`StaticSender`]s have been dropped.
        ///
        /// If there are no messages in the channel's buffer, but the channel has
        /// not yet been closed, this method will wait until a message is sent or
        /// the channel is closed.
        ///
        /// When a message is received, it is moved out of the channel by value,
        /// and replaced with a new slot according to the configured [recycling
        /// policy]. If all [`StaticSender`]s for this channel write to the channel's
        /// slots in place by using the [`send_ref`] or [`try_send_ref`] methods,
        /// consider using the [`recv_ref`] method instead, to enable the
        /// reuse of heap allocations.
        ///
        /// # Examples
        ///
        /// ```
        /// use thingbuf::mpsc::StaticChannel;
        ///
        /// static CHANNEL: StaticChannel<i32, 100> = StaticChannel::new();
        ///
        /// #[tokio::main]
        /// async fn main() {
        ///     let (tx, rx) = CHANNEL.split();
        ///
        ///     tokio::spawn(async move {
        ///         tx.send(1).await.unwrap();
        ///     });
        ///
        ///     assert_eq!(Some(1), rx.recv().await);
        ///     assert_eq!(None, rx.recv().await);
        /// }
        /// ```
        ///
        /// Values are buffered:
        ///
        /// ```
        /// use thingbuf::mpsc::StaticChannel;
        ///
        /// static CHANNEL: StaticChannel<i32, 100> = StaticChannel::new();
        ///
        /// #[tokio::main]
        /// async fn main() {
        ///     let (tx, rx) = CHANNEL.split();
        ///
        ///     tx.send(1).await.unwrap();
        ///     tx.send(2).await.unwrap();
        ///
        ///     assert_eq!(Some(1), rx.recv().await);
        ///     assert_eq!(Some(2), rx.recv().await);
        /// }
        /// ```
        ///
        /// [`send_ref`]: StaticSender::send_ref
        /// [`try_send_ref`]: StaticSender::try_send_ref
        /// [recycling policy]: crate::recycling::Recycle
        /// [`recv_ref`]: Self::recv_ref
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

        /// Attempts to receive the next message for this receiver by reference
        /// without waiting for a new message when the channel is empty.
        ///
        /// This method differs from [`recv_ref`] by returning immediately if the
        /// channel is empty or closed.
        ///
        /// # Errors
        ///
        /// This method returns an error when the channel is closed or there are
        /// no remaining messages in the channel's buffer.
        ///
        /// # Examples
        ///
        /// ```
        /// use thingbuf::mpsc::{channel, errors::TryRecvError};
        ///
        /// let (tx, rx) = channel(100);
        /// assert!(matches!(rx.try_recv_ref(), Err(TryRecvError::Empty)));
        ///
        /// tx.try_send(1).unwrap();
        /// drop(tx);
        ///
        /// assert_eq!(*rx.try_recv_ref().unwrap(), 1);
        /// assert!(matches!(rx.try_recv_ref(), Err(TryRecvError::Closed)));
        /// ```
        ///
        /// [`recv_ref`]: Self::recv_ref
        pub fn try_recv_ref(&self) -> Result<RecvRef<'_, T>, TryRecvError>
        where
            R: Recycle<T>,
        {
            self.core.try_recv_ref(self.slots.as_ref()).map(RecvRef)
        }

        /// Attempts to receive the next message for this receiver by reference
        /// without waiting for a new message when the channel is empty.
        ///
        /// This method differs from [`recv`] by returning immediately if the
        /// channel is empty or closed.
        ///
        /// # Errors
        ///
        /// This method returns an error when the channel is closed or there are
        /// no remaining messages in the channel's buffer.
        ///
        /// # Examples
        ///
        /// ```
        /// use thingbuf::mpsc::{channel, errors::TryRecvError};
        ///
        /// let (tx, rx) = channel(100);
        /// assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
        ///
        /// tx.try_send(1).unwrap();
        /// drop(tx);
        ///
        /// assert_eq!(rx.try_recv().unwrap(), 1);
        /// assert_eq!(rx.try_recv(), Err(TryRecvError::Closed));
        /// ```
        ///
        /// [`recv`]: Self::recv
        pub fn try_recv(&self) -> Result<T, TryRecvError>
        where
            R: Recycle<T>,
        {
            self.core.try_recv(self.slots.as_ref(), self.recycle)
        }

        /// Attempts to receive a message *by reference* from this channel,
        /// registering the current task for wakeup if the a message is not yet
        /// available, and returning `None` if the channel has closed and all
        /// messages have been received.
        ///
        /// Like [`StaticReceiver::recv_ref`], this method returns a [`RecvRef`]
        /// that can be used to read from (or mutate) the received message by
        /// reference. When the [`RecvRef`] is dropped, the receive operation
        /// completes and the slot occupied by the received message becomes
        /// usable for a future [`send_ref`] operation.
        ///
        /// If all [`StaticSender`]s for this channel write to the channel's slots in
        /// place by using the [`send_ref`] or [`try_send_ref`] methods, this
        /// method allows messages that own heap allocations to be reused in
        /// place.
        ///
        /// To wait asynchronously until a message becomes available, use the
        /// [`recv_ref`] method instead.
        ///
        /// # Returns
        ///
        ///  * `Poll::Pending` if no messages are available but the channel is not
        ///    closed, or if a spurious failure happens.
        ///  * `Poll::Ready(Some(RecvRef<T>))` if a message is available.
        ///  * `Poll::Ready(None)` if the channel has been closed (i.e., all
        ///    [`StaticSender`]s have been dropped), and all messages sent before it
        ///    was closed have been received.
        ///
        /// When the method returns [`Poll::Pending`], the [`Waker`] in the provided
        /// [`Context`] is scheduled to receive a wakeup when a message is sent on any
        /// sender, or when the channel is closed.  Note that on multiple calls to
        /// `poll_recv_ref`, only the [`Waker`] from the [`Context`] passed to the most
        /// recent call is scheduled to receive a wakeup.
        ///
        /// [`send_ref`]: StaticSender::send_ref
        /// [`try_send_ref`]: StaticSender::try_send_ref
        /// [`recv_ref`]: Self::recv_ref
        pub fn poll_recv_ref(&self, cx: &mut Context<'_>) -> Poll<Option<RecvRef<'_, T>>> {
            poll_recv_ref(self.core, self.slots, cx)
        }

        /// Attempts to receive a message *by value* from this channel,
        /// registering the current task for wakeup if the value is not yet
        /// available, and returning `None` if the channel has closed and all
        /// messages have been received.
        ///
        /// When a message is received, it is moved out of the channel by value,
        /// and replaced with a new slot according to the configured [recycling
        /// policy]. If all [`StaticSender`]s for this channel write to the channel's
        /// slots in place by using the [`send_ref`] or [`try_send_ref`] methods,
        /// consider using the [`poll_recv_ref`] method instead, to enable the
        /// reuse of heap allocations.
        ///
        /// To wait asynchronously until a message becomes available, use the
        /// [`recv`] method instead.
        ///
        /// # Returns
        ///
        ///  * `Poll::Pending` if no messages are available but the channel is not
        ///    closed, or if a spurious failure happens.
        ///  * `Poll::Ready(Some(message))` if a message is available.
        ///  * `Poll::Ready(None)` if the channel has been closed (i.e., all
        ///    [`StaticSender`]s have been dropped) and all messages sent before it
        ///    was closed have been received.
        ///
        /// When the method returns [`Poll::Pending`], the [`Waker`] in the provided
        /// [`Context`] is scheduled to receive a wakeup when a message is sent on any
        /// sender, or when the channel is closed.  Note that on multiple calls to
        /// `poll_recv`, only the [`Waker`] from the [`Context`] passed to the most
        /// recent call is scheduled to receive a wakeup.
        ///
        /// [`send_ref`]: StaticSender::send_ref
        /// [`try_send_ref`]: StaticSender::try_send_ref
        /// [recycling policy]: crate::recycling::Recycle
        /// [`poll_recv_ref`]: Self::poll_recv_ref
        /// [`recv`]: Self::recv
        pub fn poll_recv(&self, cx: &mut Context<'_>) -> Poll<Option<T>>
        where
            R: Recycle<T>,
        {
            self.poll_recv_ref(cx)
                .map(|opt| opt.map(|mut r| recycling::take(&mut *r, self.recycle)))
        }

        /// Returns `true` if the channel has closed (all corresponding
        /// [`StaticSender`]s have been dropped).
        ///
        /// If this method returns `true`, no new messages will become available
        /// on this channel. Previously sent messages may still be available.
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
    /// A reference to a message being sent to an asynchronous channel.
    ///
    /// A `SendRef` represents the exclusive permission to mutate a given
    /// element in a channel. A `SendRef<T>` [implements `DerefMut<T>`] to allow
    /// writing to that element. This is analogous to the [`Ref`] type, except
    /// that it completes a `send_ref` or `try_send_ref` operation when it is
    /// dropped.
    ///
    /// This type is returned by the [`Sender::send_ref`] and
    /// [`Sender::try_send_ref`] (or [`StaticSender::send_ref`] and
    /// [`StaticSender::try_send_ref`]) methods.
    ///
    /// [implements `DerefMut<T>`]: #impl-DerefMut
    /// [`Ref`]: crate::Ref
    pub struct SendRef<Waker>;
}

impl_recv_ref! {
    /// A reference to a message being received from an asynchronous channel.
    ///
    /// A `RecvRef` represents the exclusive permission to mutate a given
    /// element in a channel. A `RecvRef<T>` [implements `DerefMut<T>`] to allow
    /// writing to that element. This is analogous to the [`Ref`] type, except
    /// that it completes a `recv_ref` or `poll_recv_ref` operation when it is
    /// dropped.
    ///
    /// This type is returned by the [`Receiver::recv_ref`] and
    /// [`Receiver::poll_recv_ref`] (or [`StaticReceiver::recv_ref`] and
    /// [`StaticReceiver::poll_recv_ref`]) methods.
    ///
    /// [implements `DerefMut<T>`]: #impl-DerefMut
    /// [`Ref`]: crate::Ref
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
            some.map(|slot| {
                RecvRef(RecvRefInner {
                    _notify: super::NotifyTx(&core.tx_wait),
                    slot,
                })
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
