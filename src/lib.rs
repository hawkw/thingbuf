use core::{fmt, marker::PhantomData, mem::MaybeUninit};

#[cfg(feature = "alloc")]
extern crate alloc;

macro_rules! test_println {
    ($($arg:tt)*) => {
        if cfg!(test) {
            if std::thread::panicking() {
                // getting the thread ID while panicking doesn't seem to play super nicely with loom's
                // mock lazy_static...
                println!("[PANIC {:>17}:{:<3}] {}", file!(), line!(), format_args!($($arg)*))
            } else {
                #[cfg(test)]
                println!("[{:?} {:>17}:{:<3}] {}", crate::loom::thread::current().id(), file!(), line!(), format_args!($($arg)*))
            }
        }
    }
}

macro_rules! test_dbg {
    ($e:expr) => {
        match $e {
            e => {
                #[cfg(test)]
                test_println!("{} = {:?}", stringify!($e), &e);
                e
            }
        }
    };
}

mod array;
mod loom;
#[cfg(test)]
mod tests;
mod util;

use crate::{
    loom::{
        atomic::{AtomicUsize, Ordering},
        UnsafeCell,
    },
    util::{Backoff, CachePadded},
};

pub use crate::array::AsArray;

#[cfg(feature = "alloc")]
mod stringbuf;

#[cfg(feature = "alloc")]
pub use stringbuf::StringBuf;

/// A ringbuf of...things.
///
/// # Examples
///
/// Using a
pub struct ThingBuf<T, S = Box<[Slot<T>]>> {
    head: CachePadded<AtomicUsize>,
    tail: CachePadded<AtomicUsize>,
    gen: usize,
    gen_mask: usize,
    idx_mask: usize,
    capacity: usize,
    slots: S,
    _t: PhantomData<T>,
}

pub struct Ref<'slot, T> {
    slot: &'slot Slot<T>,
    new_state: usize,
}

#[derive(Debug)]
pub struct AtCapacity(usize);

pub struct Slot<T> {
    value: UnsafeCell<T>,
    state: AtomicUsize,
}

// === impl ThingBuf ===

impl<T: Default> ThingBuf<T> {
    pub fn new(capacity: usize) -> Self {
        Self::new_with(capacity, T::default)
    }
}

impl<T> ThingBuf<T> {
    pub fn new_with(capacity: usize, mut initializer: impl FnMut() -> T) -> Self {
        assert!(capacity > 0);
        let slots = (0..capacity)
            .map(|idx| Slot {
                state: AtomicUsize::new(idx),
                value: UnsafeCell::new(initializer()),
            })
            .collect();
        let gen = (capacity + 1).next_power_of_two();
        let idx_mask = gen - 1;
        let gen_mask = !(gen - 1);
        Self {
            head: CachePadded(AtomicUsize::new(0)),
            tail: CachePadded(AtomicUsize::new(0)),
            gen,
            gen_mask,
            idx_mask,
            capacity,
            slots,
            _t: PhantomData,
        }
    }
}

impl<T, S> ThingBuf<T, S> {
    #[inline]
    fn idx_gen(&self, val: usize) -> (usize, usize) {
        (val & self.idx_mask, val & self.gen_mask)
    }

