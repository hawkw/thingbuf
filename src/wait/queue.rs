use crate::{
    loom::{
        atomic::{AtomicUsize, Ordering::*},
        cell::UnsafeCell,
    },
    util::{mutex::Mutex, CachePadded},
    wait::{Notify, WaitResult},
};

use core::{fmt, marker::PhantomPinned, pin::Pin, ptr::NonNull};

/// A queue of waiters ([`core::task::Waker`]s or [`std::thread::Thread`]s)
/// implemented as a doubly-linked intrusive list.
///
/// The *[intrusive]* aspect of this list is important, as it means that it does
/// not allocate memory. Instead, nodes in the linked list are stored in the
/// futures of tasks trying to wait for capacity, or on the stack frames of
/// threads blocking on channel capacity.
///
/// Using an intrusive list is critical if the MPSC is to *truly* have bounded
/// memory use: if the channel has capacity for a bounded number of *messages*,
/// but when it is full, must allocate memory to store the threads or tasks that
/// are waiting to send messages, it does not actually bound the maximum amount
/// of memory used by the channel! This is, potentially, quite bad, as
/// increasing backpressure can cause out of memory conditions.
///
/// Also, not allocating can have a performance advantage, since tasks waiting
/// for capacity will never hit `malloc` or `free`. This reduces the overhead of
/// waiting, but (more importantly!) it also avoids allocator churn that can
/// hurt `malloc` performance for *other* parts of the program that need to
/// allocate memory.
///
/// Finally, this will allow using a `ThingBuf` MPSC channel with exclusively
/// static allocations, making it much easier to use in embedded systems or on
/// bare metal when an allocator may not always be available.
///
/// However, the intrusive linked list introduces one new danger: because
/// futures can be *cancelled*, and the linked list nodes live within the
/// futures trying to wait for channel capacity, we *must* ensure that the node
/// is unlinked from the list before dropping a cancelled future. Failure to do
/// so would result in the list containing dangling pointers. Therefore, we must
/// use a *doubly-linked* list, so that nodes can edit both the previous and
/// next node when they have to remove themselves. This is kind of a bummer, as
/// it means we can't use something nice like this [intrusive queue by Dmitry
/// Vyukov][2], and there are not really practical designs for lock-free
/// doubly-linked lists that don't rely on some kind of deferred reclamation
/// scheme such as hazard pointers or QSBR.
///
/// Instead, we just stick a mutex around the linked list, which must be
/// acquired to pop nodes from it, or for nodes to remove themselves when
/// futures are cancelled. This is a bit sad, but the critical sections for this
/// mutex are short enough that we still get pretty good performance despite it.
///
/// A spinlock is used on `no_std` platforms; [`std::sync::Mutex`] is used when
/// the standard library is available.
///
/// [intrusive]: https://fuchsia.dev/fuchsia-src/development/languages/c-cpp/fbl_containers_guide/introduction
/// [2]: https://www.1024cores.net/home/lock-free-algorithms/queues/intrusive-mpsc-node-based-queue
#[derive(Debug)]
pub(crate) struct WaitQueue<T> {
    /// The wait queue's state variable.
    ///
    /// The queue is always in one of the following states:
    ///
    /// - [`EMPTY`]: No waiters are queued, and there is no pending notification.
    ///   Waiting while the queue is in this state will enqueue the waiter;
    ///   notifying while in this state will store a pending notification in the
    ///   queue, transitioning to the `WAKING` state.
    ///
    /// - [`WAITING`]: There are one or more waiters in the queue. Waiting while
    ///   the queue is in this state will not transition the state. Waking while
    ///   in this state will wake the first waiter in the queue; if this empties
    ///   the queue, then the queue will transition to the `EMPTY` state.
    ///
    /// - [`WAKING`]: The queue has a stored notification. Waiting while the queue
    ///   is in this state will consume the pending notification *without*
    ///   enqueueing the waiter and transition the queue to the `EMPTY` state.
    ///   Waking while in this state will leave the queue in this state.
    ///
    /// - [`CLOSED`]: The queue is closed. Waiting while in this state will return
    ///   [`WaitResult::Closed`] without transitioning the queue's state.
    state: CachePadded<AtomicUsize>,

