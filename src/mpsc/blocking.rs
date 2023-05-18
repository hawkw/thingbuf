//! A synchronous multi-producer, single-consumer channel.
//!
//! This provides an equivalent API to the [`mpsc`](crate::mpsc) module, but the
//! [`Receiver`] types in this module wait by blocking the current thread,
//! rather than asynchronously yielding.
use super::*;
use crate::{loom::{
    atomic::{self, Ordering},
    sync::Arc,
    thread::{self, Thread},
}, MAX_CAPACITY, recycling::{self, Recycle}, util::Backoff, wait::queue};
use core::{fmt, pin::Pin};
use errors::*;
use std::time::{Duration, Instant};

/// Returns a new synchronous multi-producer, single consumer (MPSC)
/// channel with  the provided capacity.
///
/// This channel will use the [default recycling policy].
///
/// [recycling policy]: crate::recycling::DefaultRecycle
#[must_use]
pub fn channel<T: Default + Clone>(capacity: usize) -> (Sender<T>, Receiver<T>) {
    with_recycle(capacity, recycling::DefaultRecycle::new())
}

/// Returns a new synchronous multi-producer, single consumer channel with
/// the provided [recycling policy].
///
/// # Panics
///
/// Panics if the capacity exceeds `usize::MAX & !(1 << (usize::BITS - 1))`. This value represents
/// the highest power of two that can be expressed by a `usize`, excluding the most significant bit.
///
/// [recycling policy]: crate::recycling::Recycle
#[must_use]
pub fn with_recycle<T, R: Recycle<T>>(
    capacity: usize,
    recycle: R,
) -> (Sender<T, R>, Receiver<T, R>) {
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

/// Synchronously receives values from associated [`Sender`]s.
///
/// Instances of this struct are created by the [`channel`] and
/// [`with_recycle`] functions.
#[derive(Debug)]
pub struct Sender<T, R = recycling::DefaultRecycle> {
    inner: Arc<Inner<T, R>>,
}

/// Synchronously sends values to an associated [`Receiver`].
///
/// Instances of this struct are created by the [`channel`] and
/// [`with_recycle`] functions.
#[derive(Debug)]
pub struct Receiver<T, R = recycling::DefaultRecycle> {
    inner: Arc<Inner<T, R>>,
}

struct Inner<T, R> {
    core: super::ChannelCore<Thread>,
    slots: Box<[Slot<T>]>,
    recycle: R,
}

#[cfg(not(all(loom, test)))]
feature! {
    #![feature = "static"]

    use crate::loom::atomic::AtomicBool;

    /// A statically-allocated, blocking bounded MPSC channel.
    ///
    /// A statically-allocated channel allows using a MPSC channel without
    /// requiring _any_ heap allocations. The [asynchronous variant][async] may be
    /// used in `#![no_std]` environments without requiring `liballoc`. This is a
    /// synchronous version which requires the Rust standard library, because it
    /// blocks the current thread in order to wait for send capacity. However, in
    /// some cases, it may offer _very slightly_ better performance than the
    /// non-static blocking channel due to requiring fewer heap pointer
    /// dereferences.
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
    /// use thingbuf::mpsc::blocking::StaticChannel;
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
    ///
    /// [async]: crate::mpsc::StaticChannel
    /// [`split`]: StaticChannel::split
    pub struct StaticChannel<T, const CAPACITY: usize, R = recycling::DefaultRecycle> {
        core: ChannelCore<Thread>,
        slots: [Slot<T>; CAPACITY],
        is_split: AtomicBool,
        recycle: R,
    }

    /// Synchronously sends values to an associated [`StaticReceiver`].
    ///
    /// Instances of this struct are created by the [`StaticChannel::split`] and
    /// [``StaticChannel::try_split`] functions.
    pub struct StaticSender<T: 'static, R: 'static = recycling::DefaultRecycle> {
        core: &'static ChannelCore<Thread>,
        slots: &'static [Slot<T>],
        recycle: &'static R,
    }

    /// Synchronously receives values from associated [`StaticSender`]s.
    ///
    /// Instances of this struct are created by the [`StaticChannel::split`] and
    /// [``StaticChannel::try_split`] functions.
    pub struct StaticReceiver<T: 'static, R: 'static = recycling::DefaultRecycle> {
        core: &'static ChannelCore<Thread>,
        slots: &'static [Slot<T>],
        recycle: &'static R,
    }

    // === impl StaticChannel ===

    impl<T, const CAPACITY: usize> StaticChannel<T, CAPACITY> {
        /// Constructs a new statically-allocated, blocking bounded MPSC channel.
        ///
        /// A statically-allocated channel allows using a MPSC channel without
        /// requiring _any_ heap allocations. The [asynchronous variant][async] may be
        /// used in `#![no_std]` environments without requiring `liballoc`. This is a
        /// synchronous version which requires the Rust standard library, because it
        /// blocks the current thread in order to wait for send capacity. However, in
        /// some cases, it may offer _very slightly_ better performance than the
        /// non-static blocking channel due to requiring fewer heap pointer
        /// dereferences.
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
        ///
        /// [async]: crate::mpsc::StaticChannel
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
                slots: &self.slots[..],
                recycle: &self.recycle,
            };
            let rx = StaticReceiver {
                core: &self.core,
                slots: &self.slots[..],
                recycle: &self.recycle,
            };
            Some((tx, rx))
        }
    }

    // === impl StaticSender ===

    impl<T, R> StaticSender<T, R>
    where
        R: Recycle<T>,
    {
        /// Reserves a slot in the channel to mutate in place, blocking until
        /// there is a free slot to write to.
        ///
        /// This is similar to the [`send`] method, but, rather than taking a
        /// message by value to write to the channel, this method reserves a
        /// writable slot in the channel, and returns a [`SendRef`] that allows
        /// mutating the slot in place. If the [`StaticReceiver`] end of the
        /// channel uses the [`StaticReceiver::recv_ref`] method for receiving
        /// from the channel, this allows allocations for channel messages to be
        /// reused in place.
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
        /// ```
        /// use thingbuf::mpsc::blocking::StaticChannel;
        /// use std::{fmt::Write, thread};
        ///
        /// static CHANNEL: StaticChannel<String, 8> = StaticChannel::new();
        ///
        /// let (tx, rx) = CHANNEL.split();
        ///
        /// // Spawn a thread that prints each message received from the channel:
        /// thread::spawn(move || {
        ///     for _ in 0..10 {
        ///         let msg = rx.recv_ref().unwrap();
        ///         println!("{}", msg);
        ///     }
        /// });
        ///
        /// // Until the channel closes, write formatted messages to the channel.
        /// let mut count = 1;
        /// while let Ok(mut value) = tx.send_ref() {
        ///     // Writing to the `SendRef` will reuse the *existing* string
        ///     // allocation in place.
        ///     write!(value, "hello from message {}", count)
        ///         .expect("writing to a `String` should never fail");
        ///     count += 1;
        /// }
        /// ```
        ///
        /// [`send`]: Self::send
        pub fn send_ref(&self) -> Result<SendRef<'_, T>, Closed> {
            send_ref(self.core, self.slots, self.recycle)
        }

        /// Sends a message by value, blocking until there is a free slot to
        /// write to.
        ///
        /// This method takes the message by value, and replaces any previous
        /// value in the slot. This means that the channel will *not* function
        /// as an object pool while sending messages with `send`. This method is
        /// most appropriate when messages don't own reusable heap allocations,
        /// or when the [`StaticReceiver`] end of the channel must receive messages
        /// by moving them out of the channel by value (using the
        /// [`StaticReceiver::recv`] method). When messages in the channel own
        /// reusable heap allocations (such as `String`s or `Vec`s), and the
        /// [`StaticReceiver`] doesn't need to receive them by value, consider using
        /// [`send_ref`] instead, to enable allocation reuse.
        ///
        /// # Errors
        ///
        /// If the [`StaticReceiver`] end of the channel has been dropped, this
        /// returns a [`Closed`] error containing the sent value.
        ///
        /// # Examples
        ///
        /// ```
        /// use thingbuf::mpsc::blocking::StaticChannel;
        /// use std::{fmt::Write, thread};
        ///
        /// static CHANNEL: StaticChannel<i32, 8> = StaticChannel::new();
        /// let (tx, rx) = CHANNEL.split();
        ///
        /// // Spawn a thread that prints each message received from the channel:
        /// thread::spawn(move || {
        ///     for _ in 0..10 {
        ///         let msg = rx.recv().unwrap();
        ///         println!("received message {}", msg);
        ///     }
        /// });
        ///
        /// // Until the channel closes, write the current iteration to the channel.
        /// let mut count = 1;
        /// while tx.send(count).is_ok() {
        ///     count += 1;
        /// }
        /// ```
        /// [`send_ref`]: Self::send_ref
        pub fn send(&self, val: T) -> Result<(), Closed<T>> {
            match self.send_ref() {
                Err(Closed(())) => Err(Closed(val)),
                Ok(mut slot) => {
                    *slot = val;
                    Ok(())
                }
            }
        }

        /// Reserves a slot in the channel to mutate in place, blocking until
        /// there is a free slot to write to, waiting for at most `timeout`.
        ///
        /// This is similar to the [`send_timeout`] method, but, rather than taking a
        /// message by value to write to the channel, this method reserves a
        /// writable slot in the channel, and returns a [`SendRef`] that allows
        /// mutating the slot in place. If the [`StaticReceiver`] end of the
        /// channel uses the [`StaticReceiver::recv_ref`] method for receiving
        /// from the channel, this allows allocations for channel messages to be
        /// reused in place.
        ///
        /// # Errors
        ///
        /// - [`Err`]`(`[`SendTimeoutError::Timeout`]`)` if the timeout has elapsed.
        /// - [`Err`]`(`[`SendTimeoutError::Closed`]`)` if the channel has closed.
        ///
        /// # Examples
        ///
        /// Sending formatted strings by writing them directly to channel slots,
        /// in place:
        ///
        /// ```
        /// use thingbuf::mpsc::{blocking::StaticChannel, errors::SendTimeoutError};
        /// use std::{fmt::Write, time::Duration, thread};
        ///
        /// static CHANNEL: StaticChannel<String, 1> = StaticChannel::new();
        /// let (tx, rx) = CHANNEL.split();
        ///
        /// thread::spawn(move || {
        ///     thread::sleep(Duration::from_millis(500));
        ///     let msg = rx.recv_ref().unwrap();
        ///     println!("{}", msg);
        ///     thread::sleep(Duration::from_millis(500));
        /// });
        ///
        /// thread::spawn(move || {
        ///     let mut value = tx.send_ref_timeout(Duration::from_millis(200)).unwrap();
        ///     write!(value, "hello").expect("writing to a `String` should never fail");
        ///     thread::sleep(Duration::from_millis(400));
        ///
        ///     let mut value = tx.send_ref_timeout(Duration::from_millis(200)).unwrap();
        ///     write!(value, "world").expect("writing to a `String` should never fail");
        ///     thread::sleep(Duration::from_millis(400));
        ///
        ///     assert_eq!(
        ///         Err(&SendTimeoutError::Timeout(())),
        ///         tx.send_ref_timeout(Duration::from_millis(200)).as_deref().map(String::as_str)
        ///     );
        /// });
        /// ```
        ///
        /// [`send_timeout`]: Self::send_timeout
        #[cfg(not(all(test, loom)))]
        pub fn send_ref_timeout(&self, timeout: Duration) -> Result<SendRef<'_, T>, SendTimeoutError> {
            send_ref_timeout(self.core, self.slots, self.recycle, timeout)
        }

        /// Sends a message by value, blocking until there is a free slot to
        /// write to, waiting for at most `timeout`.
        ///
        /// This method takes the message by value, and replaces any previous
        /// value in the slot. This means that the channel will *not* function
        /// as an object pool while sending messages with `send_timeout`. This method is
        /// most appropriate when messages don't own reusable heap allocations,
        /// or when the [`StaticReceiver`] end of the channel must receive messages
        /// by moving them out of the channel by value (using the
        /// [`StaticReceiver::recv`] method). When messages in the channel own
        /// reusable heap allocations (such as `String`s or `Vec`s), and the
        /// [`StaticReceiver`] doesn't need to receive them by value, consider using
        /// [`send_ref_timeout`] instead, to enable allocation reuse.
        ///
        /// # Errors
        ///
        /// - [`Err`]`(`[`SendTimeoutError::Timeout`]`)` if the timeout has elapsed.
        /// - [`Err`]`(`[`SendTimeoutError::Closed`]`)` if the channel has closed.
        ///
        /// # Examples
        ///
        /// ```
        /// use thingbuf::mpsc::{blocking::StaticChannel, errors::SendTimeoutError};
        /// use std::{time::Duration, thread};
        ///
        /// static CHANNEL: StaticChannel<i32, 1> = StaticChannel::new();
        /// let (tx, rx) = CHANNEL.split();
        ///
        /// thread::spawn(move || {
        ///     thread::sleep(Duration::from_millis(500));
        ///     let msg = rx.recv().unwrap();
        ///     println!("{}", msg);
        ///     thread::sleep(Duration::from_millis(500));
        /// });
        ///
        /// thread::spawn(move || {
        ///     tx.send_timeout(1, Duration::from_millis(200)).unwrap();
        ///     thread::sleep(Duration::from_millis(400));
        ///
        ///     tx.send_timeout(2, Duration::from_millis(200)).unwrap();
        ///     thread::sleep(Duration::from_millis(400));
        ///
        ///     assert_eq!(
        ///         Err(SendTimeoutError::Timeout(3)),
        ///         tx.send_timeout(3, Duration::from_millis(200))
        ///     );
        /// });
        /// ```
        ///
        /// [`send_ref_timeout`]: Self::send_ref_timeout
        #[cfg(not(all(test, loom)))]
        pub fn send_timeout(&self, val: T, timeout: Duration) -> Result<(), SendTimeoutError<T>> {
            match self.send_ref_timeout(timeout) {
                Err(e) => Err(e.with_value(val)),
                Ok(mut slot) => {
                    *slot = val;
                    Ok(())
                }
            }
        }

        /// Attempts to reserve a slot in the channel to mutate in place,
        /// without blocking until capacity is available.
        ///
        /// This method differs from [`send_ref`] by returning immediately if the
        /// channel’s buffer is full or no [`StaticReceiver`] exists. Compared with
        /// [`send_ref`], this method has two failure cases instead of one (one for
        /// disconnection, one for a full buffer), and this method will never block.
        ///
        /// Like [`send_ref`], this method returns a [`SendRef`] that may be
        /// used to mutate a slot in the channel's buffer in place. Dropping the
        /// [`SendRef`] completes the send operation and makes the mutated value
        /// available to the [`StaticReceiver`].
        ///
        /// # Errors
        ///
        /// If the channel capacity has been reached (i.e., the channel has `n`
        /// buffered values where `n` is the `usize` const generic parameter of
        /// the [`StaticChannel`]), then [`TrySendError::Full`] is returned. In
        /// this case, a future call to `try_send` may succeed if additional
        /// capacity becomes available.
        ///
        /// If the receive half of the channel is closed (i.e., the [`StaticReceiver`]
        /// handle was dropped), the function returns [`TrySendError::Closed`].
        /// Once the channel has closed, subsequent calls to `try_send_ref` will
        /// never succeed.
        ///
        /// [`send_ref`]: Self::send_ref
        pub fn try_send_ref(&self) -> Result<SendRef<'_, T>, TrySendError> {
            self.core
                .try_send_ref(self.slots, self.recycle)
                .map(SendRef)
        }

        /// Attempts to send a message by value immediately, without blocking until
        /// capacity is available.
        ///
        /// This method differs from [`send`] by returning immediately if the
        /// channel’s buffer is full or no [`StaticReceiver`] exists. Compared
        /// with  [`send`], this method has two failure cases instead of one (one for
        /// disconnection, one for a full buffer), and this method will never block.
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
        /// [`StaticReceiver`]  handle was dropped), the function returns
        /// [`TrySendError::Closed`]. Once the channel has closed, subsequent
        /// calls to `try_send` will  never succeed.
        ///
        /// In both cases, the error includes the value passed to `try_send`.
        ///
        /// [`send`]: Self::send
        pub fn try_send(&self, val: T) -> Result<(), TrySendError<T>> {
            self.core.try_send(self.slots, val, self.recycle)
        }
    }

    impl<T, R> Clone for StaticSender<T, R> {
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
            if self.core.core.close() {
                self.core.rx_wait.close_tx();
            }
        }
    }

    impl<T, R: fmt::Debug> fmt::Debug for StaticSender<T, R> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("StaticSender")
                .field("core", &self.core)
                .field("slots", &format_args!("&[..]"))
                .field("recycle", &self.recycle)
                .finish()
        }
    }

    // === impl StaticReceiver ===

    impl<T, R> StaticReceiver<T, R> {
        /// Receives the next message for this receiver, **by reference**.
        ///
        /// This method returns `None` if the channel has been closed and there are
        /// no remaining messages in the channel's buffer. This indicates that no
        /// further values can ever be received from this `StaticReceiver`. The channel is
        /// closed when all [`StaticSender`]s have been dropped.
        ///
        /// If there are no messages in the channel's buffer, but the channel has
        /// not yet been closed, this method will block until a message is sent or
        /// the channel is closed.
        ///
        /// This method returns a [`RecvRef`] that can be used to read from (or
        /// mutate) the received message by reference. When the [`RecvRef`] is
        /// dropped, the receive operation completes and the slot occupied by
        /// the received message becomes usable for a future [`send_ref`] operation.
        ///
        /// If all [`StaticSender`]s for this channel write to the channel's
        /// slots in place by using the [`send_ref`] or [`try_send_ref`]
        /// methods, this method allows messages that own heap allocations to be reused in
        /// place.
        ///
        /// # Examples
        ///
        /// ```
        /// use thingbuf::mpsc::blocking::StaticChannel;
        /// use std::{thread, fmt::Write};
        ///
        /// static CHANNEL: StaticChannel<String, 100> = StaticChannel::new();
        /// let (tx, rx) = CHANNEL.split();
        ///
        /// thread::spawn(move || {
        ///     let mut value = tx.send_ref().unwrap();
        ///     write!(value, "hello world!")
        ///         .expect("writing to a `String` should never fail");
        /// });
        ///
        /// assert_eq!(Some("hello world!"), rx.recv_ref().as_deref().map(String::as_str));
        /// assert_eq!(None, rx.recv().as_deref());
        /// ```
        ///
        /// Values are buffered:
        ///
        /// ```
        /// use thingbuf::mpsc::blocking::StaticChannel;
        /// use std::fmt::Write;
        ///
        /// static CHANNEL: StaticChannel<String, 100> = StaticChannel::new();
        /// let (tx, rx) = CHANNEL.split();
        ///
        /// write!(tx.send_ref().unwrap(), "hello").unwrap();
        /// write!(tx.send_ref().unwrap(), "world").unwrap();
        ///
        /// assert_eq!("hello", rx.recv_ref().unwrap().as_str());
        /// assert_eq!("world", rx.recv_ref().unwrap().as_str());
        /// ```
        ///
        /// [`send_ref`]: StaticSender::send_ref
        /// [`try_send_ref`]: StaticSender::try_send_ref
        pub fn recv_ref(&self) -> Option<RecvRef<'_, T>> {
            recv_ref(self.core, self.slots)
        }

        /// Receives the next message for this receiver, **by value**.
        ///
        /// This method returns `None` if the channel has been closed and there are
        /// no remaining messages in the channel's buffer. This indicates that no
        /// further values can ever be received from this `StaticReceiver`. The channel is
        /// closed when all [`StaticSender`]s have been dropped.
        ///
        /// If there are no messages in the channel's buffer, but the channel has
        /// not yet been closed, this method will block until a message is sent or
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
        /// use thingbuf::mpsc::blocking::StaticChannel;
        /// use std::thread;
        ///
        /// static CHANNEL: StaticChannel<i32, 100> = StaticChannel::new();
        /// let (tx, rx) = CHANNEL.split();
        ///
        /// thread::spawn(move || {
        ///    tx.send(1).unwrap();
        /// });
        ///
        /// assert_eq!(Some(1), rx.recv());
        /// assert_eq!(None, rx.recv());
        /// ```
        ///
        /// Values are buffered:
        ///
        /// ```
        /// use thingbuf::mpsc::blocking::StaticChannel;
        ///
        /// static CHANNEL: StaticChannel<i32, 100> = StaticChannel::new();
        /// let (tx, rx) = CHANNEL.split();
        ///
        /// tx.send(1).unwrap();
        /// tx.send(2).unwrap();
        ///
        /// assert_eq!(Some(1), rx.recv());
        /// assert_eq!(Some(2), rx.recv());
        /// ```
        ///
        /// [`send_ref`]: StaticSender::send_ref
        /// [`try_send_ref`]: StaticSender::try_send_ref
        /// [recycling policy]: crate::recycling::Recycle
        /// [`recv_ref`]: Self::recv_ref
        pub fn recv(&self) -> Option<T>
        where
            R: Recycle<T>,
        {
            let mut val = self.recv_ref()?;
            Some(recycling::take(&mut *val, self.recycle))
        }

        /// Receives the next message for this receiver, **by reference**, waiting for at most `timeout`.
        ///
        /// If there are no messages in the channel's buffer, but the channel has
        /// not yet been closed, this method will block until a message is sent,
        /// the channel is closed, or the provided `timeout` has elapsed.
        ///
        /// # Returns
        ///
        /// - [`Ok`]`(`[`RecvRef`]`<T>)` if a message was received.
        /// - [`Err`]`(`[`RecvTimeoutError::Timeout`]`)` if the timeout has elapsed.
        /// - [`Err`]`(`[`RecvTimeoutError::Closed`]`)` if the channel has closed.
        ///
        /// # Examples
        ///
        /// ```
        /// use thingbuf::mpsc::{blocking::StaticChannel, errors::RecvTimeoutError};
        /// use std::{thread, fmt::Write, time::Duration};
        ///
        /// static CHANNEL: StaticChannel<String, 100> = StaticChannel::new();
        /// let (tx, rx) = CHANNEL.split();
        ///
        /// thread::spawn(move || {
        ///     thread::sleep(Duration::from_millis(600));
        ///     let mut value = tx.send_ref().unwrap();
        ///     write!(value, "hello world!")
        ///         .expect("writing to a `String` should never fail");
        /// });
        ///
        /// assert_eq!(
        ///     Err(&RecvTimeoutError::Timeout),
        ///     rx.recv_ref_timeout(Duration::from_millis(400)).as_deref().map(String::as_str)
        /// );
        /// assert_eq!(
        ///     Ok("hello world!"),
        ///     rx.recv_ref_timeout(Duration::from_millis(400)).as_deref().map(String::as_str)
        /// );
        /// assert_eq!(
        ///     Err(&RecvTimeoutError::Closed),
        ///     rx.recv_ref_timeout(Duration::from_millis(400)).as_deref().map(String::as_str)
        /// );
        /// ```
        #[cfg(not(all(test, loom)))]
        pub fn recv_ref_timeout(&self, timeout: Duration) -> Result<RecvRef<'_, T>, RecvTimeoutError> {
            recv_ref_timeout(self.core, self.slots, timeout)
        }

        /// Receives the next message for this receiver, **by value**, waiting for at most `timeout`.
        ///
        /// If there are no messages in the channel's buffer, but the channel
        /// has not yet been closed, this method will block until a message is
        /// sent, the channel is closed, or the provided `timeout` has elapsed.
        ///
        /// When a message is received, it is moved out of the channel by value,
        /// and replaced with a new slot according to the configured [recycling
        /// policy]. If all [`StaticSender`]s for this channel write to the
        /// channel's slots in place by using the [`send_ref`] or
        /// [`try_send_ref`] methods, consider using the [`recv_ref_timeout`]
        /// method instead, to enable the reuse of heap allocations.
        ///
        /// [`send_ref`]: StaticSender::send_ref
        /// [`try_send_ref`]: StaticSender::try_send_ref
        /// [recycling policy]: crate::recycling::Recycle
        /// [`recv_ref_timeout`]: Self::recv_ref_timeout
        ///
        /// # Returns
        ///
        /// - [`Ok`]`(<T>)` if a message was received.
        /// - [`Err`]`(`[`RecvTimeoutError::Timeout`]`)` if the timeout has elapsed.
        /// - [`Err`]`(`[`RecvTimeoutError::Closed`]`)` if the channel has closed.
        ///
        /// # Examples
        ///
        /// ```
        /// use thingbuf::mpsc::{blocking::StaticChannel, errors::RecvTimeoutError};
        /// use std::{thread, time::Duration};
        ///
        /// static CHANNEL: StaticChannel<i32, 100> = StaticChannel::new();
        /// let (tx, rx) = CHANNEL.split();
        ///
        /// thread::spawn(move || {
        ///    thread::sleep(Duration::from_millis(600));
        ///    tx.send(1).unwrap();
        /// });
        ///
        /// assert_eq!(
        ///     Err(RecvTimeoutError::Timeout),
        ///     rx.recv_timeout(Duration::from_millis(400))
        /// );
        /// assert_eq!(
        ///     Ok(1),
        ///     rx.recv_timeout(Duration::from_millis(400))
        /// );
        /// assert_eq!(
        ///     Err(RecvTimeoutError::Closed),
        ///     rx.recv_timeout(Duration::from_millis(400))
        /// );
        /// ```
        #[cfg(not(all(test, loom)))]
        pub fn recv_timeout(&self, timeout: Duration) -> Result<T, RecvTimeoutError>
        where
            R: Recycle<T>,
        {
            let mut val = self.recv_ref_timeout(timeout)?;
            Ok(recycling::take(&mut *val, self.recycle))
        }

        /// Attempts to receive the next message for this receiver by reference
        /// without blocking.
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
        /// use thingbuf::mpsc::{blocking, errors::TryRecvError};
        ///
        /// let (tx, rx) = blocking::channel(100);
        /// assert!(matches!(rx.try_recv_ref(), Err(TryRecvError::Empty)));
        ///
        /// tx.send(1).unwrap();
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

        /// Attempts to receive the next message for this receiver by value
        /// without blocking.
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
        /// use thingbuf::mpsc::{blocking, errors::TryRecvError};
        ///
        /// let (tx, rx) = blocking::channel(100);
        /// assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
        ///
        /// tx.send(1).unwrap();
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

        /// Returns `true` if the channel has closed (all corresponding
        /// [`StaticSender`]s have been dropped).
        ///
        /// If this method returns `true`, no new messages will become available
        /// on this channel. Previously sent messages may still be available.
        pub fn is_closed(&self) -> bool {
            test_dbg!(self.core.tx_count.load(Ordering::SeqCst)) <= 1
        }
    }

    impl<'a, T, R> Iterator for &'a StaticReceiver<T, R> {
        type Item = RecvRef<'a, T>;

        fn next(&mut self) -> Option<Self::Item> {
            self.recv_ref()
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
    /// A reference to a message being sent to a blocking channel.
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
    pub struct SendRef<Thread>;
}

impl_recv_ref! {
    /// A reference to a message being received from a blocking channel.
    ///
    /// A `RecvRef` represents the exclusive permission to mutate a given
    /// element in a channel. A `RecvRef<T>` [implements `DerefMut<T>`] to allow
    /// writing to that element. This is analogous to the [`Ref`] type, except
    /// that it completes a `recv_ref` operation when it is dropped.
    ///
    /// This type is returned by the [`Receiver::recv_ref`] and
    /// [`StaticReceiver::recv_ref`] methods.
    ///
    /// [implements `DerefMut<T>`]: #impl-DerefMut
    /// [`Ref`]: crate::Ref
    pub struct RecvRef<Thread>;
}

// === impl Sender ===

impl<T, R> Sender<T, R>
where
    R: Recycle<T>,
{
    /// Reserves a slot in the channel to mutate in place, blocking until
    /// there is a free slot to write to.
    ///
    /// This is similar to the [`send`] method, but, rather than taking a
    /// message by value to write to the channel, this method reserves a
    /// writable slot in the channel, and returns a [`SendRef`] that allows
    /// mutating the slot in place. If the [`Receiver`] end of the channel
    /// uses the [`Receiver::recv_ref`] method for receiving from the channel,
    /// this allows allocations for channel messages to be reused in place.
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
    /// use thingbuf::mpsc::blocking;
    /// use std::{fmt::Write, thread};
    ///
    /// let (tx, rx) = blocking::channel::<String>(8);
    ///
    /// // Spawn a thread that prints each message received from the channel:
    /// thread::spawn(move || {
    ///     for _ in 0..10 {
    ///         let msg = rx.recv_ref().unwrap();
    ///         println!("{}", msg);
    ///     }
    /// });
    ///
    /// // Until the channel closes, write formatted messages to the channel.
    /// let mut count = 1;
    /// while let Ok(mut value) = tx.send_ref() {
    ///     // Writing to the `SendRef` will reuse the *existing* string
    ///     // allocation in place.
    ///     write!(value, "hello from message {}", count)
    ///         .expect("writing to a `String` should never fail");
    ///     count += 1;
    /// }
    /// ```
    ///
    /// [`send`]: Self::send
    pub fn send_ref(&self) -> Result<SendRef<'_, T>, Closed> {
        send_ref(
            &self.inner.core,
            self.inner.slots.as_ref(),
            &self.inner.recycle,
        )
    }

    /// Sends a message by value, blocking until there is a free slot to
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
    /// use thingbuf::mpsc::blocking;
    /// use std::thread;
    ///
    /// let (tx, rx) = blocking::channel(8);
    ///
    /// // Spawn a thread that prints each message received from the channel:
    /// thread::spawn(move || {
    ///     for _ in 0..10 {
    ///         let msg = rx.recv().unwrap();
    ///         println!("received message {}", msg);
    ///     }
    /// });
    ///
    /// // Until the channel closes, write the current iteration to the channel.
    /// let mut count = 1;
    /// while tx.send(count).is_ok() {
    ///     count += 1;
    /// }
    /// ```
    /// [`send_ref`]: Self::send_ref
    pub fn send(&self, val: T) -> Result<(), Closed<T>> {
        match self.send_ref() {
            Err(Closed(())) => Err(Closed(val)),
            Ok(mut slot) => {
                *slot = val;
                Ok(())
            }
        }
    }

    /// Reserves a slot in the channel to mutate in place, blocking until
    /// there is a free slot to write to, waiting for at most `timeout`.
    ///
    /// This is similar to the [`send_timeout`] method, but, rather than taking a
    /// message by value to write to the channel, this method reserves a
    /// writable slot in the channel, and returns a [`SendRef`] that allows
    /// mutating the slot in place. If the [`Receiver`] end of the channel
    /// uses the [`Receiver::recv_ref`] method for receiving from the channel,
    /// this allows allocations for channel messages to be reused in place.
    ///
    /// # Errors
    ///
    /// - [`Err`]`(`[`SendTimeoutError::Timeout`]`)` if the timeout has elapsed.
    /// - [`Err`]`(`[`SendTimeoutError::Closed`]`)` if the channel has closed.
    ///
    /// # Examples
    ///
    /// Sending formatted strings by writing them directly to channel slots,
    /// in place:
    ///
    /// ```
    /// use thingbuf::mpsc::{blocking, errors::SendTimeoutError};
    /// use std::{fmt::Write, time::Duration, thread};
    ///
    /// let (tx, rx) = blocking::channel::<String>(1);
    ///
    /// thread::spawn(move || {
    ///     thread::sleep(Duration::from_millis(500));
    ///     let msg = rx.recv_ref().unwrap();
    ///     println!("{}", msg);
    ///     thread::sleep(Duration::from_millis(500));
    /// });
    ///
    /// thread::spawn(move || {
    ///     let mut value = tx.send_ref_timeout(Duration::from_millis(200)).unwrap();
    ///     write!(value, "hello").expect("writing to a `String` should never fail");
    ///     thread::sleep(Duration::from_millis(400));
    ///
    ///     let mut value = tx.send_ref_timeout(Duration::from_millis(200)).unwrap();
    ///     write!(value, "world").expect("writing to a `String` should never fail");
    ///     thread::sleep(Duration::from_millis(400));
    ///
    ///     assert_eq!(
    ///         Err(&SendTimeoutError::Timeout(())),
    ///         tx.send_ref_timeout(Duration::from_millis(200)).as_deref().map(String::as_str)
    ///     );
    /// });
    /// ```
    ///
    /// [`send_timeout`]: Self::send_timeout
    #[cfg(not(all(test, loom)))]
    pub fn send_ref_timeout(&self, timeout: Duration) -> Result<SendRef<'_, T>, SendTimeoutError> {
        send_ref_timeout(
            &self.inner.core,
            self.inner.slots.as_ref(),
            &self.inner.recycle,
            timeout,
        )
    }

    /// Sends a message by value, blocking until there is a free slot to
    /// write to, for at most `timeout`.
    ///
    /// This method takes the message by value, and replaces any previous
    /// value in the slot. This means that the channel will *not* function
    /// as an object pool while sending messages with `send_timeout`. This method is
    /// most appropriate when messages don't own reusable heap allocations,
    /// or when the [`Receiver`] end of the channel must receive messages by
    /// moving them out of the channel by value (using the
    /// [`Receiver::recv`] method). When messages in the channel own
    /// reusable heap allocations (such as `String`s or `Vec`s), and the
    /// [`Receiver`] doesn't need to receive them by value, consider using
    /// [`send_ref_timeout`] instead, to enable allocation reuse.
    ///
    ///
    /// # Errors
    ///
    /// - [`Err`]`(`[`SendTimeoutError::Timeout`]`)` if the timeout has elapsed.
    /// - [`Err`]`(`[`SendTimeoutError::Closed`]`)` if the channel has closed.
    ///
    /// # Examples
    ///
    /// ```
    /// use thingbuf::mpsc::{blocking, errors::SendTimeoutError};
    /// use std::{time::Duration, thread};
    ///
    /// let (tx, rx) = blocking::channel(1);
    ///
    /// thread::spawn(move || {
    ///     thread::sleep(Duration::from_millis(500));
    ///     let msg = rx.recv().unwrap();
    ///     println!("{}", msg);
    ///     thread::sleep(Duration::from_millis(500));
    /// });
    ///
    /// thread::spawn(move || {
    ///     tx.send_timeout(1, Duration::from_millis(200)).unwrap();
    ///     thread::sleep(Duration::from_millis(400));
    ///
    ///     tx.send_timeout(2, Duration::from_millis(200)).unwrap();
    ///     thread::sleep(Duration::from_millis(400));
    ///
    ///     assert_eq!(
    ///         Err(SendTimeoutError::Timeout(3)),
    ///         tx.send_timeout(3, Duration::from_millis(200))
    ///     );
    /// });
    /// ```
    ///
    /// [`send_ref_timeout`]: Self::send_ref_timeout
    #[cfg(not(all(test, loom)))]
    pub fn send_timeout(&self, val: T, timeout: Duration) -> Result<(), SendTimeoutError<T>> {
        match self.send_ref_timeout(timeout) {
            Err(e) => Err(e.with_value(val)),
            Ok(mut slot) => {
                *slot = val;
                Ok(())
            }
        }
    }

    /// Attempts to reserve a slot in the channel to mutate in place,
    /// without blocking until capacity is available.
    ///
    /// This method differs from [`send_ref`] by returning immediately if the
    /// channel’s buffer is full or no [`Receiver`] exists. Compared with
    /// [`send_ref`], this method has two failure cases instead of one (one for
    /// disconnection, one for a full buffer), and this method will never block.
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

    /// Attempts to send a message by value immediately, without blocking until
    /// capacity is available.
    ///
    /// This method differs from [`send`] by returning immediately if the
    /// channel’s buffer is full or no [`Receiver`] exists. Compared with
    /// [`send`], this method has two failure cases instead of one (one for
    /// disconnection, one for a full buffer), and this method will never block.
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
        if self.inner.core.core.close() {
            self.inner.core.rx_wait.close_tx();
        }
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
    /// not yet been closed, this method will block until a message is sent or
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
    /// use thingbuf::mpsc::blocking;
    /// use std::{thread, fmt::Write};
    ///
    /// let (tx, rx) = blocking::channel::<String>(100);
    ///
    /// thread::spawn(move || {
    ///     let mut value = tx.send_ref().unwrap();
    ///     write!(value, "hello world!")
    ///         .expect("writing to a `String` should never fail");
    /// });
    ///
    /// assert_eq!(Some("hello world!"), rx.recv_ref().as_deref().map(String::as_str));
    /// assert_eq!(None, rx.recv().as_deref());
    /// ```
    ///
    /// Values are buffered:
    ///
    /// ```
    /// use thingbuf::mpsc::blocking;
    /// use std::fmt::Write;
    ///
    /// let (tx, rx) = blocking::channel::<String>(100);
    ///
    /// write!(tx.send_ref().unwrap(), "hello").unwrap();
    /// write!(tx.send_ref().unwrap(), "world").unwrap();
    ///
    /// assert_eq!("hello", rx.recv_ref().unwrap().as_str());
    /// assert_eq!("world", rx.recv_ref().unwrap().as_str());
    /// ```
    ///
    /// [`send_ref`]: Sender::send_ref
    /// [`try_send_ref`]: Sender::try_send_ref
    pub fn recv_ref(&self) -> Option<RecvRef<'_, T>> {
        recv_ref(&self.inner.core, self.inner.slots.as_ref())
    }

    /// Receives the next message for this receiver, **by value**.
    ///
    /// This method returns `None` if the channel has been closed and there are
    /// no remaining messages in the channel's buffer. This indicates that no
    /// further values can ever be received from this `Receiver`. The channel is
    /// closed when all [`Sender`]s have been dropped.
    ///
    /// If there are no messages in the channel's buffer, but the channel has
    /// not yet been closed, this method will block until a message is sent or
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
    /// use thingbuf::mpsc::blocking;
    /// use std::{thread, fmt::Write};
    ///
    /// let (tx, rx) = blocking::channel(100);
    ///
    /// thread::spawn(move || {
    ///    tx.send(1).unwrap();
    /// });
    ///
    /// assert_eq!(Some(1), rx.recv());
    /// assert_eq!(None, rx.recv());
    /// ```
    ///
    /// Values are buffered:
    ///
    /// ```
    /// use thingbuf::mpsc::blocking;
    ///
    /// let (tx, rx) = blocking::channel(100);
    ///
    /// tx.send(1).unwrap();
    /// tx.send(2).unwrap();
    ///
    /// assert_eq!(Some(1), rx.recv());
    /// assert_eq!(Some(2), rx.recv());
    /// ```
    ///
    /// [`send_ref`]: Sender::send_ref
    /// [`try_send_ref`]: Sender::try_send_ref
    /// [recycling policy]: crate::recycling::Recycle
    /// [`recv_ref`]: Self::recv_ref
    pub fn recv(&self) -> Option<T>
    where
        R: Recycle<T>,
    {
        let mut val = self.recv_ref()?;
        Some(recycling::take(&mut *val, &self.inner.recycle))
    }

    /// Receives the next message for this receiver, **by reference**, waiting for at most `timeout`.
    ///
    /// If there are no messages in the channel's buffer, but the channel has
    /// not yet been closed, this method will block until a message is sent,
    /// the channel is closed, or the provided `timeout` has elapsed.
    ///
    /// # Returns
    ///
    /// - [`Ok`]`(`[`RecvRef`]`<T>)` if a message was received.
    /// - [`Err`]`(`[`RecvTimeoutError::Timeout`]`)` if the timeout has elapsed.
    /// - [`Err`]`(`[`RecvTimeoutError::Closed`]`)` if the channel has closed.
    ///
    /// # Examples
    ///
    /// ```
    /// use thingbuf::mpsc::{blocking, errors::RecvTimeoutError};
    /// use std::{thread, fmt::Write, time::Duration};
    ///
    /// let (tx, rx) = blocking::channel::<String>(100);
    ///
    /// thread::spawn(move || {
    ///     thread::sleep(Duration::from_millis(600));
    ///     let mut value = tx.send_ref().unwrap();
    ///     write!(value, "hello world!")
    ///         .expect("writing to a `String` should never fail");
    /// });
    ///
    /// assert_eq!(
    ///     Err(&RecvTimeoutError::Timeout),
    ///     rx.recv_ref_timeout(Duration::from_millis(400)).as_deref().map(String::as_str)
    /// );
    /// assert_eq!(
    ///     Ok("hello world!"),
    ///     rx.recv_ref_timeout(Duration::from_millis(400)).as_deref().map(String::as_str)
    /// );
    /// assert_eq!(
    ///     Err(&RecvTimeoutError::Closed),
    ///     rx.recv_ref_timeout(Duration::from_millis(400)).as_deref().map(String::as_str)
    /// );
    /// ```
    #[cfg(not(all(test, loom)))]
    pub fn recv_ref_timeout(&self, timeout: Duration) -> Result<RecvRef<'_, T>, RecvTimeoutError> {
        recv_ref_timeout(&self.inner.core, self.inner.slots.as_ref(), timeout)
    }

    /// Receives the next message for this receiver, **by value**, waiting for at most `timeout`.
    ///
    /// If there are no messages in the channel's buffer, but the channel
    /// has not yet been closed, this method will block until a message is
    /// sent, the channel is closed, or the provided `timeout` has elapsed.
    ///
    /// When a message is received, it is moved out of the channel by value,
    /// and replaced with a new slot according to the configured [recycling
    /// policy]. If all [`Sender`]s for this channel write to the
    /// channel's slots in place by using the [`send_ref`] or
    /// [`try_send_ref`] methods, consider using the [`recv_ref_timeout`]
    /// method instead, to enable the reuse of heap allocations.
    ///
    /// [`send_ref`]: Sender::send_ref
    /// [`try_send_ref`]: Sender::try_send_ref
    /// [recycling policy]: crate::recycling::Recycle
    /// [`recv_ref_timeout`]: Self::recv_ref_timeout
    ///
    /// # Returns
    ///
    /// - [`Ok`]`(<T>)` if a message was received.
    /// - [`Err`]`(`[`RecvTimeoutError::Timeout`]`)` if the timeout has elapsed.
    /// - [`Err`]`(`[`RecvTimeoutError::Closed`]`)` if the channel has closed.
    ///
    /// # Examples
    ///
    /// ```
    /// use thingbuf::mpsc::{blocking, errors::RecvTimeoutError};
    /// use std::{thread, fmt::Write, time::Duration};
    ///
    /// let (tx, rx) = blocking::channel(100);
    ///
    /// thread::spawn(move || {
    ///    thread::sleep(Duration::from_millis(600));
    ///    tx.send(1).unwrap();
    /// });
    ///
    /// assert_eq!(
    ///     Err(RecvTimeoutError::Timeout),
    ///     rx.recv_timeout(Duration::from_millis(400))
    /// );
    /// assert_eq!(
    ///     Ok(1),
    ///     rx.recv_timeout(Duration::from_millis(400))
    /// );
    /// assert_eq!(
    ///     Err(RecvTimeoutError::Closed),
    ///     rx.recv_timeout(Duration::from_millis(400))
    /// );
    /// ```
    #[cfg(not(all(test, loom)))]
    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, RecvTimeoutError>
    where
        R: Recycle<T>,
    {
        let mut val = self.recv_ref_timeout(timeout)?;
        Ok(recycling::take(&mut *val, &self.inner.recycle))
    }

    /// Attempts to receive the next message for this receiver by reference
    /// without blocking.
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
    /// use thingbuf::mpsc::{blocking, errors::TryRecvError};
    ///
    /// let (tx, rx) = blocking::channel(100);
    /// assert!(matches!(rx.try_recv_ref(), Err(TryRecvError::Empty)));
    ///
    /// tx.send(1).unwrap();
    /// drop(tx);
    ///
    /// assert_eq!(*rx.try_recv_ref().unwrap(), 1);
    /// assert!(matches!(rx.try_recv_ref(), Err(TryRecvError::Closed)));
    /// ```
    ///
    /// [`recv_ref`]: Self::recv_ref
    pub fn try_recv_ref(&self) -> Result<RecvRef<'_, T>, TryRecvError> {
        self.inner
            .core
            .try_recv_ref(self.inner.slots.as_ref())
            .map(RecvRef)
    }

    /// Attempts to receive the next message for this receiver by value
    /// without blocking.
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
    /// use thingbuf::mpsc::{blocking, errors::TryRecvError};
    ///
    /// let (tx, rx) = blocking::channel(100);
    /// assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    ///
    /// tx.send(1).unwrap();
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
        self.inner
            .core
            .try_recv(self.inner.slots.as_ref(), &self.inner.recycle)
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

