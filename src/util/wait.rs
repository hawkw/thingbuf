use crate::util::panic::UnwindSafe;
use core::{fmt, task::Waker};

mod wait_cell;
pub(crate) use self::wait_cell::WaitCell;

feature! {
    #![feature = "alloc"]
    pub(crate) mod wait_queue;
    pub(crate) use self::wait_queue::WaitQueue;
}
pub(crate) mod wait_queue2;
#[cfg(feature = "std")]
use crate::loom::thread;

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum WaitResult {
    Wait,
    TxClosed,
    Notified,
}

#[derive(Debug)]
pub(crate) struct NotifyOnDrop<T: Notify>(Option<T>);

pub(crate) trait Notify: UnwindSafe + fmt::Debug {
    fn notify(self);
}

#[cfg(feature = "std")]
impl Notify for thread::Thread {
    fn notify(self) {
        test_println!("NOTIFYING {:?} (from {:?})", self, thread::current());
        self.unpark();
    }
}

impl Notify for Waker {
    fn notify(self) {
        test_println!("WAKING TASK {:?} (from {:?})", self, thread::current());
        self.wake();
    }
}

impl<T: Notify> NotifyOnDrop<T> {
    pub(crate) fn new(notify: T) -> Self {
        Self(Some(notify))
    }
}

impl<T: Notify> Notify for NotifyOnDrop<T> {
    fn notify(self) {
        drop(self)
    }
}

impl<T: Notify> Drop for NotifyOnDrop<T> {
    fn drop(&mut self) {
        if let Some(notify) = self.0.take() {
            notify.notify();
        }
    }
}