    /// The linked list of waiters.
    ///
    /// # Safety
    ///
    /// This is protected by a mutex; the mutex *must* be acquired when
    /// manipulating the linked list, OR when manipulating waiter nodes that may
    /// be linked into the list. If a node is known to not be linked, it is safe
    /// to modify that node (such as by setting or unsetting its
    /// `Waker`/`Thread`) without holding the lock; otherwise, it may be
    /// modified through the list, so the lock must be held when modifying the
    /// node.
    ///
    /// A spinlock is used on `no_std` platforms; [`std::sync::Mutex`] or
    /// `parking_lot::Mutex` are used when the standard library is available
    /// (depending on feature flags).
    list: Mutex<List<T>>,
}

/// A waiter node which may be linked into a wait queue.
#[derive(Debug)]
pub(crate) struct Waiter<T> {
    /// The waiter's state variable.
    ///
    /// A waiter is always in one of the following states:
    ///
    /// - [`EMPTY`]: The waiter is not linked in the queue, and does not have a
    ///   `Thread`/`Waker`.
    ///
    /// - [`WAITING`]: The waiter is linked in the queue and has a
    ///   `Thread`/`Waker`.
    ///
    /// - [`WAKING`]: The waiter has been notified by the wait queue. If it is in
    ///   this state, it is *not* linked into the queue, and does not have a
    ///   `Thread`/`Waker`.
    ///
    /// - [`WAKING`]: The waiter has been notified because the wait queue closed.
    ///   If it is in this state, it is *not* linked into the queue, and does
    ///   not have a `Thread`/`Waker`.
    ///
    /// This may be inspected without holding the lock; it can be used to
    /// determine whether the lock must be acquired.
    state: CachePadded<AtomicUsize>,

    /// The linked list node and stored `Thread`/`Waker`.
    ///
    /// # Safety
    ///
    /// This `UnsafeCell` may only be accessed while holding the `Mutex` around
    /// the wait queue's linked list!
    node: UnsafeCell<Node<T>>,
}

#[derive(Debug)]
#[pin_project::pin_project]
struct Node<T> {
    next: Link<Waiter<T>>,
    prev: Link<Waiter<T>>,
    waiter: Option<T>,

    // This type is !Unpin due to the heuristic from:
    // <https://github.com/rust-lang/rust/pull/82834>
    #[pin]
    _pin: PhantomPinned,
}

type Link<T> = Option<NonNull<T>>;

struct List<T> {
    head: Link<Waiter<T>>,
    tail: Link<Waiter<T>>,
}

const EMPTY: usize = 0;
const WAITING: usize = 1;
const WAKING: usize = 2;
const CLOSED: usize = 3;

impl<T> WaitQueue<T> {
    #[cfg(loom)]
    pub(crate) fn new() -> Self {
        Self {
            state: CachePadded(AtomicUsize::new(EMPTY)),
            list: Mutex::new(List::new()),
        }
    }

    #[cfg(not(loom))]
    pub(crate) const fn new() -> Self {
        Self {
            state: CachePadded(AtomicUsize::new(EMPTY)),
            list: crate::util::mutex::const_mutex(List::new()),
        }
    }
}

impl<T: Notify + Unpin> WaitQueue<T> {
    /// Start waiting for a notification.
    ///
    /// If the queue has a stored notification, this consumes it and returns
    /// [`WaitResult::Notified`] without adding the waiter to the queue. If the
    /// queue is closed, this returns [`WaitResult::Closed`] without adding the
    /// waiter to the queue. Otherwise, the waiter is enqueued, and this returns
    /// [`WaitResult::Wait`].
    #[inline(always)]
    pub(crate) fn start_wait(&self, node: Pin<&mut Waiter<T>>, waiter: &T) -> WaitResult {
        test_println!("WaitQueue::start_wait({:p})", node);

        // Optimistically, acquire a stored notification before trying to lock
        // the wait list.
        match test_dbg!(self.state.compare_exchange(WAKING, EMPTY, SeqCst, SeqCst)) {
            Ok(_) => return WaitResult::Notified,
            Err(CLOSED) => return WaitResult::Closed,
            Err(_) => {}
        }

        // Slow path: the queue is not closed, and we failed to consume a stored
        // notification. We need to acquire the lock and enqueue the waiter.
        self.start_wait_slow(node, waiter)
    }

