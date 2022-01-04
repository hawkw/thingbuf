use crate::loom::atomic::Ordering;
use crate::{Core, Full, Ref, Slot};
use alloc::boxed::Box;
use core::{fmt, ptr};

#[cfg(all(loom, test))]
mod tests;

/// A fixed-size, lock-free multi-producer multi-consumer queue.
#[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
pub struct ThingBuf<T> {
    pub(crate) core: Core,
    pub(crate) slots: Box<[Slot<T>]>,
}

// === impl ThingBuf ===

impl<T: Default> ThingBuf<T> {
    /// Return a new `ThingBuf` with space for `capacity` entries.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
        Self {
            core: Core::new(capacity),
            slots: Slot::make_boxed_array(capacity),
        }
    }

    /// Reserve a slot to push an entry into the queue, returning a [`Ref`] that
    /// can be used to write to that slot.
    ///
    /// This can be used to reuse allocations for queue entries in place,
    /// by clearing previous data prior to writing.
    pub fn push_ref(&self) -> Result<Ref<'_, T>, Full> {
        self.core.push_ref(&*self.slots).map_err(|e| match e {
            crate::mpsc::TrySendError::Full(()) => Full(()),
            _ => unreachable!(),
        })
    }

    #[inline]
    pub fn push_with<U>(&self, f: impl FnOnce(&mut T) -> U) -> Result<U, Full> {
        self.push_ref().map(|mut r| r.with_mut(f))
    }

    /// Dequeue the first element in the queue, returning a [`Ref`] that can be
    /// used to read from (or mutate) the element.
    pub fn pop_ref(&self) -> Option<Ref<'_, T>> {
        self.core.pop_ref(&*self.slots).ok()
    }

    #[inline]
    pub fn pop_with<U>(&self, f: impl FnOnce(&mut T) -> U) -> Option<U> {
        self.pop_ref().map(|mut r| r.with_mut(f))
    }
}

impl<T> ThingBuf<T> {
    /// Returns the total capacity of the `ThingBuf`. This includes both
    /// occupied and unoccupied entries.
    ///
    /// The number of _unoccupied_ entries can be determined by subtracing the
    /// value returned by [`len`] from the value returned by `capacity`.
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

    /// Returns the number of messages in the queue
    ///
    /// The number of _unoccupied_ entries can be determined by subtracing the
    /// value returned by [`len`] from the value returned by `capacity`.
    ///
    /// # Examples
    ///
    /// ```
    /// use thingbuf::ThingBuf;
    ///
    /// let q = ThingBuf::<usize>::new(100);
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
    /// let q = ThingBuf::<usize>::new(100);
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