impl<'a, T, R> Iterator for &'a Receiver<T, R> {
    type Item = RecvRef<'a, T>;

    fn next(&mut self) -> Option<Self::Item> {
        self.recv_ref()
    }
}

impl<T, R> Drop for Receiver<T, R> {
    fn drop(&mut self) {
        self.inner.core.close_rx();
    }
}

// === impl Inner ===

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

#[inline]
fn recv_ref<'a, T>(core: &'a ChannelCore<Thread>, slots: &'a [Slot<T>]) -> Option<RecvRef<'a, T>> {
    loop {
        match core.poll_recv_ref(slots, thread::current) {
            Poll::Ready(r) => {
                return r.map(|slot| {
                    RecvRef(RecvRefInner {
                        _notify: super::NotifyTx(&core.tx_wait),
                        slot,
                    })
                })
            }
            Poll::Pending => {
                test_println!("parking ({:?})", thread::current());
                thread::park();
            }
        }
    }
}

#[cfg(not(all(test, loom)))]
#[inline]
fn recv_ref_timeout<'a, T>(
    core: &'a ChannelCore<Thread>,
    slots: &'a [Slot<T>],
    timeout: Duration,
) -> Result<RecvRef<'a, T>, RecvTimeoutError> {
    let beginning_park = Instant::now();
    loop {
        match core.poll_recv_ref(slots, thread::current) {
            Poll::Ready(r) => {
                return r
                    .map(|slot| {
                        RecvRef(RecvRefInner {
                            _notify: super::NotifyTx(&core.tx_wait),
                            slot,
                        })
                    })
                    .ok_or(RecvTimeoutError::Closed);
            }
            Poll::Pending => {
                test_println!("park_timeout ({:?})", thread::current());
                thread::park_timeout(timeout);
                let elapsed = beginning_park.elapsed();
                if elapsed >= timeout {
                    return Err(RecvTimeoutError::Timeout);
                }
            }
        }
    }
}

