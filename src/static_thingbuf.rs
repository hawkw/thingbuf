use crate::loom::atomic::Ordering;
use crate::{Core, Full, Ref, Slot};
use core::{fmt, mem, ptr};

/// A statically allocated, fixed-size lock-free multi-producer multi-consumer
/// queue.
///
/// This type is identical to the [`ThingBuf`] type, except that the queue's
/// capacity is controlled by a const generic parameter, rather than dynamically
/// allocated at runtime. This means that a `StaticThingBuf` can be used without
/// requiring heap allocation, and can be stored in a `static`.
///
/// # Examples
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
pub struct StaticThingBuf<T, const CAP: usize> {
    core: Core,
    slots: [Slot<T>; CAP],
}

// === impl ThingBuf ===

#[cfg(not(test))]
impl<T, const CAP: usize> StaticThingBuf<T, CAP> {
    pub const fn new() -> Self {
        Self {
            core: Core::new(CAP),
            slots: Slot::make_static_array::<CAP>(),
        }
    }
}

impl<T, const CAP: usize> StaticThingBuf<T, CAP> {
    #[inline]
    pub fn capacity(&self) -> usize {
        CAP
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.core.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T: Default, const CAP: usize> StaticThingBuf<T, CAP> {
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
    /// #[derive(Default)]
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
        self.core.push_ref(&self.slots).map_err(|e| match e {
            crate::mpsc::TrySendError::Full(()) => Full(()),
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
        Some(mem::take(&mut *slot))
    }

    #[inline]
    pub fn pop_with<U>(&self, f: impl FnOnce(&mut T) -> U) -> Option<U> {
        self.pop_ref().map(|mut r| r.with_mut(f))
    }
}

impl<T, const CAP: usize> Drop for StaticThingBuf<T, CAP> {
    fn drop(&mut self) {
        let tail = self.core.tail.load(Ordering::SeqCst);
        let (idx, gen) = self.core.idx_gen(tail);
        let num_initialized = if gen > 0 { self.capacity() } else { idx };
        for slot in &self.slots[..num_initialized] {
            unsafe {
                slot.value
                    .with_mut(|value| ptr::drop_in_place((*value).as_mut_ptr()));
            }
        }
    }
}

impl<T, const CAP: usize> fmt::Debug for StaticThingBuf<T, CAP> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StaticThingBuf")
            .field("len", &self.len())
            .field("slots", &format_args!("[...]"))
            .field("core", &self.core)
            .finish()
    }
}