    /// Slow path of `start_wait`: acquires the linked list lock, and adds the
    /// waiter to the queue.
    #[cold]
    #[inline(never)]
    fn start_wait_slow(&self, node: Pin<&mut Waiter<T>>, waiter: &T) -> WaitResult {
        test_println!("WaitQueue::start_wait_slow({:p})", node);
        // There are no queued notifications to consume, and the queue is
        // still open. Therefore, it's time to actually push the waiter to
        // the queue...finally lol :)

        // Grab the lock...
        let mut list = self.list.lock();
        // Reload the queue's state, as it may have changed while we were
        // waiting to lock the linked list.
        let mut state = self.state.load(Acquire);

        loop {
            match test_dbg!(state) {
                // The queue is empty: transition the state to WAITING, as we
                // are adding a waiter.
                EMPTY => {
                    match test_dbg!(self
                        .state
                        .compare_exchange_weak(EMPTY, WAITING, SeqCst, SeqCst))
                    {
                        Ok(_) => break,
                        Err(actual) => {
                            debug_assert!(actual == EMPTY || actual == WAKING || actual == CLOSED);
                            state = actual;
                        }
                    }
                }

                // The queue was woken while we were waiting to acquire the
                // lock. Attempt to consume the wakeup.
                WAKING => {
                    match test_dbg!(self
                        .state
                        .compare_exchange_weak(WAKING, EMPTY, SeqCst, SeqCst))
                    {
                        // Consumed the wakeup!
                        Ok(_) => return WaitResult::Notified,
                        Err(actual) => {
                            debug_assert!(actual == WAKING || actual == EMPTY || actual == CLOSED);
                            state = actual;
                        }
                    }
                }

                // The queue closed while we were waiting to acquire the lock;
                // we're done here!
                CLOSED => return WaitResult::Closed,

                // The queue is already in the WAITING state, so we don't need
                // to mess with it.
                _state => {
                    debug_assert_eq!(_state, WAITING,
                        "start_wait_slow: unexpected state value {:?} (expected WAITING). this is a bug!",
                        _state,
                    );
                    break;
                }
            }
        }

        // Time to wait! Store the waiter in the node, advance the node's state
        // to Waiting, and add it to the queue.

        node.with_node(&mut *list, |node| {
            let _prev = node.waiter.replace(waiter.clone());
            debug_assert!(
                _prev.is_none(),
                "start_wait_slow: called with a node that already had a waiter!"
            );
        });

        let _prev_state = test_dbg!(node.state.swap(WAITING, Release));
        debug_assert!(
            _prev_state == EMPTY || _prev_state == WAKING,
            "start_wait_slow: called with a node that was not empty ({}) or woken ({})! actual={}",
            EMPTY,
            WAKING,
            _prev_state,
        );
        list.enqueue(node);

        WaitResult::Wait
    }

    /// Continue waiting for a notification.
    ///
    /// This is called when a waiter has been woken. It determines if the
    /// node was woken from the queue, or if the wakeup was spurious. If the
    /// wakeup was from the queue, this returns [`WaitResult::Notified`] or
    /// [`WaitResult::Closed`]. Otherwise, if the wakeup was spurious, this will
    /// lock the queue and check if the node's waiter needs to be updated.
    #[inline(always)]
    pub(crate) fn continue_wait(&self, node: Pin<&mut Waiter<T>>, my_waiter: &T) -> WaitResult {
        test_println!("WaitQueue::continue_wait({:p})", node);

        // Fast path: check if the node was woken from the queue.
        let state = test_dbg!(node.state.load(Acquire));
        match state {
            WAKING => return WaitResult::Notified,
            CLOSED => return WaitResult::Closed,
            _state => {
                debug_assert_eq!(
                    _state, WAITING,
                    "continue_wait should not be called unless the node has been enqueued"
                );
            }
        }

        // Slow path: received a spurious wakeup. We must lock the queue so that
        // we can potentially modify the node's waiter.
        self.continue_wait_slow(node, my_waiter)
    }

