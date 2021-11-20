#![cfg_attr(not(feature = "std"), no_std)]

use core::{fmt, mem::MaybeUninit, ops::Index};

#[macro_use]
mod macros;

pub mod error;
mod loom;
mod util;
mod wait;

feature! {
    #![feature = "alloc"]
    extern crate alloc;

    mod thingbuf;
    pub use self::thingbuf::ThingBuf;

    mod stringbuf;
    pub use stringbuf::{StaticStringBuf, StringBuf};
}

feature! {
    #![feature = "std"]
    pub mod sync_mpsc;
    #[doc(inline)]
    pub use sync_mpsc::sync_mpsc;
}

mod static_thingbuf;
pub use self::static_thingbuf::StaticThingBuf;

use crate::{
    error::AtCapacity,
    loom::{
        atomic::{AtomicUsize, Ordering},
        UnsafeCell,
    },
    util::{Backoff, CachePadded},
};

#[derive(Debug)]
struct Core {
    head: CachePadded<AtomicUsize>,
    tail: CachePadded<AtomicUsize>,
    gen: usize,
    gen_mask: usize,
    idx_mask: usize,
    capacity: usize,
}

pub struct Ref<'slot, T> {
    slot: &'slot Slot<T>,
    new_state: usize,
}

pub struct Slot<T> {
    value: UnsafeCell<MaybeUninit<T>>,
    state: AtomicUsize,
}

impl Core {
    #[cfg(not(test))]
    const fn new(capacity: usize) -> Self {
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
        }
    }

    #[cfg(test)]
    fn new(capacity: usize) -> Self {
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
        }
    }

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

    #[inline]
    fn capacity(&self) -> usize {
        self.capacity
    }

    fn push_ref<'slots, T, S>(&self, slots: &'slots S) -> Result<Ref<'slots, T>, AtCapacity>
    where
        T: Default,
        S: Index<usize, Output = Slot<T>> + ?Sized,
    {
        test_println!("push_ref");
        let mut backoff = Backoff::new();
        let mut tail = self.tail.load(Ordering::Relaxed);

        loop {
            let (idx, gen) = self.idx_gen(tail);
            test_dbg!(idx);
            test_dbg!(gen);
            let slot = &slots[idx];
            let state = slot.state.load(Ordering::Acquire);

            if state == tail || (state == 0 && gen == 0) {
                // Move the tail index forward by 1.
                let next_tail = self.next(idx, gen);
                match self.tail.compare_exchange_weak(
                    tail,
                    next_tail,
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        // We got the slot! It's now okay to write to it
                        test_println!("claimed tail slot");
                        if gen == 0 {
                            slot.value.with_mut(|value| unsafe {
                                // Safety: we have just claimed exclusive ownership over
                                // this slot.
                                (*value).write(T::default());
                            });
                            test_println!("-> initialized");
                        }

                        return Ok(Ref {
                            new_state: tail + 1,
                            slot,
                        });
                    }
                    Err(actual) => {
                        // Someone else took this slot and advanced the tail
                        // index. Try to claim the new tail.
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

    fn pop_ref<'slots, T, S>(&self, slots: &'slots S) -> Option<Ref<'slots, T>>
    where
        S: Index<usize, Output = Slot<T>> + ?Sized,
    {
        test_println!("pop_ref");
        let mut backoff = Backoff::new();
        let mut head = self.head.load(Ordering::Relaxed);

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
                        test_println!("claimed head slot");
                        return Some(Ref {
                            new_state: head.wrapping_add(self.gen),
                            slot,
                        });
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

    fn len(&self) -> usize {
        use std::cmp;
        loop {
            let tail = self.tail.load(Ordering::SeqCst);
            let head = self.head.load(Ordering::SeqCst);

            if self.tail.load(Ordering::SeqCst) == tail {
                let (head_idx, _) = self.idx_gen(head);
                let (tail_idx, _) = self.idx_gen(tail);
                return match head_idx.cmp(&tail_idx) {
                    cmp::Ordering::Less => head_idx - tail_idx,
                    cmp::Ordering::Greater => self.capacity - head_idx + tail_idx,
                    _ if tail == head => 0,
                    _ => self.capacity,
                };
            }
        }
    }
}

// === impl Ref ===

impl<T> Ref<'_, T> {
    #[inline]
    pub fn with<U>(&self, f: impl FnOnce(&T) -> U) -> U {
        self.slot.value.with(|value| unsafe {
            // Safety: if a `Ref` exists, we have exclusive ownership of the
            // slot. A `Ref` is only created if the slot has already been
            // initialized.
            // TODO(eliza): use `MaybeUninit::assume_init_ref` here once it's
            // supported by `tracing-appender`'s MSRV.
            f(&*(&*value).as_ptr())
        })
    }

    #[inline]
    pub fn with_mut<U>(&mut self, f: impl FnOnce(&mut T) -> U) -> U {
        self.slot.value.with_mut(|value| unsafe {
            // Safety: if a `Ref` exists, we have exclusive ownership of the
            // slot.
            // TODO(eliza): use `MaybeUninit::assume_init_mut` here once it's
            // supported by `tracing-appender`'s MSRV.
            f(&mut *(&mut *value).as_mut_ptr())
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

impl<T> Slot<T> {
    #[cfg(not(test))]
    const fn empty() -> Self {
        Self {
            value: UnsafeCell::new(MaybeUninit::uninit()),
            state: AtomicUsize::new(0),
        }
    }

    #[cfg(test)]
    fn empty() -> Self {
        Self {
            value: UnsafeCell::new(MaybeUninit::uninit()),
            state: AtomicUsize::new(0),
        }
    }
}

unsafe impl<T: Sync> Sync for Slot<T> {}
