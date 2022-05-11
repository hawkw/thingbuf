//! Multi-producer, single-consumer channels using [`ThingBuf`].
//!
//! A _channel_ is a synchronization and communication primitive that combines a
//! shared queue with the ability to _wait_. Channels provide a [send]
//! operation, which enqueues a message if there is capacity in the queue, or
//! waits for capacity to become available if there is none; and a [receive]
//! operation, which dequeues a message from the queue if any are available, or
//! waits for a message to be sent if the queue is empty. This module provides
//! an implementation of multi-producer, single-consumer channels built on top
//! of the lock-free queue implemented by [`ThingBuf`].
//!
//! The channel API in this module is broadly similar to the one provided by
//! other implementations, such as [`std::sync::mpsc`], [`tokio::sync::mpsc`],
//! or [`crossbeam::channel`]. A given channel instance is represented by a pair
//! of types:
//!
//! * a [`Sender`] handle, which represents the capacity to [send] messages to the
//!   channel
//! * a [`Receiver`] handle, which represents the capacity to [receive] messages
//!   from the channel
//!
//! ```
//! // Constructing a channel returns a paired `Sender` and `Receiver`:
//! let (tx, rx) = thingbuf::mpsc::channel::<String>(10);
//! # drop((tx, rx));
//! ```
//!
//! As these are multi-producer, single-consumer channels, the [`Sender`] type
//! implements `Clone`; it may be cloned any number of times to create multiple
//! [`Sender`]s that send messages to the same channel. On the other hand, each
//! channel instance has only a single [`Receiver`].
//!
//! # Disconnection
//!
//! When all [`Sender`] handles have been dropped, it is no longer
//! possible to send values into the channel. When this occurs, the channel is
//! considered to have been closed by the sender; calling [`Receiver::recv`]
//! once a channel has closed will return `None`. Note that if the [`Receiver`] has
//! not received any messages sent prior to the last [`Sender`] being dropped,
//! those messages will be returned before [`Receiver::recv`] returns `None`.
//!
//! If the [`Receiver`] handle is dropped, then messages can no longer
//! be read out of the channel. In this case, all further attempts to send will
//! result in an error.
//!
//! # Channel Flavors
//!
//! This module contains several different "flavors" of multi-producer,
//! single-consumer channels. The primary differences between the different
//! channel implementations are whether they wait asynchronously or by blocking
//! the current thread, and whether the array that stores channel messages is
//! allocated dynamically on the heap or statically at compile-time.
//!
//! |              | **Dynamic Allocation** | **Static Allocation**        |
//! |--------------|------------------------|------------------------------|
//! | **Async**    | [`channel`]            | [`StaticChannel`]            |
//! | **Blocking** | [`blocking::channel`]  | [`blocking::StaticChannel`]  |
//!
//! ## Asynchronous and Blocking Channels
//!
//! The default channel implementation (in this module) are _asynchronous_.
//! With these channels, the send and receive operations are [`async fn`]s
//! &mdash; when it's necessary to wait to send or receive a message, the
//! [`Future`]s returned by these functions will _yield_, allowing other [tasks]
//! to execute. These asynchronous channels are suitable for use in an async
//! runtime like [`tokio`], or in embedded or bare-metal systems that use
//! `async`/`await` syntax. They do not require the Rust standard library.
//!
//! If the "std" feature flag is enabled, this module also provides a
//! [`blocking`] submodule, which implements _blocking_ (or _synchronous_)
//! channels. The [blocking receiver] will instead wait for new messages by
//! blocking the  current thread, and the [blocking sender] will wait for send
//! capacity by blocking the current thread. A blocking channel can be
//! constructed using the [`blocking::channel`] function. Naturally, blocking
//! the current thread requires thread APIs, so these channels are only
//! available when the Rust standard library is available.
//!
//! An asynchronous channel is used with asynchronous tasks:
//!
//! ```rust
//! use thingbuf::mpsc;
//!
//! #[tokio::main]
//! async fn main() {
//!     let (tx, rx) = mpsc::channel(8);
//!
//!     // Spawn some tasks that write to the channel:
//!     for i in 0..10 {
//!         let tx = tx.clone();
//!         tokio::spawn(async move {
//!             tx.send(i).await.unwrap();
//!         });
//!     }
//!
//!     // Print out every message recieved from the channel:
//!     for _ in 0..10 {
//!         let j = rx.recv().await.unwrap();
//!
//!         println!("received {}", j);
//!         assert!(0 <= j && j < 10);
//!     }
//! }
//! ```
//!
//! A blocking channel is used with threads:
//!
//! ```rust
//! use thingbuf::mpsc::blocking;
//! use std::thread;
//!
//! let (tx, rx) = blocking::channel(8);
//!
//! // Spawn some threads that write to the channel:
//! for i in 0..10 {
//!     let tx = tx.clone();
//!     thread::spawn(move || {
//!         tx.send(i).unwrap();
//!     });
//! }
//!
//! // Print out every message recieved from the channel:
//! for _ in 0..10 {
//!     let j = rx.recv().unwrap();
//!
//!     println!("received {}", j);
//!     assert!(0 <= j && j < 10);
//! }
//! ```
//!
//! ## Static and Dynamically Allocated Channels
//!
//! The other difference between channel flavors is whether the array that backs
//! the channel's queue is allocated dynamically on the heap, or allocated
//! statically.
//!
//! The default channels returned by [`channel`] and [`blocking::channel`] are
//! _dynamically allocated_: although they are fixed-size, the size of the
//! channel can be determined at runtime when the channel is created, and any
//! number of dynamically-allocated channels may be created and destroyed over
//! the lifetime of the program. Because these channels dynamically allocate
//! memory, they require the "alloc" feature flag.
//!
//! In some use cases, though, dynamic memory allocation may not be available
//! (such as in some embedded systems, or within a memory allocator
//! implementation), or it may be desirable to avoid dynamic memory allocation
//! (such as in very performance-sensitive applications). To support those use
//! cases, `thingbuf::mpsc` also provides _static channels_. The size of these
//! channels is determined at compile-time (by a const generic parameter) rather
//! than at runtime, so they may be constructed in a static initializer. This
//! allows allocating the array that backs the channel's queue statically, so
//! that dynamic allocation is not needed.The [`StaticChannel`] and
//! [`blocking::StaticChannel`] types are used to construct static channels.
//!
//! A dynamically allocated channel's size can be determined at runtime:
//!
//! ```
//! use thingbuf::mpsc::blocking;
//! use std::env;
//!
//! # // the `main` fn is used explicitly to show that this is an "application".
//! # #[allow(clippy::needless_doctest_main)]
//! fn main() {
//!     // Determine the channel capacity from the first command-line line
//!     // argument, if one is present.
//!     let channel_capacity = env::args()
//!         .nth(1)
//!         .and_then(|cap| cap.parse::<usize>().ok())
//!         .unwrap_or(16);
//!
//!     // Construct a dynamically-sized blocking channel.
//!     let (tx, rx) = blocking::channel(channel_capacity);
//!
//!     tx.send("hello world").unwrap();
//!
//!     // ...
//!     # drop(tx); drop(rx);
//! }
//! ```
//!
//! A statically allocated channel may be used without heap allocation:
//!
//! ```
//! // We are in a `no_std` context with no memory allocator!
//! #![no_std]
//! use thingbuf::mpsc;
//!
//! // Create a channel backed by a static array with 256 entries.
//! static KERNEL_EVENTS: mpsc::StaticChannel<KernelEvent, 256> = mpsc::StaticChannel::new();
//!
//! #[no_mangle]
//! pub fn kernel_main() {
//!     // Split the static channel into a sender/receiver pair.
//!     let (event_tx, event_rx) = KERNEL_EVENTS.split();
//!     let mut kernel_tasks = TaskExecutor::new();
//!
//!     kernel_tasks.spawn(async move {
//!         // Process kernel events
//!         while let Some(event) = event_rx.recv().await {
//!             // ...
//!             # drop(event);
//!         }
//!     });
//!
//!     // Some device driver that needs to emit events to the channel.
//!     kernel_tasks.spawn(some_device_driver(event_tx.clone()));
//!
//!     loop {
//!         kernel_tasks.tick();
//!     }
//! }
//!
//! async fn some_device_driver(event_tx: mpsc::StaticSender<KernelEvent>) {
//!     let mut device = SomeDevice::new(0x42);
//!
//!     loop {
//!         // When the device has data, emit a kernel event.
//!         match device.poll_for_events().await {
//!             Ok(event) => event_tx.send(event).await.unwrap(),
//!             Err(err) => event_tx.send(KernelEvent::from(err)).await.unwrap(),
//!         }
//!     }
//! }
//! # type KernelEvent = ();
//! # struct TaskExecutor;
//! # impl TaskExecutor {
//! #   const fn new() -> Self { Self }
//! #   fn spawn(&mut self, _: impl core::future::Future) {}
//! #   fn tick(&mut self) {}
//! # }
//! # struct SomeDevice(u64);
//! # impl SomeDevice {
//! #   fn new(u: u64) -> Self { Self(u) }
//! #   async fn poll_for_events(&mut self) -> Result<(), ()> { Ok(()) }
//! # }
//! # fn main() {}
//! ```
//!
//! [send]: Sender::send
//! [receive]: Receiver::recv
//! [`ThingBuf`]: crate::ThingBuf
//! [`tokio::sync::mpsc`]: https://docs.rs/tokio/latest/tokio/sync/mpsc/index.html
//! [`crossbeam::channel`]: https://docs.rs/crossbeam/latest/crossbeam/channel/index.html
//! [`async fn`]: https://rust-lang.github.io/async-book/01_getting_started/04_async_await_primer.html
//! [`Future`]: core::future::Future
//! [tasks]: core::task
//! [`tokio`]: https://tokio.rs
//! [blocking receiver]: blocking::Receiver
//! [blocking sender]: blocking::Sender
use crate::{
    loom::{atomic::AtomicUsize, hint},
    recycling::{take, Recycle},
    wait::{Notify, WaitCell, WaitQueue, WaitResult},
    Core, Ref, Slot,
};
use core::{fmt, task::Poll};