    /// Slow path for `continue_wait`: locks the linked list and updates the
    /// node with a new waiter.
    #[cold]
    #[inline(never)]
    fn continue_wait_slow(&self, node: Pin<&mut Waiter<T>>, my_waiter: &T) -> WaitResult {
        test_println!("WaitQueue::continue_wait_slow({:p})", node);

        // If the waiting task/thread was woken but no wakeup was assigned to
        // the node, we may need to update the node with a new waiter.
        // Therefore, lock the queue in order to modify the node.
        let mut list = self.list.lock();

        // The node may have been woken while we were waiting to acquire the
        // lock. If so, check the new state.
        match test_dbg!(node.state.load(Acquire)) {
            WAKING => return WaitResult::Notified,
            CLOSED => return WaitResult::Closed,
            _state => {
                debug_assert_eq!(
                    _state, WAITING,
                    "continue_wait_slow should not be called unless the node has been enqueued"
                );
            }
        }

        // Okay, we were not woken and need to continue waiting. It may be
        // necessary to update the waiter with a new waiter (in practice, this
        // is only necessary in async).
        node.with_node(&mut *list, |node| {
            if let Some(ref mut waiter) = node.waiter {
                if !waiter.same(my_waiter) {
                    *waiter = my_waiter.clone();
                }
            } else {
                // XXX(eliza): This branch should _probably_ never occur...
                node.waiter = Some(my_waiter.clone());
            }
        });

        WaitResult::Wait
    }

    /// Notify one waiter from the queue. If there are no waiters in the linked
    /// list, the notification is instead assigned to the queue itself.
    ///
    /// If a waiter was popped from the queue, returns `true`. Otherwise, if the
    /// notification was assigned to the queue, returns `false`.
    #[inline(always)]
    pub(crate) fn notify(&self) -> bool {
        test_println!("WaitQueue::notify()");

        // Fast path: If the queue is empty, we can simply assign the
        // notification to the queue.
        let mut state = self.state.load(Acquire);

        while test_dbg!(state) == WAKING || state == EMPTY {
            match test_dbg!(self
                .state
                .compare_exchange_weak(state, WAKING, SeqCst, SeqCst))
            {
                // No waiters are currently waiting, assign the notification to
                // the queue to be consumed by the next wait attempt.
                Ok(_) => return false,
                Err(actual) => state = actual,
            }
        }

        // Slow path: there are waiters in the queue, so we must acquire the
        // lock and wake one of them.
        self.notify_slow(state)
    }

    /// Slow path for `notify`: acquire the lock on the linked list, dequeue a
    /// waiter, and notify it.
    #[cold]
    #[inline(never)]
    fn notify_slow(&self, state: usize) -> bool {
        test_println!("WaitQueue::notify_slow(state: {})", state);

        let mut list = self.list.lock();
        match state {
            EMPTY | WAKING => {
                if let Err(actual) = self.state.compare_exchange(state, WAKING, SeqCst, SeqCst) {
                    debug_assert!(actual == EMPTY || actual == WAKING);
                    self.state.store(WAKING, SeqCst);
                }
            }
            WAITING => {
                let waiter = list.dequeue(WAKING);
                debug_assert!(
                    waiter.is_some(),
                    "if we were in the `WAITING` state, there must be a waiter in the queue!\nself={:#?}",
                    self,
                );

                // If we popped the last node, transition back to the empty
                // state.
                if test_dbg!(list.is_empty()) {
                    self.state.store(EMPTY, SeqCst);
                }

                // drop the lock
                drop(list);

                // wake the waiter
                if let Some(waiter) = waiter {
                    waiter.notify();
                    return true;
                }
            }
            _weird => {
                // huh, if `notify_slow` was called, we *probably* shouldn't be
                // in the `closed` state...
                #[cfg(debug_assertions)]
                unreachable!("notify_slow: unexpected state value {:?}", _weird);
            }
        }
        false
    }

