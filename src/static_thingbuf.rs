use crate::{
    recycling::{self, Recycle},
    Core, Full, Ref, Slot,
};
use core::fmt;

/// A statically allocated, fixed-size lock-free multi-producer multi-consumer
/// queue.
///
/// This type is identical to the [`ThingBuf`] type, except that the queue's
/// capacity is controlled by a const generic parameter, rather than dynamically
/// allocated at runtime. This means that a `StaticThingBuf` can be used without
/// requiring heap allocation, and can be stored in a `static`.
///
/// `StaticThingBuf` will likely be particularly useful for embedded systems,
/// bare-metal code like operating system kernels or device drivers, and other
/// contexts where allocations may not be available. It can also be used to
/// implement a global queue that lives for the entire lifespan of a program,
/// without having to pass around reference-counted [`std::sync::Arc`] pointers.
///
/// # Examples
///
/// Storing a `StaticThingBuf` in a `static`:
///
/// ```rust
/// use thingbuf::StaticThingBuf;
///
/// static MY_THINGBUF: StaticThingBuf<i32, 8> = StaticThingBuf::new();
///
/// fn main() {
///     assert_eq!(MY_THINGBUF.push(1), Ok(()));
///     assert_eq!(MY_THINGBUF.pop(), Some(1));
/// }
/// ```
///
/// Constructing a `StaticThingBuf` on the stack:
///
/// ```rust
/// use thingbuf::StaticThingBuf;
///
/// let q = StaticThingBuf::<_, 2>::new();
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
/// A `StaticThingBuf` is a fixed-size, array-based queue. Elements in the queue are
/// stored in a single array whose capacity is specified at compile time as a
/// `const` generic parameter. This means that a `StaticThingBuf` requires *no*
/// heap allocations. Calling [`StaticThingBuf::new`], [`push`] or [`pop`] will
/// never allocate or deallocate memory.
///
/// If the size of the queue is dynamic and not known at compile-time, the [`ThingBuf`]
/// type, which performs a single heap allocation, can be used instead (provided
/// that the `alloc` feature flag is enabled).
///
/// ## Reusing Allocations
///
/// Of course, if the *elements* in the queue are themselves heap-allocated
/// (such as `String`s or `Vec`s), heap allocations and deallocations may still
/// occur when those types are created or dropped. However, `StaticThingBuf` also
/// provides an API for enqueueing and dequeueing elements *by reference*. In
/// some use cases, this API can be used to reduce allocations for queue elements.
///
/// As an example, consider the case where multiple threads in a program format
/// log messages and send them to a dedicated worker thread that writes those
/// messages to a file. A naive implementation might look something like this:
///
/// ```rust
/// use thingbuf::StaticThingBuf;
/// use std::{sync::Arc, fmt, thread, fs::File, error::Error, io::Write};
///
/// static LOG_QUEUE: StaticThingBuf<String, 1048> = StaticThingBuf::new();
///
/// // Called by application threads to log a message.
/// fn log_event(message: &dyn fmt::Debug) {
///     // Format the log line to a `String`.
///     let line = format!("{:?}\n", message);
///     // Send the string to the worker thread.
///     let _ = LOG_QUEUE.push(line);
///     // If the queue was full, ignore the error and drop the log line.
/// }
///
/// fn main() -> Result<(), Box<dyn Error>> {
/// # // wrap the actual code in a function that's never called so that running
/// # // the test never actually creates the log file.
/// # fn docs() -> Result<(), Box<dyn Error>> {
///     // Spawn the background worker thread.
///     let mut file = File::create("myapp.log")?;
///     thread::spawn(move || {
///         use std::io::Write;
///         loop {
///             // Pop from the queue, and write each log line to the file.
///             while let Some(line) = LOG_QUEUE.pop() {
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
/// use thingbuf::StaticThingBuf;
/// use std::{sync::Arc, fmt, thread, fs::File, error::Error};
///
/// static LOG_QUEUE: StaticThingBuf<String, 1048> = StaticThingBuf::new();
///
/// // Called by application threads to log a message.
/// fn log_event( message: &dyn fmt::Debug) {
///     use std::fmt::Write;
///
///     // Reserve a slot in the queue to write to.
///     if let Ok(mut slot) = LOG_QUEUE.push_ref() {
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
///     // Spawn the background worker thread.
///     let mut file = File::create("myapp.log")?;
///     thread::spawn(move || {
///         use std::io::Write;
///         loop {
///             // Pop from the queue, and write each log line to the file.
///             while let Some(line) = LOG_QUEUE.pop_ref() {
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
/// [`ThingBuf`]: crate::ThingBuf
/// [vyukov]: https://www.1024cores.net/home/lock-free-algorithms/queues/bounded-mpmc-queue
/// [object pool]: https://en.wikipedia.org/wiki/Object_pool_pattern
#[cfg_attr(docsrs, doc(cfg(feature = "static")))]
pub struct StaticThingBuf<T, const CAP: usize, R = recycling::DefaultRecycle> {
    core: Core,
    recycle: R,
    slots: [Slot<T>; CAP],
}

// === impl ThingBuf ===