#[inline]
fn send_ref<'a, T, R: Recycle<T>>(
    core: &'a ChannelCore<Thread>,
    slots: &'a [Slot<T>],
    recycle: &'a R,
) -> Result<SendRef<'a, T>, Closed<()>> {
    // fast path: avoid getting the thread and constructing the node if the
    // slot is immediately ready.
    match core.try_send_ref(slots, recycle) {
        Ok(slot) => return Ok(SendRef(slot)),
        Err(TrySendError::Closed(_)) => return Err(Closed(())),
        _ => {}
    }

    let mut waiter = queue::Waiter::new();
    let mut unqueued = true;
    let thread = thread::current();
    let mut boff = Backoff::new();
    loop {
        let node = unsafe {
            // Safety: in this case, it's totally safe to pin the waiter, as
            // it is owned uniquely by this function, and it cannot possibly
            // be moved while this thread is parked.
            Pin::new_unchecked(&mut waiter)
        };

        let wait = if unqueued {
            test_dbg!(core.tx_wait.start_wait(node, &thread))
        } else {
            test_dbg!(core.tx_wait.continue_wait(node, &thread))
        };

        match wait {
            WaitResult::Closed => return Err(Closed(())),
            WaitResult::Notified => {
                boff.spin_yield();
                match core.try_send_ref(slots.as_ref(), recycle) {
                    Ok(slot) => return Ok(SendRef(slot)),
                    Err(TrySendError::Closed(_)) => return Err(Closed(())),
                    _ => {}
                }
            }
            WaitResult::Wait => {
                unqueued = false;
                thread::park();
            }
        }
    }
}