    /// Close the queue, notifying all waiting tasks.
    pub(crate) fn close(&self) {
        test_println!("WaitQueue::close()");

        test_dbg!(self.state.swap(CLOSED, SeqCst));
        let mut list = self.list.lock();
        while !list.is_empty() {
            if let Some(waiter) = list.dequeue(CLOSED) {
                waiter.notify();
            }
        }
    }
}

// === impl Waiter ===

impl<T: Notify> Waiter<T> {
    pub(crate) fn new() -> Self {
        Self {
            state: CachePadded(AtomicUsize::new(EMPTY)),
            node: UnsafeCell::new(Node {
                next: None,
                prev: None,
                waiter: None,
                _pin: PhantomPinned,
            }),
        }
    }

    #[inline(never)]
    pub(crate) fn remove(self: Pin<&mut Self>, q: &WaitQueue<T>) {
        test_println!("Waiter::remove({:p})", self);
        let mut list = q.list.lock();
        unsafe {
            // Safety: removing a node is unsafe even when the list is locked,
            // because there's no way to guarantee that the node is part of
            // *this* list. However, the potential callers of this method will
            // never have access to any other linked lists, so we can just kind
            // of assume that this is safe.
            list.remove(self);
        }
        if test_dbg!(list.is_empty()) {
            let _ = test_dbg!(q.state.compare_exchange(WAITING, EMPTY, SeqCst, SeqCst));
        }
    }

    #[inline]
    pub(crate) fn is_linked(&self) -> bool {
        test_dbg!(self.state.load(Acquire)) == WAITING
    }
}

impl<T> Waiter<T> {
    /// # Safety
    ///
    /// This is only safe to call while the list is locked. The dummy `_list`
    /// parameter ensures this method is only called while holding the lock, so
    /// this can be safe.
    #[inline(always)]
    #[cfg_attr(loom, track_caller)]
    fn with_node<U>(&self, _list: &mut List<T>, f: impl FnOnce(&mut Node<T>) -> U) -> U {
        self.node.with_mut(|node| unsafe {
            // Safety: the dummy `_list` argument ensures that the caller has
            // the right to mutate the list (e.g. the list is locked).
            f(&mut *node)
        })
    }

    /// # Safety
    ///
    /// This is only safe to call while the list is locked.
    #[cfg_attr(loom, track_caller)]
    unsafe fn set_prev(&mut self, prev: Option<NonNull<Waiter<T>>>) {
        self.node.with_mut(|node| (*node).prev = prev);
    }

    /// # Safety
    ///
    /// This is only safe to call while the list is locked.
    #[cfg_attr(loom, track_caller)]
    unsafe fn take_prev(&mut self) -> Option<NonNull<Waiter<T>>> {
        self.node.with_mut(|node| (*node).prev.take())
    }

    /// # Safety
    ///
    /// This is only safe to call while the list is locked.
    #[cfg_attr(loom, track_caller)]
    unsafe fn take_next(&mut self) -> Option<NonNull<Waiter<T>>> {
        self.node.with_mut(|node| (*node).next.take())
    }
}

unsafe impl<T: Send> Send for Waiter<T> {}
unsafe impl<T: Send> Sync for Waiter<T> {}

// === impl List ===

impl<T> List<T> {
    const fn new() -> Self {
        Self {
            head: None,
            tail: None,
        }
    }

    fn enqueue(&mut self, waiter: Pin<&mut Waiter<T>>) {
        test_println!("List::enqueue({:p})", waiter);

        let node = unsafe { waiter.get_unchecked_mut() };
        let head = self.head.take();
        node.with_node(self, |node| {
            node.next = head;
            node.prev = None;
        });

        let ptr = NonNull::from(node);
        debug_assert_ne!(
            self.head,
            Some(ptr),
            "tried to enqueue the same waiter twice!"
        );
        if let Some(mut head) = head {
            unsafe {
                head.as_mut().set_prev(Some(ptr));
            }
        }
        self.head = Some(ptr);

        if self.tail.is_none() {
            self.tail = Some(ptr);
        }
    }