pub mod errors;
use self::errors::{TryRecvError, TrySendError};

#[derive(Debug)]
struct ChannelCore<N> {
    core: Core,
    rx_wait: WaitCell<N>,
    tx_count: AtomicUsize,
    tx_wait: WaitQueue<N>,
}

struct SendRefInner<'a, T, N: Notify> {
    // /!\ LOAD BEARING STRUCT DROP ORDER /!\
    //
    // The `Ref` field *must* be dropped before the `NotifyInner` field, or else
    // loom tests will fail. This ensures that the mutable access to the slot is
    // considered to have ended *before* the receiver thread/task is notified.
    //
    // The alternatives to a load-bearing drop order would be:
    // (a) put one field inside an `Option` so it can be dropped before the
    //     other (not great, as it adds a little extra overhead even outside
    //     of Loom tests),
    // (b) use `core::mem::ManuallyDrop` (also not great, requires additional
    //     unsafe code that in this case we can avoid)
    //
    // So, given that, relying on struct field drop order seemed like the least
    // bad option here. Just don't reorder these fields. :)
    slot: Ref<'a, T>,
    _notify: NotifyRx<'a, N>,
}

struct RecvRefInner<'a, T, N: Notify + Unpin> {
    // /!\ LOAD BEARING STRUCT DROP ORDER /!\
    //
    // The `Ref` field *must* be dropped before the `NotifyTx` field, or else
    // loom tests will fail. This ensures that the mutable access to the slot is
    // considered to have ended *before* the receiver thread/task is notified.
    //
    // The alternatives to a load-bearing drop order would be:
    // (a) put one field inside an `Option` so it can be dropped before the
    //     other (not great, as it adds a little extra overhead even outside
    //     of Loom tests),
    // (b) use `core::mem::ManuallyDrop` (also not great, requires additional
    //     unsafe code that in this case we can avoid)
    //
    // So, given that, relying on struct field drop order seemed like the least
    // bad option here. Just don't reorder these fields. :)
    slot: Ref<'a, T>,
    _notify: crate::mpsc::NotifyTx<'a, N>,
}

