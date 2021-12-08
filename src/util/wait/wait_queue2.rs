use crate::{
    loom::atomic::{AtomicBool, AtomicUsize, Ordering::*},
    util::{
        mutex::Mutex,
        wait::{Notify, WaitResult},
        CachePadded,
    },
};

use core::{fmt, marker::PhantomPinned, pin::Pin, ptr::NonNull};

#[derive(Debug)]
pub(crate) struct WaitQueue<T> {
    state: CachePadded<AtomicUsize>,
    list: Mutex<List<T>>,
}

#[pin_project::pin_project]
#[derive(Debug)]
pub(crate) struct Waiter<T> {
    next: Option<NonNull<Self>>,
    prev: Option<NonNull<Self>>,
    waiter: Option<T>,

    // This type is !Unpin due to the heuristic from:
    // <https://github.com/rust-lang/rust/pull/82834>
    #[pin]
    _pin: PhantomPinned,
}

struct List<T> {
    head: Option<NonNull<Waiter<T>>>,
    tail: Option<NonNull<Waiter<T>>>,
}

const CLOSED: usize = 1 << 0;
const ONE_QUEUED: usize = 1 << 1;

#[derive(Copy, Clone)]
struct State(usize);

impl<T: Notify + Unpin> WaitQueue<T> {
    pub(crate) fn new() -> Self {
        Self {
            state: CachePadded(AtomicUsize::new(0)),
            list: Mutex::new(List::new()),
        }
    }

    pub(crate) fn push_waiter(
        &self,
        node: &mut Option<Pin<&mut Waiter<T>>>,
        mk_waiter: impl FnOnce() -> T,
    ) -> WaitResult {
        let mut state = test_dbg!(self.state.load(Acquire));
        if test_dbg!(state & CLOSED != 0) {
            return WaitResult::TxClosed;
        }

        if test_dbg!(node.is_some()) {
            while test_dbg!(state > CLOSED) {
                match test_dbg!(self.state.compare_exchange_weak(
                    state,
                    state.saturating_sub(ONE_QUEUED),
                    AcqRel,
                    Acquire
                )) {
                    Ok(_) => return WaitResult::Notified,
                    Err(actual) => state = test_dbg!(actual),
                }
            }

            if test_dbg!(state & CLOSED != 0) {
                return WaitResult::TxClosed;
            }

            let mut list = self.list.lock();
            let state = test_dbg!(self.state.load(Acquire));
            if test_dbg!(state >= ONE_QUEUED) {
                self.state
                    .compare_exchange(state, state.saturating_sub(ONE_QUEUED), AcqRel, Acquire)
                    .expect("should succeed");
                return WaitResult::Notified;
            }

            if let Some(mut node) = node.take() {
                *node.as_mut().project().waiter = Some(mk_waiter());
                test_println!("-> push {:?}", node);
                list.push_front(node);
            } else {
                unreachable!("this could be unchecked...")
            }
        }

        WaitResult::Wait
    }

    pub(crate) fn notify(&self) -> bool {
        if let Some(node) = test_dbg!(self.list.lock().pop_back()) {
            node.notify();
            true
        } else {
            self.state.fetch_add(ONE_QUEUED, Release);
            false
        }
    }

    pub(crate) fn close(&self) {
        test_dbg!(self.state.fetch_or(CLOSED, Release));
        let mut list = self.list.lock();
        while let Some(node) = list.pop_back() {
            node.notify();
        }
    }
}

// === impl Waiter ===

impl<T: Notify> Waiter<T> {
    pub(crate) fn new() -> Self {
        Self {
            next: None,
            prev: None,
            waiter: None,
            _pin: PhantomPinned,
        }
    }

    pub(crate) fn register(&mut self, mk_waiter: impl FnOnce() -> T) {
        if self.waiter.is_none() {
            self.waiter = Some(mk_waiter())
        }
    }

    pub(crate) fn register_pinned(self: Pin<&mut Self>, mk_waiter: impl FnOnce() -> T) {
        let this = self.project();
        if this.waiter.is_none() {
            *this.waiter = Some(mk_waiter())
        }
    }

    fn notify(self: Pin<&mut Self>) -> bool {
        if let Some(waker) = self.project().waiter.take() {
            waker.notify();
            true
        } else {
            false
        }
    }
}

