use crate::{
    loom::{
        atomic::{AtomicBool, AtomicUsize, Ordering::*},
        cell::UnsafeCell,
    },
    util::{mutex::Mutex, CachePadded},
    wait::{Notify, WaitResult},
};

use core::{fmt, marker::PhantomPinned, pin::Pin, ptr::NonNull};

/// A queue of waiters ([`core::task::Waker`]s or [`std;:thread::Thread`]s)
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
/// [intrusive]: https://fuchsia.dev/fuchsia-src/development/languages/c-cpp/fbl_containers_guide/introduction
/// [2]: https://www.1024cores.net/home/lock-free-algorithms/queues/intrusive-mpsc-node-based-queue
#[derive(Debug)]
pub(crate) struct WaitQueue<T> {
    /// The wait queue's state variable. The first bit indicates whether the
    /// queue is closed; the remainder is a counter of notifications assigned to
    /// the queue because no waiters were currently available to be woken.
    ///
    /// These stored notifications are "consumed" when new waiters are
    /// registered; those waiters will be woken immediately rather than being
    /// enqueued to wait.
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
    /// A spinlock is used on `no_std` platforms; [`std::sync::Mutex`] is used
    /// when the standard library is available.
    list: Mutex<List<T>>,
}

/// A waiter node which may be linked into a wait queue.
#[derive(Debug)]
pub(crate) struct Waiter<T> {
    node: UnsafeCell<Node<T>>,
    pub(crate) was_woken_from_queue: AtomicBool,
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

const CLOSED: usize = 1;
const ONE_QUEUED: usize = 2;
const EMPTY: usize = 0;

impl<T: Notify + Unpin> WaitQueue<T> {
    pub(crate) fn new() -> Self {
        Self {
            state: CachePadded(AtomicUsize::new(EMPTY)),
            list: Mutex::new(List::new()),
        }
    }

    pub(crate) fn wait(
        &self,
        waiter: &mut Option<Pin<&mut Waiter<T>>>,
        register: impl FnOnce(&mut Option<T>),
    ) -> WaitResult {
        test_println!("WaitQueue::push_waiter()");

        // First, go ahead and check if the queue has been closed. This is
        // necessary even if `waiter` is `None`, as the waiter may already be
        // queued, and just checking if the list was closed.
        // TODO(eliza): that actually kind of sucks lol...
        // Is there at least one queued notification assigned to the wait
        // queue? If so, try to consume that now, rather than waiting.
        match test_dbg!(self
            .state
            .compare_exchange(ONE_QUEUED, EMPTY, AcqRel, Acquire))
        {
            Ok(_) => return WaitResult::Notified,
            Err(CLOSED) => return WaitResult::Closed,
            Err(_state) => debug_assert_eq!(_state, EMPTY),
        }

        // If we were actually called with a real waiter, try to queue the node.
        if test_dbg!(waiter.is_some()) {
            // There are no queued notifications to consume, and the queue is
            // still open. Therefore, it's time to actually push the waiter to
            // the queue...finally lol :)

            // Grab the lock...
            let mut list = self.list.lock();

            // Okay, we have the lock...but what if someone changed the state
            // WHILE we were waiting to acquire the lock? isn't concurrent
            // programming great? :) :) :) :) :)
            // Try to consume a queued notification *again* in case any were
            // assigned to the queue while we were waiting to acquire the lock.
            match test_dbg!(self
                .state
                .compare_exchange(ONE_QUEUED, EMPTY, AcqRel, Acquire))
            {
                Ok(_) => return WaitResult::Notified,
                Err(CLOSED) => return WaitResult::Closed,
                Err(_state) => debug_assert_eq!(_state, EMPTY),
            }

            // We didn't consume a queued notification. it is now, finally, time
            // to actually put the waiter in the linked list. wasn't that fun?

            if let Some(waiter) = waiter.take() {
                // Now that we have the lock, register the `Waker` or `Thread`
                // to
                let should_queue = unsafe {
                    test_println!("WaitQueue::push_waiter -> registering {:p}", waiter);
                    // Safety: the waker can only be registered while holding
                    // the wait queue lock. We are holding the lock, so no one
                    // else will try to touch the waker until we're done.
                    waiter.with_node(|node| {
                        // Does the node need to be added to the wait queue? If
                        // it currently has a waiter (prior to registering),
                        // then we know it's already in the queue. Otherwise, if
                        // it doesn't have a waiter, it is either waiting for
                        // the first time, or it is re-registering after a
                        // notification that it wasn't able to consume (for some
                        // reason).
                        let should_queue = node.waiter.is_none();
                        register(&mut node.waiter);
                        should_queue
                    })
                };
                if test_dbg!(should_queue) {
                    test_println!("WaitQueue::push_waiter -> pushing {:p}", waiter);
                    test_dbg!(waiter.was_woken_from_queue.swap(false, AcqRel));
                    list.push_front(waiter);
                }
            } else {
                // XXX(eliza): in practice we can't ever get here because of the
                // `if` above. this should probably be `unreachable_unchecked`
                // but i'm a coward...
                unreachable!("this could be unchecked...")
            }
        }

        WaitResult::Wait
    }