struct NotifyRx<'a, N: Notify>(&'a WaitCell<N>);
struct NotifyTx<'a, N: Notify + Unpin>(&'a WaitQueue<N>);

// ==== impl Inner ====

impl<N> ChannelCore<N> {
    #[cfg(not(loom))]
    const fn new(capacity: usize) -> Self {
        Self {
            core: Core::new(capacity),
            rx_wait: WaitCell::new(),
            tx_count: AtomicUsize::new(1),
            tx_wait: WaitQueue::new(),
        }
    }

    #[cfg(loom)]
    fn new(capacity: usize) -> Self {
        Self {
            core: Core::new(capacity),
            rx_wait: WaitCell::new(),
            tx_count: AtomicUsize::new(1),
            tx_wait: WaitQueue::new(),
        }
    }
}

impl<N> ChannelCore<N>
where
    N: Notify + Unpin,
{
    fn close_rx(&self) {
        if self.core.close() {
            crate::loom::hint::spin_loop();
            test_println!("draining_queue");
            self.tx_wait.close();
        }
    }
}

impl<N> ChannelCore<N>
where
    N: Notify + Unpin,
{
    fn try_send_ref<'a, T, R>(
        &'a self,
        slots: &'a [Slot<T>],
        recycle: &R,
    ) -> Result<SendRefInner<'a, T, N>, TrySendError>
    where
        R: Recycle<T>,
    {
        self.core.push_ref(slots, recycle).map(|slot| SendRefInner {
            _notify: NotifyRx(&self.rx_wait),
            slot,
        })
    }

    fn try_send<T, R>(&self, slots: &[Slot<T>], val: T, recycle: &R) -> Result<(), TrySendError<T>>
    where
        R: Recycle<T>,
    {
        match self.try_send_ref(slots, recycle) {
            Ok(mut slot) => {
                slot.with_mut(|slot| *slot = val);
                Ok(())
            }
            Err(e) => Err(e.with_value(val)),
        }
    }

    fn try_recv_ref<'a, T>(
        &'a self,
        slots: &'a [Slot<T>],
    ) -> Result<RecvRefInner<'a, T, N>, TryRecvError> {
        self.core.pop_ref(slots).map(|slot| RecvRefInner {
            _notify: NotifyTx(&self.tx_wait),
            slot,
        })
    }

    fn try_recv<T, R>(&self, slots: &[Slot<T>], recycle: &R) -> Result<T, TryRecvError>
    where
        R: Recycle<T>,
    {
        match self.try_recv_ref(slots) {
            Ok(mut slot) => Ok(take(&mut *slot, recycle)),
            Err(e) => Err(e),
        }
    }

    /// Performs one iteration of the `recv_ref` loop.
    ///
    /// The loop itself has to be written in the actual `send` method's
    /// implementation, rather than on `inner`, because it might be async and
    /// may yield, or might park the thread.
    fn poll_recv_ref<'a, T>(
        &'a self,
        slots: &'a [Slot<T>],
        mk_waiter: impl Fn() -> N,
    ) -> Poll<Option<Ref<'a, T>>> {
        macro_rules! try_poll_recv {
            () => {
                // If we got a value, return it!
                match self.core.pop_ref(slots) {
                    Ok(slot) => return Poll::Ready(Some(slot)),
                    Err(TryRecvError::Closed) => return Poll::Ready(None),
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
                    test_println!("-> yield");
                    return Poll::Pending;
                }
                WaitResult::Closed => {
                    // the channel is closed (all the receivers are dropped).
                    // however, there may be messages left in the queue. try
                    // popping from the queue until it's empty.
                    return Poll::Ready(self.core.pop_ref(slots).ok());
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

// === impl SendRefInner ===

impl<T, N: Notify> core::ops::Deref for SendRefInner<'_, T, N> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.slot.deref()
    }
}

impl<T, N: Notify> core::ops::DerefMut for SendRefInner<'_, T, N> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.slot.deref_mut()
    }
}