    fn dequeue(&mut self, new_state: usize) -> Option<T> {
        let mut last = self.tail?;
        test_println!("List::dequeue({:?}) -> {:p}", new_state, last);

        let last = unsafe { last.as_mut() };
        let _prev_state = test_dbg!(last.state.swap(new_state, Release));
        debug_assert_eq!(_prev_state, WAITING);

        let (prev, waiter) = last.with_node(self, |node| {
            node.next = None;
            (node.prev.take(), node.waiter.take())
        });

        match prev {
            Some(mut prev) => unsafe {
                let _ = prev.as_mut().take_next();
            },
            None => self.head = None,
        }

        self.tail = prev;

        waiter
    }

    unsafe fn remove(&mut self, node: Pin<&mut Waiter<T>>) {
        test_println!("List::remove({:p})", node);

        let node_ref = node.get_unchecked_mut();
        let prev = node_ref.take_prev();
        let next = node_ref.take_next();
        let ptr = NonNull::from(node_ref);

        if let Some(mut prev) = prev {
            prev.as_mut().with_node(self, |prev| {
                debug_assert_eq!(prev.next, Some(ptr));
                prev.next = next;
            });
        } else if self.head == Some(ptr) {
            self.head = next;
        }

        if let Some(mut next) = next {
            next.as_mut().with_node(self, |next| {
                debug_assert_eq!(next.prev, Some(ptr));
                next.prev = prev;
            });
        } else if self.tail == Some(ptr) {
            self.tail = prev;
        }
    }

    fn is_empty(&self) -> bool {
        self.head.is_none() && self.tail.is_none()
    }
}

impl<T> fmt::Debug for List<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("List")
            .field("head", &self.head)
            .field("tail", &self.tail)
            .field("is_empty", &self.is_empty())
            .finish()
    }
}

