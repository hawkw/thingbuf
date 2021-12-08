use crate::{
    loom::{
        alloc::Track,
        cell::UnsafeCell,
        sync::atomic::{AtomicPtr, AtomicUsize, Ordering::*},
    },
    util::{
        wait::{Notify, WaitResult},
        Backoff, CachePadded,
    },
};

use alloc::boxed::Box;
use core::ptr;

#[derive(Debug)]
pub(crate) struct WaitQueue<T: Notify> {
    state: CachePadded<AtomicUsize>,
    head: CachePadded<AtomicPtr<Waiter<T>>>,
    /// The tail of the wait queue. This is *only* accessed by the popping thread.
    tail: UnsafeCell<*mut Waiter<T>>,
    stub: *mut Waiter<T>,
}

type Waiter<T> = Track<Node<T>>;

#[derive(Debug)]
struct Node<T> {
    next: AtomicPtr<Waiter<T>>,
    value: Option<T>,
}

#[derive(Debug)]
enum PopError {
    Empty,
    Inconsistent,
}

impl<T: Notify> WaitQueue<T> {
    pub(crate) fn push_waiter(&self, mk_waiter: impl FnOnce() -> T) -> WaitResult {
        if test_dbg!(self.state.load(Acquire) & Self::CLOSED != 0) {
            return WaitResult::TxClosed;
        }

        self.push(mk_waiter());
        WaitResult::Wait
    }

    pub(crate) fn notify(&self) -> bool {
        let mut backoff = Backoff::new();
        loop {
            match test_dbg!(unsafe { self.pop() }) {
                Ok(waiter) => {
                    waiter.notify();
                    test_println!("notified");
                    return true;
                }
                Err(PopError::Empty) => return false,
                Err(PopError::Inconsistent) => backoff.spin_yield(),
            }
        }
    }

    pub(crate) fn close(&self) {
        self.state.fetch_or(Self::CLOSED, Release);
        while self.notify() {}
    }

    const CLOSED: usize = 1;

    pub(crate) fn new() -> Self {
        let stub = unsafe { Node::new(None) };
        Self {
            state: CachePadded(AtomicUsize::new(0)),
            head: CachePadded(AtomicPtr::new(stub)),
            tail: UnsafeCell::new(stub),
            stub,
        }
    }

    fn push(&self, waiter: T) {
        unsafe {
            let node = test_dbg!(Node::new(Some(waiter)));
            self.push_node(node);
        }
    }

    unsafe fn push_node(&self, node: *mut Waiter<T>) {
        let prev = test_dbg!(self.head.swap(node, AcqRel));
        test_dbg!((*prev).get_ref().next.store(node, Release));
    }

    unsafe fn pop(&self) -> Result<T, PopError> {
        self.tail.with_mut(|tail_cell| unsafe {
            let mut tail = test_dbg!(*tail_cell);
            let mut next = test_dbg!((*tail).get_ref().next.load(Acquire));

            if test_dbg!(tail == self.stub) {
                if test_dbg!(next.is_null()) {
                    return Err(PopError::Empty);
                }

                (*tail_cell) = next;
                tail = next;
                next = test_dbg!((*next).get_ref().next.load(Acquire));
            }

            if test_dbg!(!next.is_null()) {
                (*tail_cell) = next;
                assert!((*tail).get_ref().value.is_none());

                // Take the waker/thread out of the node.
                let value = (*tail)
                    .get_mut()
                    .value
                    .take()
                    .expect("popped node must have a value");

                drop(Box::from_raw(tail));
                return Ok(value);
            }

            let head = test_dbg!(self.head.load(Acquire));
            if test_dbg!(head != tail) {
                // The queue is inconsistent! The popping thread will need to
                // try again.
                return Err(PopError::Inconsistent);
            }

            self.push_node(self.stub);

            next = test_dbg!((*tail).get_ref().next.load(Acquire));
            if test_dbg!(!next.is_null()) {
                (*tail_cell) = next;
                // assert!((*tail).get_ref().value.is_none());

                // Take the waker/thread out of the node.
                let value = (*tail)
                    .get_mut()
                    .value
                    .take()
                    .expect("popped node must have a value");

                drop(Box::from_raw(tail));
                return Ok(value);
            }

            Err(PopError::Empty)
        })
    }
}

unsafe impl<T: Notify + Send> Send for WaitQueue<T> {}
unsafe impl<T: Notify + Send> Sync for WaitQueue<T> {}

// === impl Node ===

impl<T> Node<T> {
    unsafe fn new(value: Option<T>) -> *mut Waiter<T> {
        let node = Box::new(Track::new(Node {
            next: AtomicPtr::new(ptr::null_mut()),
            value,
        }));
        Box::into_raw(node)
    }
}

impl<T: Notify> Drop for WaitQueue<T> {
    fn drop(&mut self) {
        test_println!("dropping wait queue");
        self.state.fetch_or(Self::CLOSED, Release);
        self.tail.with_mut(|tail| unsafe {
            let mut cur = *tail;
            while !cur.is_null() {
                let next = (*cur).get_ref().next.load(Relaxed);
                if let Some(waiter) = (*cur).get_mut().value.take() {
                    waiter.notify();
                }
                drop(Box::from_raw(cur));
                cur = next;
            }
        });
    }
}