impl<T, N: Notify> SendRefInner<'_, T, N> {
    #[inline]
    pub(crate) fn with<U>(&self, f: impl FnOnce(&T) -> U) -> U {
        self.slot.with(f)
    }

    #[inline]
    pub(crate) fn with_mut<U>(&mut self, f: impl FnOnce(&mut T) -> U) -> U {
        self.slot.with_mut(f)
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

// === impl RecvRefInner ===

impl<T, N: Notify + Unpin> core::ops::Deref for RecvRefInner<'_, T, N> {
    type Target = T;
    #[inline]
    fn deref(&self) -> &Self::Target {
        self.slot.deref()
    }
}

impl<T, N: Notify + Unpin> core::ops::DerefMut for RecvRefInner<'_, T, N> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.slot.deref_mut()
    }
}

impl<T: fmt::Debug, N: Notify + Unpin> fmt::Debug for RecvRefInner<'_, T, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.slot.fmt(f)
    }
}

impl<T: fmt::Display, N: Notify + Unpin> fmt::Display for RecvRefInner<'_, T, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.slot.fmt(f)
    }
}

impl<T: fmt::Write, N: Notify + Unpin> fmt::Write for RecvRefInner<'_, T, N> {
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

impl<N: Notify> Drop for NotifyRx<'_, N> {
    #[inline]
    fn drop(&mut self) {
        test_println!("notifying rx ({})", core::any::type_name::<N>());
        self.0.notify();
    }
}

impl<N: Notify + Unpin> Drop for NotifyTx<'_, N> {
    #[inline]
    fn drop(&mut self) {
        test_println!("notifying tx ({})", core::any::type_name::<N>());
        self.0.notify();
    }
}

macro_rules! impl_ref_inner {
    ($(#[$m:meta])*, $inner:ident, $name:ident, $notify:ty) => {
        $(#[$m])*
        pub struct $name<'a, T>($inner<'a, T, $notify>);

        impl<T> core::ops::Deref for $name<'_, T> {
            type Target = T;

            #[inline]
            fn deref(&self) -> &Self::Target {
                self.0.deref()
            }
        }

        impl<T> core::ops::DerefMut for $name<'_, T> {
            #[inline]
            fn deref_mut(&mut self) -> &mut Self::Target {
                self.0.deref_mut()
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

macro_rules! impl_send_ref {
    ($(#[$m:meta])* pub struct $name:ident<$notify:ty>;) => {
        impl_ref_inner!($(#[$m])*, SendRefInner, $name, $notify);
    };
}

macro_rules! impl_recv_ref {
    ($(#[$m:meta])* pub struct $name:ident<$notify:ty>;) => {
        impl_ref_inner!($(#[$m])*, RecvRefInner, $name, $notify);
    };
}

mod async_impl;
pub use self::async_impl::*;

feature! {
    #![feature = "std"]
    pub mod blocking;
}

#[cfg(all(loom, test))]
mod tests;
