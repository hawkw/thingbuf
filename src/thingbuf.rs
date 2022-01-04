use crate::loom::atomic::Ordering;
use crate::{Core, Full, Ref, Slot};
use alloc::boxed::Box;
use core::{fmt, mem, ptr};

#[cfg(all(loom, test))]
mod tests;

/// A fixed-size lock-free multi-producer multi-consumer queue.
#[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
pub struct ThingBuf<T> {
    pub(crate) core: Core,
    pub(crate) slots: Box<[Slot<T>]>,
}

// === impl ThingBuf ===

impl<T: Default> ThingBuf<T> {
    /// Returns a new `ThingBuf` with space for `capacity` entries.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
        Self {
            core: Core::new(capacity),
            slots: Slot::make_boxed_array(capacity),
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
        self.core.push_ref(&*self.slots).map_err(|e| match e {
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
        self.core.pop_ref(&*self.slots).ok()
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
        Some(mem::take(&mut *slot))
    }

    #[inline]
    pub fn pop_with<U>(&self, f: impl FnOnce(&mut T) -> U) -> Option<U> {
        self.pop_ref().map(|mut r| r.with_mut(f))
    }
}

impl<T> ThingBuf<T> {
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
    /// [`capacity]: Self::capacity
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
    #[inline]
    pub fn len(&self) -> usize {
        self.core.len()
    }

    /// Returns `true` if there are currently no entries in this `ThingBuf`.
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

impl<T> Drop for ThingBuf<T> {
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

impl<T> fmt::Debug for ThingBuf<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ThingBuf")
            .field("len", &self.len())
            .field("slots", &format_args!("[...]"))
            .field("core", &self.core)
            .finish()
    }
}
