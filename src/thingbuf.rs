use crate::{recycling::{self, Recycle}, Core, Full, Ref, Slot, MAX_CAPACITY};
use alloc::boxed::Box;
use core::fmt;

#[cfg(all(loom, test))]
mod tests;

/// A fixed-size, lock-free, multi-producer multi-consumer (MPMC) queue.
///
/// This is a fixed-capacity, first-in, first-out data structure. Elements are
/// enqueued at the front of the queue by calling the [`push`] method, and
/// dequeued from the end of the queue by calling the [`pop`] method. Elements
/// may be enqueued and dequeued from a `ThingBuf` concurrently by any number of
/// threads or asynchronous tasks.
///
/// This queue is an implementation of a [design by Dmitry Vyukov][vyukov] of
/// 1024cores.net.
///
/// # Examples
///
/// ```
/// use thingbuf::ThingBuf;
///
/// let q = ThingBuf::new(2);
///
/// // Push some values to the queue.
/// q.push(1).unwrap();
/// q.push(2).unwrap();
///
/// // Now, the queue is at capacity.
/// assert!(q.push(3).is_err());
/// ```
///
/// # Allocation
///
/// A `ThingBuf` is a fixed-size, array-based queue. Elements in the queue are
/// stored in a single array whose capacity is specified when the `ThingBuf` is
/// constructed. This means that a `ThingBuf` requires only a single heap
/// allocation over its entire lifespan. Calling [`ThingBuf::new`] will allocate
/// an array to store `capacity` elements. Subsequent calls to [`push`] and
/// [`pop`] will never allocate or deallocate memory.
///
/// If the size of the queue is known at compile-time, the [`StaticThingBuf`]
/// type, which requires *no* heap allocations at all, can be used instead.
///
/// ## Reusing Allocations
///
/// Of course, if the *elements* in the queue are themselves heap-allocated
/// (such as `String`s or `Vec`s), heap allocations and deallocations may still
/// occur when those types are created or dropped. However, `ThingBuf` also
/// provides an API for enqueueing and dequeueing elements *by reference*. In
/// some use cases, this API can be used to reduce allocations for queue elements.
///
/// As an example, consider the case where multiple threads in a program format
/// log messages and send them to a dedicated worker thread that writes those
/// messages to a file. A naive implementation might look something like this:
///
/// ```rust
/// use thingbuf::ThingBuf;
/// use std::{sync::Arc, fmt, thread, fs::File, error::Error, io::Write};
///
/// // Called by application threads to log a message.
/// fn log_event(q: &Arc<ThingBuf<String>>, message: &dyn fmt::Debug) {
///     // Format the log line to a `String`.
///     let line = format!("{:?}\n", message);
///     // Send the string to the worker thread.
///     let _ = q.push(line);
///     // If the queue was full, ignore the error and drop the log line.
/// }
///
/// fn main() -> Result<(), Box<dyn Error>> {
/// # // wrap the actual code in a function that's never called so that running
/// # // the test never actually creates the log file.
/// # fn docs() -> Result<(), Box<dyn Error>> {
///     let log_queue = Arc::new(ThingBuf::<String>::new(1024));
///
///     // Spawn the background worker thread.
///     let q = log_queue.clone();
///     let mut file = File::create("myapp.log")?;
///     thread::spawn(move || {
///         use std::io::Write;
///         loop {
///             // Pop from the queue, and write each log line to the file.
///             while let Some(line) = q.pop() {
///                 file.write_all(line.as_bytes()).unwrap();
///             }
///
///             // No more messages in the queue!
///             file.flush().unwrap();
///             thread::yield_now();
///         }
///     });
///
///     // ...
///     # Ok(())
/// # }
/// # Ok(())
/// }
/// ```
///
/// With this design, however, new `String`s are allocated for every message
/// that's logged, and then are immediately deallocated once they are written to
/// the file. This can have a negative performance impact.
///
/// Using `ThingBuf`'s [`push_ref`] and [`pop_ref`] methods, this code can be
/// redesigned to _reuse_ `String` allocations in place. With these methods,
/// rather than moving an element by-value into the queue when enqueueing, we
/// instead reserve the rights to _mutate_ a slot in the queue in place,
/// returning a [`Ref`]. Similarly, when dequeueing, we also recieve a [`Ref`]
/// that allows reading from (and mutating) the dequeued element. This allows
/// the queue to _own_ the set of `String`s used for formatting log messages,
/// which are cleared in place and the existing allocation reused.
///
/// The rewritten code might look like this:
///
/// ```rust
/// use thingbuf::ThingBuf;
/// use std::{sync::Arc, fmt, thread, fs::File, error::Error};
///
/// // Called by application threads to log a message.
/// fn log_event(q: &Arc<ThingBuf<String>>, message: &dyn fmt::Debug) {
///     use std::fmt::Write;
///
///     // Reserve a slot in the queue to write to.
///     if let Ok(mut slot) = q.push_ref() {
///         // Clear the string in place, retaining the allocated capacity.
///         slot.clear();
///         // Write the log message to the string.
///         write!(&mut *slot, "{:?}\n", message);
///     }
///     // Otherwise, if `push_ref` returns an error, the queue is full;
///     // ignore this log line.
/// }
///
/// fn main() -> Result<(), Box<dyn Error>> {
/// # // wrap the actual code in a function that's never called so that running
/// # // the test never actually creates the log file.
/// # fn docs() -> Result<(), Box<dyn Error>> {
///     let log_queue = Arc::new(ThingBuf::<String>::new(1024));
///
///     // Spawn the background worker thread.
///     let q = log_queue.clone();
///     let mut file = File::create("myapp.log")?;
///     thread::spawn(move || {
///         use std::io::Write;
///         loop {
///             // Pop from the queue, and write each log line to the file.
///             while let Some(line) = q.pop_ref() {
///                 file.write_all(line.as_bytes()).unwrap();
///             }
///
///             // No more messages in the queue!
///             file.flush().unwrap();
///             thread::yield_now();
///         }
///     });
///
///     // ...
///     # Ok(())
/// # }
/// # Ok(())
/// }
/// ```
///
/// In this implementation, the strings will only be reallocated if their
/// current capacity is not large enough to store the formatted representation
/// of the log message.
///
/// When using a `ThingBuf` in this manner, it can be thought of as a
/// combination of a concurrent queue and an [object pool].
///
/// [`push`]: Self::push
/// [`pop`]: Self::pop
/// [`push_ref`]: Self::push_ref
/// [`pop_ref`]: Self::pop_ref
/// [`StaticThingBuf`]: crate::StaticThingBuf
/// [vyukov]: https://www.1024cores.net/home/lock-free-algorithms/queues/bounded-mpmc-queue
/// [object pool]: https://en.wikipedia.org/wiki/Object_pool_pattern
#[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
pub struct ThingBuf<T, R = recycling::DefaultRecycle> {
    pub(crate) core: Core,
    pub(crate) slots: Box<[Slot<T>]>,
    recycle: R,
}