    #[inline]
    fn next(&self, idx: usize, gen: usize) -> usize {
        // Are we still in the same generation?
        if idx + 1 < self.capacity() {
            // If we're still within the current generation, increment the index
            // by 1.
            (idx | gen) + 1
        } else {
            // We've reached the end of the current lap, wrap the index around
            // to 0.
            gen.wrapping_add(self.gen)
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

impl<T, S> ThingBuf<T, S>
where
    S: AsArray<T>,
{
    pub fn from_array(slots: S) -> Self {
        let capacity = slots.len();
        assert!(capacity > 0);
        for (idx, slot) in slots.as_array().iter().enumerate() {
            // Relaxed is fine here, because the slot is not shared yet.
            slot.state.store(idx, Ordering::Relaxed);
        }
        let gen = (capacity + 1).next_power_of_two();
        let idx_mask = gen - 1;
        let gen_mask = !(gen - 1);
        Self {
            head: CachePadded(AtomicUsize::new(0)),
            tail: CachePadded(AtomicUsize::new(0)),
            gen,
            gen_mask,
            idx_mask,
            capacity,
            slots,
            _t: PhantomData,
        }
    }

    #[inline]
    pub fn push_with<U>(&self, f: impl FnOnce(&mut T) -> U) -> Result<U, AtCapacity> {
        self.push_ref().map(|mut r| r.with_mut(f))
    }

    pub fn push_ref(&self) -> Result<Ref<'_, T>, AtCapacity> {
        let mut backoff = Backoff::new();
        let mut tail = self.tail.load(Ordering::Relaxed);
        let slots = self.slots.as_array();

        loop {
            let (idx, gen) = self.idx_gen(tail);
            test_dbg!(idx);
            test_dbg!(gen);
            let slot = &slots[idx];
            let state = slot.state.load(Ordering::Acquire);

            if state == tail {
                // Move the tail index forward by 1.
                let next_tail = self.next(idx, gen);
                match self.tail.compare_exchange_weak(
                    tail,
                    next_tail,
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        return Ok(Ref {
                            new_state: tail + 1,
                            slot,
                        })
                    }
                    Err(actual) => {
                        tail = actual;
                        backoff.spin();
                        continue;
                    }
                }
            }

            if state.wrapping_add(self.gen) == tail + 1 {
                if self.head.load(Ordering::SeqCst).wrapping_add(self.gen) == tail {
                    return Err(AtCapacity(self.capacity()));
                }

                backoff.spin();
            } else {
                backoff.spin_yield();
            }

            tail = self.tail.load(Ordering::Relaxed)
        }
    }

    #[inline]
    pub fn pop_with<U>(&self, f: impl FnOnce(&mut T) -> U) -> Option<U> {
        self.pop_ref().map(|mut r| r.with_mut(f))
    }

    pub fn pop_ref(&self) -> Option<Ref<'_, T>> {
        let mut backoff = Backoff::new();
        let mut head = self.head.load(Ordering::Relaxed);
        let slots = self.slots.as_array();

        loop {
            test_dbg!(head);
            let (idx, gen) = self.idx_gen(head);
            test_dbg!(idx);
            test_dbg!(gen);
            let slot = &slots[idx];
            let state = slot.state.load(Ordering::Acquire);
            test_dbg!(state);

            // If the slot's state is ahead of the head index by one, we can pop
            // it.
            if test_dbg!(state == head + 1) {
                let next_head = self.next(idx, gen);
                match self.head.compare_exchange(
                    head,
                    next_head,
                    Ordering::SeqCst,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        return Some(Ref {
                            new_state: head.wrapping_add(self.gen),
                            slot,
                        })
                    }
                    Err(actual) => {
                        head = actual;
                        backoff.spin();
                        continue;
                    }
                }
            }

            if test_dbg!(state == head) {
                let tail = self.tail.load(Ordering::SeqCst);

                if test_dbg!(tail == head) {
                    return None;
                }

                backoff.spin();
            } else {
                backoff.spin_yield();
            }

            head = self.head.load(Ordering::Relaxed);
        }
    }
}

impl<T, S> fmt::Debug for ThingBuf<T, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ThingBuf")
            .field("capacity", &self.capacity())
            .finish()
    }
}

// === impl Ref ===

impl<T> Ref<'_, T> {
    #[inline]
    pub fn with<U>(&self, f: impl FnOnce(&T) -> U) -> U {
        self.slot.value.with(|value| unsafe {
            // Safety: if a `Ref` exists, we have exclusive ownership of the slot.
            f(&*value)
        })
    }

    #[inline]
    pub fn with_mut<U>(&mut self, f: impl FnOnce(&mut T) -> U) -> U {
        self.slot.value.with_mut(|value| unsafe {
            // Safety: if a `Ref` exists, we have exclusive ownership of the slot.
            f(&mut *value)
        })
    }
}

impl<T> Drop for Ref<'_, T> {
    #[inline]
    fn drop(&mut self) {
        test_println!("drop_ref");
        self.slot
            .state
            .store(test_dbg!(self.new_state), Ordering::Release);
    }
}

impl<T: fmt::Debug> fmt::Debug for Ref<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.with(|val| fmt::Debug::fmt(val, f))
    }
}

impl<T: fmt::Display> fmt::Display for Ref<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.with(|val| fmt::Display::fmt(val, f))
    }
}

impl<T: fmt::Write> fmt::Write for Ref<'_, T> {
    #[inline]
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.with_mut(|val| val.write_str(s))
    }

    #[inline]
    fn write_char(&mut self, c: char) -> fmt::Result {
        self.with_mut(|val| val.write_char(c))
    }

    #[inline]
    fn write_fmt(&mut self, f: fmt::Arguments<'_>) -> fmt::Result {
        self.with_mut(|val| val.write_fmt(f))
    }
}

// === impl Slot ===

impl<T: Default> Default for Slot<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> Slot<T> {
    const UNINIT: usize = usize::MAX;

    #[cfg(not(test))]
    pub const fn new(t: T) -> Self {
        Self {
            value: UnsafeCell::new(t),
            state: AtomicUsize::new(Self::UNINIT),
        }
    }

    #[cfg(test)]
    pub fn new(t: T) -> Self {
        Self {
            value: UnsafeCell::new(t),
            state: AtomicUsize::new(Self::UNINIT),
        }
    }
}

impl<T> Slot<MaybeUninit<T>> {
    #[cfg(not(test))]
    pub const fn uninit() -> Self {
        Self::new(MaybeUninit::uninit())
    }

    #[cfg(test)]
    pub fn uninit() -> Self {
        Self::new(MaybeUninit::uninit())
    }
}

unsafe impl<T: Sync> Sync for Slot<T> {}
