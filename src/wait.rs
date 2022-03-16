//! Waiter cells and queues to allow threads/tasks to wait for notifications.
//!
//! The MPSC channel enhance a ThingBuf --- which implements a non-blocking
//! queue --- with the capacity to wait. A `ThingBuf` only has `try_send` and
//! `try_recv`-like operations, which immediately return in the case where the
//! queue is full or empty, respectively. In a MPSC channel, the sender is
//! provided with the ability to *wait* until there is capacity in the queue to
//! send its message. Similarly, a receiver can wait until the channel has
//! messages to receive.
//!
//! This module implements two types of structure for waiting: a wait *cell*,
//! which stores a *single* waiting thread or task, and a wait *queue*, which
//! stores a queue of waiting tasks. Since the channel is a MPSC (multiple
//! producer, single consumer) channel, the wait queue is used to store waiting
//! senders, while the wait cell is used to store a waiting receiver (as there
//! is only ever one thread/task waiting to receive from a channel).
//!
//! This module is generic over the _type_ of the waiter; they may either be
//! [`core::task::Waker`]s, for the async MPSC, or [`std::thread::Thread`]s, for
//! the blocking MPSC. In either case, the role played by these types is fairly
//! analogous.
use core::{fmt, task::Waker};

mod cell;
pub(crate) mod queue;
pub(crate) use self::{cell::WaitCell, queue::WaitQueue};

#[cfg(feature = "std")]
use crate::loom::thread;

/// What happened while trying to register to wait.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum WaitResult {
    /// The waiter was registered, and the calling thread/task can now wait.
    Wait,
    /// The channel is closed.
    ///
    /// When registering a sender, this means the receiver was dropped; when
    /// registering a receiver, this means that all senders have been dropped.
    /// In this case, the waiting thread/task should *not* wait, because it will
    /// never be woken back up.
    ///
    /// If this is returned, the waiter (`Thread`/`Waker`) was *not* registered.
    Closed,
    /// We were notified while trying to register a waiter.
    ///
    /// This means that, while we were trying to access the wait cell or wait
    /// queue, the other side of the channel sent a notification. In this case,
    /// we don't need to wait, and we can try the operation we were attempting
    /// again, as it may now be ready.
    ///
    /// If this is returned, the waiter (`Thread`/`Waker`) was *not* registered.
    Notified,
}

pub(crate) trait Notify: fmt::Debug + Clone {
    fn notify(self);

    fn same(&self, other: &Self) -> bool;
}

#[cfg(feature = "std")]
impl Notify for thread::Thread {
    #[inline]
    fn notify(self) {
        test_println!("NOTIFYING {:?} (from {:?})", self, thread::current());
        self.unpark();
    }

    #[inline]
    fn same(&self, other: &Self) -> bool {
        other.id() == self.id()
    }
}

impl Notify for Waker {
    #[inline]
    fn notify(self) {
        test_println!("WAKING TASK {:?} (from {:?})", self, thread::current());
        self.wake();
    }

    #[inline]
    fn same(&self, other: &Self) -> bool {
        other.will_wake(self)
    }
}