// === impl ThingBuf ===

impl<T: Default + Clone> ThingBuf<T> {
    /// Returns a new `ThingBuf` with space for `capacity` elements.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self::with_recycle(capacity, recycling::DefaultRecycle::new())
    }
}

impl<T, R> ThingBuf<T, R> {
    /// Returns the *total* capacity of this queue. This includes both
    /// occupied and unoccupied entries.
    ///
    /// To determine the queue's remaining *unoccupied* capacity, use
    /// [`remaining`] instead.
    ///
    /// # Examples
    ///
    /// ```
    /// use thingbuf::ThingBuf;
    ///
    /// let q = ThingBuf::<usize>::new(100);
    /// assert_eq!(q.capacity(), 100);
    /// ```
    ///
    /// Even after pushing several messages to the queue, the capacity remains
    /// the same:
    /// ```
    /// # use thingbuf::ThingBuf;
    ///
    /// let q = ThingBuf::<usize>::new(100);
    ///
    /// *q.push_ref().unwrap() = 1;
    /// *q.push_ref().unwrap() = 2;
    /// *q.push_ref().unwrap() = 3;
    ///
    /// assert_eq!(q.capacity(), 100);
    /// ```
    ///
    /// [`remaining`]: Self::remaining
    #[inline]
    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    /// Returns the unoccupied capacity of the queue (i.e., how many additional
    /// elements can be enqueued before the queue will be full).
    ///
    /// This is equivalent to subtracting the queue's [`len`] from its [`capacity`].
    ///
    /// [`len`]: Self::len
    /// [`capacity`]: Self::capacity
    pub fn remaining(&self) -> usize {
        self.capacity() - self.len()
    }

