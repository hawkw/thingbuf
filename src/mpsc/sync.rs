//! A synchronous multi-producer, single-consumer channel.
//!
//! This provides an equivalent API to the [`mpsc`](crate::mpsc) module, but the
//! [`Receiver`] type in this module waits by blocking the current thread,
//! rather than asynchronously yielding.
use super::*;
use crate::{
    loom::{
        atomic::{self, Ordering},
        sync::Arc,
        thread::{self, Thread},
    },
    recycling::{self, Recycle},
    util::Backoff,
    wait::queue,
    Ref,
};
use core::{fmt, pin::Pin};

/// Returns a new synchronous multi-producer, single consumer channel.
pub fn channel<T: Default + Clone>(capacity: usize) -> (Sender<T>, Receiver<T>) {
    with_recycle(capacity, recycling::DefaultRecycle::new())
}

/// Returns a new synchronous multi-producer, single consumer channel with
/// the provided [recycling policy].
///
/// [recycling policy]: crate::recycling::Recycle
pub fn with_recycle<T, R: Recycle<T>>(
    capacity: usize,
    recycle: R,
) -> (Sender<T, R>, Receiver<T, R>) {
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
pub struct Sender<T, R = recycling::DefaultRecycle> {
    inner: Arc<Inner<T, R>>,
}

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
    pub struct StaticChannel<T, const CAPACITY: usize, R = recycling::DefaultRecycle> {
        core: ChannelCore<Thread>,
        slots: [Slot<T>; CAPACITY],
        is_split: AtomicBool,
        recycle: R,
    }

    pub struct StaticSender<T: 'static, R: 'static = recycling::DefaultRecycle> {
        core: &'static ChannelCore<Thread>,
        slots: &'static [Slot<T>],
        recycle: &'static R,
    }

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
        pub fn try_send_ref(&self) -> Result<SendRef<'_, T>, TrySendError> {
            self.core
                .try_send_ref(self.slots, self.recycle)
                .map(SendRef)
        }

        pub fn try_send(&self, val: T) -> Result<(), TrySendError<T>> {
            self.core.try_send(self.slots, val, self.recycle)
        }

        pub fn send_ref(&self) -> Result<SendRef<'_, T>, Closed> {
            send_ref(self.core, self.slots, self.recycle)
        }

        pub fn send(&self, val: T) -> Result<(), Closed<T>> {
            match self.send_ref() {
                Err(Closed(())) => Err(Closed(val)),
                Ok(mut slot) => {
                    slot.with_mut(|slot| *slot = val);
                    Ok(())
                }
            }
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
        pub fn recv_ref(&self) -> Option<RecvRef<'_, T>> {
            recv_ref(self.core, self.slots)
        }

        pub fn recv(&self) -> Option<T>
        where
            R: Recycle<T>,
        {
            let mut val = self.recv_ref()?;
            Some(recycling::take(&mut *val, self.recycle))
        }

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
    pub struct SendRef<Thread>;
}

impl_recv_ref! {
    pub struct RecvRef<Thread>;
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

    pub fn send_ref(&self) -> Result<SendRef<'_, T>, Closed> {
        send_ref(
            &self.inner.core,
            self.inner.slots.as_ref(),
            &self.inner.recycle,
        )
    }

    pub fn send(&self, val: T) -> Result<(), Closed<T>> {
        match self.send_ref() {
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
        if self.inner.core.core.close() {
            self.inner.core.rx_wait.close_tx();
        }
    }
}

// === impl Receiver ===

impl<T, R> Receiver<T, R> {
    pub fn recv_ref(&self) -> Option<RecvRef<'_, T>> {
        recv_ref(&self.inner.core, self.inner.slots.as_ref())
    }

    pub fn recv(&self) -> Option<T>
    where
        R: Recycle<T>,
    {
        let mut val = self.recv_ref()?;
        Some(recycling::take(&mut *val, &self.inner.recycle))
    }

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
                return r.map(|slot| RecvRef {
                    _notify: super::NotifyTx(&core.tx_wait),
                    slot,
                })
            }
            Poll::Pending => {
                test_println!("parking ({:?})", thread::current());
                thread::park();
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