#[cfg(not(all(test, loom)))]
#[inline]
fn send_ref_timeout<'a, T, R: Recycle<T>>(
    core: &'a ChannelCore<Thread>,
    slots: &'a [Slot<T>],
    recycle: &'a R,
    timeout: Duration,
) -> Result<SendRef<'a, T>, SendTimeoutError> {
    // fast path: avoid getting the thread and constructing the node if the
    // slot is immediately ready.
    match core.try_send_ref(slots, recycle) {
        Ok(slot) => return Ok(SendRef(slot)),
        Err(TrySendError::Closed(_)) => return Err(SendTimeoutError::Closed(())),
        _ => {}
    }

    let mut waiter = queue::Waiter::new();
    let mut unqueued = true;
    let thread = thread::current();
    let mut boff = Backoff::new();
    let beginning_park = Instant::now();
    loop {
        let node = unsafe {
            // Safety: in this case, it's totally safe to pin the waiter, as
            // it is owned uniquely by this function, and it cannot possibly
            // be moved while this thread is parked.
            Pin::new_unchecked(&mut waiter)
        };

        let wait = if unqueued {
            test_dbg!(core.tx_wait.start_wait(node, &thread))
        } else {
            test_dbg!(core.tx_wait.continue_wait(node, &thread))
        };

        match wait {
            WaitResult::Closed => return Err(SendTimeoutError::Closed(())),
            WaitResult::Notified => {
                boff.spin_yield();
                match core.try_send_ref(slots.as_ref(), recycle) {
                    Ok(slot) => return Ok(SendRef(slot)),
                    Err(TrySendError::Closed(_)) => return Err(SendTimeoutError::Closed(())),
                    _ => {}
                }
            }
            WaitResult::Wait => {
                unqueued = false;
                thread::park_timeout(timeout);
                let elapsed = beginning_park.elapsed();
                if elapsed >= timeout {
                    return Err(SendTimeoutError::Timeout(()));
                }
            }
        }
    }
}