    /// Returns the number of elements in the queue
    ///
    /// To determine the queue's remaining *unoccupied* capacity, use
    /// [`remaining`] instead.
    ///
    /// # Examples
    ///
    /// ```
    /// use thingbuf::ThingBuf;
    ///
    /// let q = ThingBuf::new(100);
    /// assert_eq!(q.len(), 0);
    ///
    /// *q.push_ref().unwrap() = 1;
    /// *q.push_ref().unwrap() = 2;
    /// *q.push_ref().unwrap() = 3;
    /// assert_eq!(q.len(), 3);
    ///
    /// let _ = q.pop_ref();
    /// assert_eq!(q.len(), 2);
    /// ```
    ///
    /// [`remaining`]: Self::remaining
    #[inline]
    pub fn len(&self) -> usize {
        self.core.len()
    }

    /// Returns `true` if there are currently no elements in this `ThingBuf`.
    ///
    /// # Examples
    ///
    /// ```
    /// use thingbuf::ThingBuf;
    ///
    /// let q = ThingBuf::new(100);
    /// assert!(q.is_empty());
    ///
    /// *q.push_ref().unwrap() = 1;
    /// assert!(!q.is_empty());
    ///
    /// let _ = q.pop_ref();
    /// assert!(q.is_empty());
    /// ```
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T, R> ThingBuf<T, R>
where
    R: Recycle<T>,
{
    /// Returns a new `ThingBuf` with space for `capacity` elements and
    /// the provided [recycling policy].
    ///
    /// # Panics
    ///
    /// Panics if the capacity exceeds `usize::MAX & !(1 << (usize::BITS - 1))`. This value
    /// represents the highest power of two that can be expressed by a `usize`, excluding the most
    /// significant bit.
    ///
    /// [recycling policy]: crate::recycling::Recycle
    #[must_use]
    pub fn with_recycle(capacity: usize, recycle: R) -> Self {
        assert!(capacity > 0);
        assert!(capacity <= MAX_CAPACITY);
        Self {
            core: Core::new(capacity),
            slots: Slot::make_boxed_array(capacity),
            recycle,
        }
    }

    /// Reserves a slot to push an element into the queue, returning a [`Ref`] that
    /// can be used to write to that slot.
    ///
    /// This can be used to reuse allocations for queue elements in place,
    /// by clearing previous data prior to writing. In order to ensure
    /// allocations can be reused in place, elements should be dequeued using
    /// [`pop_ref`] rather than [`pop`]. If values are expensive to produce,
    /// `push_ref` can also be used to avoid producing a value if there is no
    /// capacity for it in the queue.
    ///
    /// For values that don't own heap allocations, or heap allocated values
    /// that cannot be reused in place, [`push`] can also be used.
    ///
    /// # Returns
    ///
    /// - `Ok(`[`Ref`]`)` if there is space for a new element
    /// - `Err(`[`Full`]`)`, if there is no capacity remaining in the queue
    ///
    /// # Examples
    ///
    /// ```rust
    /// use thingbuf::ThingBuf;
    ///
    /// let q = ThingBuf::new(1);
    ///
    /// // Reserve a `Ref` and enqueue an element.
    /// *q.push_ref().expect("queue should have capacity") = 10;
    ///
    /// // Now that the queue has one element in it, a subsequent `push_ref`
    /// // call will fail.
    /// assert!(q.push_ref().is_err());
    /// ```
    ///
    /// Avoiding producing an expensive element when the queue is at capacity:
    ///
    /// ```rust
    /// use thingbuf::ThingBuf;
    ///
    /// #[derive(Clone, Default)]
    /// struct Message {
    ///     // ...
    /// }
    ///
    /// fn make_expensive_message() -> Message {
    ///     // Imagine this function performs some costly operation, or acquires
    ///     // a limited resource...
    ///     # Message::default()
    /// }
    ///
    /// fn enqueue_message(q: &ThingBuf<Message>) {
    ///     loop {
    ///         match q.push_ref() {
    //              // If `push_ref` returns `Ok`, we've reserved a slot in
    ///             // the queue for our message, so it's okay to generate
    ///             // the expensive message.
    ///             Ok(mut slot) => {
    ///                 *slot = make_expensive_message();
    ///                 return;
    ///             },
    ///
    ///             // If there's no space in the queue, avoid generating
    ///             // an expensive message that won't be sent.
    ///             Err(_) => {
    ///                 // Wait until the queue has capacity...
    ///                 std::thread::yield_now();
    ///             }
    ///         }
    ///     }
    /// }
    /// ```
    ///
    /// [`pop`]: Self::pop
    /// [`pop_ref`]: Self::pop_ref
    /// [`push`]: Self::push_ref
    pub fn push_ref(&self) -> Result<Ref<'_, T>, Full> {
        self.core
            .push_ref(&self.slots, &self.recycle)
            .map_err(|e| match e {
                crate::mpsc::errors::TrySendError::Full(()) => Full(()),
                _ => unreachable!(),
            })
    }