impl<T: Notify> Waiter<T> {
    pub(crate) fn remove(self: Pin<&mut Self>, q: &WaitQueue<T>) {
        test_println!("removing {:?}", self);
        let mut list = q.list.lock();
        unsafe {
            list.remove(self);
        }
    }
}

unsafe impl<T: Send> Send for Waiter<T> {}
unsafe impl<T: Send> Sync for Waiter<T> {}

// === impl List ===

impl<T> List<T> {
    fn new() -> Self {
        Self {
            head: None,
            tail: None,
        }
    }

    fn is_empty(&self) -> bool {
        debug_assert_eq!(self.head.is_none(), self.tail.is_none());
        test_dbg!(self.head.is_none())
    }

    fn push_front(&mut self, node: Pin<&mut Waiter<T>>) {
        let mut node = unsafe { node.get_unchecked_mut() };
        node.next = self.head;
        node.prev = None;

        let ptr = NonNull::from(node);

        assert_ne!(self.head, Some(ptr), "tried to push the same waiter twice!");
        if let Some(mut head) = self.head {
            unsafe {
                head.as_mut().prev = Some(ptr);
            }
        }

        self.head = Some(ptr);
        if self.tail.is_none() {
            self.tail = Some(ptr);
        }
    }

    fn pop_back(&mut self) -> Option<Pin<&mut Waiter<T>>> {
        let last = unsafe { self.tail?.as_mut() };

        let prev = last.prev.take();
        if let Some(mut prev) = prev {
            unsafe {
                prev.as_mut().next = None;
            }
        } else {
            self.head = None;
        }
        self.tail = prev;
        last.next = None;
        unsafe { Some(Pin::new_unchecked(last)) }
    }

    unsafe fn remove(&mut self, node: Pin<&mut Waiter<T>>) {
        let node_ref = node.get_unchecked_mut();
        let prev = node_ref.prev.take();
        let next = node_ref.next.take();
        let ptr = NonNull::from(node_ref);

        if let Some(mut prev) = prev {
            let prev = prev.as_mut();
            debug_assert_eq!(prev.next, Some(ptr));
            prev.next = next;
        } else if self.head == Some(ptr) {
            self.head = next;
        }

        if let Some(mut next) = next {
            let next = next.as_mut();
            debug_assert_eq!(next.prev, Some(ptr));

            next.prev = prev;
        } else if self.tail == Some(ptr) {
            self.tail = prev;
        };
    }
}

impl<T> fmt::Debug for List<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("List")
            .field("head", &self.head)
            .field("tail", &self.tail)
            .finish()
    }
}

unsafe impl<T: Send> Send for List<T> {}

// === impl State ===

impl State {
    const UNLOCKED: Self = Self(0b00);
    const LOCKED: Self = Self(0b01);
    const EMPTY: Self = Self(0b10);
    const CLOSED: Self = Self(0b100);

    const FLAG_BITS: usize = Self::LOCKED.0 | Self::EMPTY.0 | Self::CLOSED.0;
    const QUEUED_SHIFT: usize = Self::FLAG_BITS.trailing_ones() as usize;
    const QUEUED_ONE: usize = 1 << Self::QUEUED_SHIFT;

    fn queued(self) -> usize {
        self.0 >> Self::QUEUED_SHIFT
    }

    fn add_queued(self) -> Self {
        Self(self.0 + Self::QUEUED_ONE)
    }

    fn contains(self, Self(state): Self) -> bool {
        self.0 & state == state
    }

    fn sub_queued(self) -> Self {
        let flags = self.0 & Self::FLAG_BITS;
        Self(self.0 & (!Self::FLAG_BITS).saturating_sub(Self::QUEUED_ONE) | flags)
    }
}

impl fmt::Debug for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("State(")?;
        let mut has_flags = false;

        fmt_bits!(self, f, has_flags, LOCKED, EMPTY, CLOSED);

        if !has_flags {
            f.write_str("UNLOCKED")?;
        }

        let queued = self.queued();
        if queued > 0 {
            write!(f, ", queued: {})", queued)?;
        } else {
            f.write_str(")")?;
        }

        Ok(())
    }
}