unsafe impl<T: Send> Send for List<T> {}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    #[derive(Debug, Clone)]
    struct MockNotify(Arc<AtomicBool>);

    impl Notify for MockNotify {
        fn notify(self) {
            self.0.store(true, Ordering::SeqCst);
        }

        fn same(&self, Self(other): &Self) -> bool {
            Arc::ptr_eq(&self.0, other)
        }
    }

    impl MockNotify {
        fn new() -> Self {
            Self(Arc::new(AtomicBool::new(false)))
        }

        fn was_notified(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
    }

    #[test]
    fn notify_one() {
        let q = WaitQueue::new();

        let notify1 = MockNotify::new();
        let notify2 = MockNotify::new();

        let mut waiter1 = Box::pin(Waiter::new());
        let mut waiter2 = Box::pin(Waiter::new());

        assert_eq_dbg!(q.start_wait(waiter1.as_mut(), &notify1), WaitResult::Wait);
        assert_dbg!(waiter1.is_linked());

        assert_eq_dbg!(q.start_wait(waiter2.as_mut(), &notify2), WaitResult::Wait);
        assert_dbg!(waiter2.is_linked());

        assert_dbg!(!notify1.was_notified());
        assert_dbg!(!notify2.was_notified());

        assert_dbg!(q.notify());

        assert_dbg!(notify1.was_notified());
        assert_dbg!(!waiter1.is_linked());

        assert_dbg!(!notify2.was_notified());
        assert_dbg!(waiter2.is_linked());

        assert_eq_dbg!(
            q.continue_wait(waiter2.as_mut(), &notify2),
            WaitResult::Wait
        );

        assert_eq_dbg!(
            q.continue_wait(waiter1.as_mut(), &notify1),
            WaitResult::Notified
        );
    }

    #[test]
    fn close() {
        let q = WaitQueue::new();

        let notify1 = MockNotify::new();
        let notify2 = MockNotify::new();

        let mut waiter1 = Box::pin(Waiter::new());
        let mut waiter2 = Box::pin(Waiter::new());

        assert_eq_dbg!(q.start_wait(waiter1.as_mut(), &notify1), WaitResult::Wait);
        assert_dbg!(waiter1.is_linked());

        assert_eq_dbg!(q.start_wait(waiter2.as_mut(), &notify2), WaitResult::Wait);
        assert_dbg!(waiter2.is_linked());

        assert_dbg!(!notify1.was_notified());
        assert_dbg!(!notify2.was_notified());

        q.close();

        assert_dbg!(notify1.was_notified());
        assert_dbg!(!waiter1.is_linked());

        assert_dbg!(notify2.was_notified());
        assert_dbg!(!waiter2.is_linked());

        assert_eq_dbg!(
            q.continue_wait(waiter2.as_mut(), &notify2),
            WaitResult::Closed
        );

        assert_eq_dbg!(
            q.continue_wait(waiter1.as_mut(), &notify1),
            WaitResult::Closed
        );
    }

    #[test]
    fn remove_from_middle() {
        let q = WaitQueue::new();

        let notify1 = MockNotify::new();
        let notify2 = MockNotify::new();
        let notify3 = MockNotify::new();

        let mut waiter1 = Box::pin(Waiter::new());
        let mut waiter2 = Box::pin(Waiter::new());
        let mut waiter3 = Box::pin(Waiter::new());

        assert_eq_dbg!(q.start_wait(waiter1.as_mut(), &notify1), WaitResult::Wait);
        assert_dbg!(waiter1.is_linked());

        assert_eq_dbg!(q.start_wait(waiter2.as_mut(), &notify2), WaitResult::Wait);
        assert_dbg!(waiter2.is_linked());

        assert_eq_dbg!(q.start_wait(waiter3.as_mut(), &notify3), WaitResult::Wait);
        assert_dbg!(waiter2.is_linked());

        assert_dbg!(!notify1.was_notified());
        assert_dbg!(!notify2.was_notified());
        assert_dbg!(!notify3.was_notified());

        waiter2.as_mut().remove(&q);
        assert_dbg!(!notify2.was_notified());
        drop(waiter2);

        assert_dbg!(q.notify());

        assert_dbg!(notify1.was_notified());
        assert_dbg!(!waiter1.is_linked());

        assert_dbg!(!notify3.was_notified());
        assert_dbg!(waiter3.is_linked());

        assert_eq_dbg!(
            q.continue_wait(waiter3.as_mut(), &notify3),
            WaitResult::Wait
        );

        assert_eq_dbg!(
            q.continue_wait(waiter1.as_mut(), &notify1),
            WaitResult::Notified
        );
    }

    #[test]
    fn remove_after_notify() {
        let q = WaitQueue::new();

        let notify1 = MockNotify::new();
        let notify2 = MockNotify::new();
        let notify3 = MockNotify::new();

        let mut waiter1 = Box::pin(Waiter::new());
        let mut waiter2 = Box::pin(Waiter::new());
        let mut waiter3 = Box::pin(Waiter::new());

        assert_eq_dbg!(q.start_wait(waiter1.as_mut(), &notify1), WaitResult::Wait);
        assert_dbg!(waiter1.is_linked());

        assert_eq_dbg!(q.start_wait(waiter2.as_mut(), &notify2), WaitResult::Wait);
        assert_dbg!(waiter2.is_linked());

        assert_eq_dbg!(q.start_wait(waiter3.as_mut(), &notify3), WaitResult::Wait);
        assert_dbg!(waiter2.is_linked());

        assert_dbg!(!notify1.was_notified());
        assert_dbg!(!notify2.was_notified());
        assert_dbg!(!notify3.was_notified());

        assert_dbg!(q.notify());

        assert_dbg!(notify1.was_notified());
        assert_dbg!(!waiter1.is_linked());

        assert_dbg!(!notify2.was_notified());
        assert_dbg!(waiter2.is_linked());

        assert_dbg!(!notify3.was_notified());
        assert_dbg!(waiter3.is_linked());

        assert_eq_dbg!(
            q.continue_wait(waiter3.as_mut(), &notify3),
            WaitResult::Wait
        );

        assert_eq_dbg!(
            q.continue_wait(waiter2.as_mut(), &notify2),
            WaitResult::Wait
        );

        assert_eq_dbg!(
            q.continue_wait(waiter1.as_mut(), &notify1),
            WaitResult::Notified
        );

        waiter2.as_mut().remove(&q);
        assert_dbg!(!notify2.was_notified());
        drop(waiter2);

        assert_dbg!(!notify3.was_notified());
        assert_dbg!(waiter3.is_linked());

        assert_eq_dbg!(
            q.continue_wait(waiter3.as_mut(), &notify3),
            WaitResult::Wait
        );
    }
}