#[cfg(not(test))]
impl<T, const CAP: usize> StaticThingBuf<T, CAP> {
    /// Returns a new `StaticThingBuf` with space for `CAP` elements.
    ///
    /// This queue will use the [default recycling policy].
    ///
    /// [recycling policy]: crate::recycling::DefaultRecycle
    #[must_use]
    pub const fn new() -> Self {
        Self::with_recycle(recycling::DefaultRecycle::new())
    }
}

impl<T, const CAP: usize, R> StaticThingBuf<T, CAP, R> {
    /// Returns a new `StaticThingBuf` with space for `CAP` elements and
    /// the provided [recycling policy].
    ///
    /// [recycling policy]: crate::recycling::Recycle
    #[must_use]
    pub const fn with_recycle(recycle: R) -> Self {
        StaticThingBuf {
            core: Core::new(CAP),
            recycle,
            slots: Slot::make_static_array::<CAP>(),
        }
    }
}

impl<T, const CAP: usize, R> StaticThingBuf<T, CAP, R> {
    /// Returns the *total* capacity of this queue. This includes both
    /// occupied and unoccupied entries.
    ///
    /// To determine the queue's remaining *unoccupied* capacity, use
    /// [`remaining`] instead.
    ///
    /// # Examples
    ///
    /// ```
    /// # use thingbuf::StaticThingBuf;
    /// static MY_THINGBUF: StaticThingBuf::<usize, 100> = StaticThingBuf::new();
    ///
    /// assert_eq!(MY_THINGBUF.capacity(), 100);
    /// ```
    ///
    /// Even after pushing several messages to the queue, the capacity remains
    /// the same:
    /// ```
    /// # use thingbuf::StaticThingBuf;
    /// static MY_THINGBUF: StaticThingBuf::<usize, 100> = StaticThingBuf::new();
    ///
    /// *MY_THINGBUF.push_ref().unwrap() = 1;
    /// *MY_THINGBUF.push_ref().unwrap() = 2;
    /// *MY_THINGBUF.push_ref().unwrap() = 3;
    ///
    /// assert_eq!(MY_THINGBUF.capacity(), 100);
    /// ```
    ///
    /// [`remaining`]: Self::remaining
    #[inline]
    pub fn capacity(&self) -> usize {
        CAP
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
    /// # use thingbuf::StaticThingBuf;
    /// static MY_THINGBUF: StaticThingBuf::<usize, 100> = StaticThingBuf::new();
    ///
    /// assert_eq!(MY_THINGBUF.len(), 0);
    ///
    /// *MY_THINGBUF.push_ref().unwrap() = 1;
    /// *MY_THINGBUF.push_ref().unwrap() = 2;
    /// *MY_THINGBUF.push_ref().unwrap() = 3;
    /// assert_eq!(MY_THINGBUF.len(), 3);
    ///
    /// let _ = MY_THINGBUF.pop_ref();
    /// assert_eq!(MY_THINGBUF.len(), 2);
    /// ```
    ///
    /// [`remaining`]: Self::remaining
    #[inline]
    pub fn len(&self) -> usize {
        self.core.len()
    }

    /// Returns `true` if there are currently no elements in this `StaticThingBuf`.
    ///
    /// # Examples
    ///
    /// ```
    /// # use thingbuf::StaticThingBuf;
    /// static MY_THINGBUF: StaticThingBuf::<usize, 100> = StaticThingBuf::new();
    ///
    /// assert!(MY_THINGBUF.is_empty());
    ///
    /// *MY_THINGBUF.push_ref().unwrap() = 1;
    /// assert!(!MY_THINGBUF.is_empty());
    ///
    /// let _ = MY_THINGBUF.pop_ref();
    /// assert!(MY_THINGBUF.is_empty());
    /// ```
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T, const CAP: usize, R> StaticThingBuf<T, CAP, R>
where
    R: Recycle<T>,
{
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
    /// use thingbuf::StaticThingBuf;
    ///
    /// static MY_THINGBUF: StaticThingBuf<i32, 1> = StaticThingBuf::new();
    ///
    /// // Reserve a `Ref` and enqueue an element.
    /// *MY_THINGBUF.push_ref().expect("queue should have capacity") = 10;
    ///
    /// // Now that the queue has one element in it, a subsequent `push_ref`
    /// // call will fail.
    /// assert!(MY_THINGBUF.push_ref().is_err());
    /// ```
    ///
    /// Avoiding producing an expensive element when the queue is at capacity:
    ///
    /// ```rust
    /// use thingbuf::StaticThingBuf;
    ///
    /// static MESSAGES: StaticThingBuf<Message, 16> = StaticThingBuf::new();
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
    /// fn enqueue_message() {
    ///     loop {
    ///         match MESSAGES.push_ref() {
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
    /// **Note**: A `StaticThingBuf` is *not* a "broadcast"-style queue. Each element
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
    /// **Note**: A `StaticThingBuf` is *not* a "broadcast"-style queue. Each element
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

impl<T, const CAP: usize, R: fmt::Debug> fmt::Debug for StaticThingBuf<T, CAP, R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StaticThingBuf")
            .field("len", &self.len())
            .field("slots", &format_args!("[...]"))
            .field("core", &self.core)
            .field("recycle", &self.recycle)
            .finish()
    }
}

impl<T, const CAP: usize, R> Drop for StaticThingBuf<T, CAP, R> {
    fn drop(&mut self) {
        self.core.drop_slots(&mut self.slots[..]);
    }
}
