use crate::loom::atomic::Ordering;
use crate::{Core, Full, Ref, Slot};
use alloc::boxed::Box;
use core::{fmt, ptr};

#[cfg(test)]
mod tests;

pub struct ThingBuf<T> {
    core: Core,
    slots: Box<[Slot<T>]>,
}

// === impl ThingBuf ===

impl<T: Default> ThingBuf<T> {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
        let slots = (0..capacity).map(|_| Slot::empty()).collect();
        Self {
            core: Core::new(capacity),
            slots,
        }
    }

    pub fn push_ref(&self) -> Result<Ref<'_, T>, Full> {
        self.core.push_ref(&*self.slots)
    }

    #[inline]
    pub fn push_with<U>(&self, f: impl FnOnce(&mut T) -> U) -> Result<U, Full> {
        self.push_ref().map(|mut r| r.with_mut(f))
    }

    pub fn pop_ref(&self) -> Option<Ref<'_, T>> {
        self.core.pop_ref(&*self.slots)
    }

    #[inline]
    pub fn pop_with<U>(&self, f: impl FnOnce(&mut T) -> U) -> Option<U> {
        self.pop_ref().map(|mut r| r.with_mut(f))
    }
}

impl<T> ThingBuf<T> {
    #[inline]
    pub fn capacity(&self) -> usize {
        self.slots.len()
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
