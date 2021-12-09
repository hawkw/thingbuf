use crate::util::panic::UnwindSafe;
use core::{fmt, task::Waker};

mod cell;
pub(crate) mod queue;
pub(crate) use self::{cell::WaitCell, queue::WaitQueue};

#[cfg(feature = "std")]
use crate::loom::thread;

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum WaitResult {
    Wait,
    TxClosed,
    Notified,
}

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