    /// Notify one waiter from the queue. If there are no waiters in the linked
    /// list, the notification is instead assigned to the queue itself.
    ///
    /// If a waiter was popped from the queue, returns `true`. Otherwise, if the
    /// notification was assigned to the queue, returns `false`.
    pub(crate) fn notify(&self) -> bool {
        test_println!("WaitQueue::notify()");
        let mut list = self.list.lock();
        if let Some(node) = list.pop_back() {
            drop(list);
            test_println!("notifying {:?}", node);
            node.notify();
            true
        } else {
            test_println!("no waiters to notify...");
            // This can be relaxed because we're holding the lock.
            test_dbg!(self.state.store(ONE_QUEUED, Relaxed));
            false
        }
    }

    /// Close the queue, notifying all waiting tasks.
    pub(crate) fn close(&self) {
        test_println!("WaitQueue::close()");
        test_dbg!(self.state.store(CLOSED, Release));
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
            node: UnsafeCell::new(Node {
                next: None,
                prev: None,
                waiter: None,
                _pin: PhantomPinned,
            }),
            was_woken_from_queue: AtomicBool::new(false),
        }
    }
}

impl<T> Waiter<T> {
    unsafe fn with_node<U>(&self, f: impl FnOnce(&mut Node<T>) -> U) -> U {
        self.node.with_mut(|node| f(&mut *node))
    }

    unsafe fn set_prev(&mut self, prev: Option<NonNull<Waiter<T>>>) {
        self.node.with_mut(|node| (*node).prev = prev);
    }

    unsafe fn take_prev(&mut self) -> Option<NonNull<Waiter<T>>> {
        self.node.with_mut(|node| (*node).prev.take())
    }

    unsafe fn take_next(&mut self) -> Option<NonNull<Waiter<T>>> {
        self.node.with_mut(|node| (*node).next.take())
    }
}

impl<T: Notify> Waiter<T> {
    pub(crate) fn remove(self: Pin<&mut Self>, q: &WaitQueue<T>) {
        test_println!("Waiter::remove({:p})", self);
        unsafe {
            // Safety: removing a node is unsafe even when the list is locked,
            // because there's no way to guarantee that the node is part of
            // *this* list. However, the potential callers of this method will
            // never have access to any other linked lists, so we can just kind
            // of assume that this is safe.
            q.list.lock().remove(self);
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

    fn push_front(&mut self, waiter: Pin<&mut Waiter<T>>) {
        unsafe {
            waiter.with_node(|node| {
                node.next = self.head;
                node.prev = None;
            })
        }

        let ptr = unsafe { NonNull::from(Pin::into_inner_unchecked(waiter)) };

        debug_assert_ne!(self.head, Some(ptr), "tried to push the same waiter twice!");
        if let Some(mut head) = self.head.replace(ptr) {
            unsafe {
                head.as_mut().set_prev(Some(ptr));
            }
        }

        if self.tail.is_none() {
            self.tail = Some(ptr);
        }
    }

    fn pop_back(&mut self) -> Option<T> {
        let mut last = self.tail?;
        test_println!("List::pop_back() -> {:p}", last);

        unsafe {
            let last = last.as_mut();
            let prev = last.take_prev();

            match prev {
                Some(mut prev) => {
                    let _ = prev.as_mut().take_next();
                }
                None => self.head = None,
            }

            self.tail = prev;
            last.take_next();
            last.was_woken_from_queue.store(true, Relaxed);
            last.with_node(|node| node.waiter.take())
        }
    }

    unsafe fn remove(&mut self, node: Pin<&mut Waiter<T>>) {
        let node_ref = node.get_unchecked_mut();
        let prev = node_ref.take_prev();
        let next = node_ref.take_next();
        let ptr = NonNull::from(node_ref);

        if let Some(mut prev) = prev {
            prev.as_mut().with_node(|prev| {
                debug_assert_eq!(prev.next, Some(ptr));
                prev.next = next;
            });
        } else if self.head == Some(ptr) {
            self.head = next;
        }

        if let Some(mut next) = next {
            next.as_mut().with_node(|next| {
                debug_assert_eq!(next.prev, Some(ptr));
                next.prev = prev;
            });
        } else if self.tail == Some(ptr) {
            self.tail = prev;
        }
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