    /// Attempt to enqueue an element by value.
    ///
    /// If the queue is full, the element is returned in the [`Full`] error.
    ///
    /// Unlike [`push_ref`], this method does not enable the reuse of previously
    /// allocated elements. If allocations are being reused by using
    /// [`push_ref`] and [`pop_ref`], this method should not be used, as it will
    /// drop pooled allocations.
    ///
    /// # Returns
    ///
    /// - `Ok(())` if the element was enqueued
    /// - `Err(`[`Full`]`)`, containing the value, if there is no capacity
    ///   remaining in the queue
    ///
    /// [`push_ref`]: Self::push_ref
    /// [`pop_ref`]: Self::pop_ref
    #[inline]
    pub fn push(&self, val: T) -> Result<(), Full<T>> {
        match self.push_ref() {
            Err(_) => Err(Full(val)),
            Ok(mut slot) => {
                *slot = val;
                Ok(())
            }
        }
    }

    /// Reserves a slot to push an element into the queue, and invokes the
    /// provided function `f` with a mutable reference to that element.
    ///
    /// # Returns
    ///
    /// - `Ok(U)` containing the return value of the provided function, if the
    ///   element was enqueued
    /// - `Err(`[`Full`]`)`, if there is no capacity remaining in the queue
    #[inline]
    pub fn push_with<U>(&self, f: impl FnOnce(&mut T) -> U) -> Result<U, Full> {
        self.push_ref().map(|mut r| r.with_mut(f))
    }

    /// Dequeue the first element in the queue, returning a [`Ref`] that can be
    /// used to read from (or mutate) the element.
    ///
    /// **Note**: A `ThingBuf` is *not* a "broadcast"-style queue. Each element
    /// is dequeued a single time. Once a thread has dequeued a given element,
    /// it is no longer the head of the queue.
    ///
    /// This can be used to reuse allocations for queue elements in place,
    /// by clearing previous data prior to writing. In order to ensure
    /// allocations can be reused in place, elements should be enqueued using
    /// [`push_ref`] rather than [`push`].
    ///
    /// For values that don't own heap allocations, or heap allocated values
    /// that cannot be reused in place, [`pop`] can also be used.
    ///
    /// # Returns
    ///
    /// - `Some(`[`Ref<T>`](Ref)`)` if an element was dequeued
    /// - `None` if there are no elements in the queue
    ///
    /// [`push_ref`]: Self::push_ref
    /// [`push`]: Self::push
    /// [`pop`]: Self::pop
    pub fn pop_ref(&self) -> Option<Ref<'_, T>> {
        self.core.pop_ref(&self.slots).ok()
    }

    /// Dequeue the first element in the queue *by value*, moving it out of the
    /// queue.
    ///
    /// **Note**: A `ThingBuf` is *not* a "broadcast"-style queue. Each element
    /// is dequeued a single time. Once a thread has dequeued a given element,
    /// it is no longer the head of the queue.
    ///
    /// # Returns
    ///
    /// - `Some(T)` if an element was dequeued
    /// - `None` if there are no elements in the queue
    #[inline]
    pub fn pop(&self) -> Option<T> {
        let mut slot = self.pop_ref()?;
        Some(recycling::take(&mut *slot, &self.recycle))
    }

    /// Dequeue the first element in the queue by reference, and invoke the
    /// provided function `f` with a mutable reference to the dequeued element.
    ///
    /// # Returns
    ///
    /// - `Some(U)` containing the return value of the provided function, if the
    ///   element was dequeued
    /// - `None` if the queue is empty
    #[inline]
    pub fn pop_with<U>(&self, f: impl FnOnce(&mut T) -> U) -> Option<U> {
        self.pop_ref().map(|mut r| r.with_mut(f))
    }
}

impl<T, R> Drop for ThingBuf<T, R> {
    fn drop(&mut self) {
        self.core.drop_slots(&mut self.slots[..]);
    }
}

impl<T, R: fmt::Debug> fmt::Debug for ThingBuf<T, R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ThingBuf")
            .field("len", &self.len())
            .field("slots", &format_args!("[...]"))
            .field("core", &self.core)
            .field("recycle", &self.recycle)
            .finish()
    }
}
