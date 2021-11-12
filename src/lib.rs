use core::{
    fmt,
    ops::{Deref, DerefMut},
};

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

mod loom;

#[cfg(test)]
mod tests;

use crate::loom::{
    atomic::{AtomicUsize, Ordering},
    UnsafeCell,
};

#[cfg(feature = "alloc")]
mod stringbuf;

pub struct ThingBuf<T> {
    head: CachePadded<AtomicUsize>,
    tail: CachePadded<AtomicUsize>,
    gen: usize,
    gen_mask: usize,
    idx_mask: usize,
    slots: Box<[Slot<T>]>,
}

pub struct Ref<'slot, T> {
    slot: &'slot Slot<T>,
    new_state: usize,
}

#[derive(Debug)]
pub struct AtCapacity(usize);

struct Slot<T> {
    value: UnsafeCell<T>,
    state: AtomicUsize,
}

#[derive(Debug)]
struct Backoff(u8);

#[cfg_attr(any(target_arch = "x86_64", target_arch = "aarch64"), repr(align(128)))]
#[cfg_attr(
    not(any(target_arch = "x86_64", target_arch = "aarch64")),
    repr(align(64))
)]
#[derive(Clone, Copy, Default, Hash, PartialEq, Eq, Debug)]
struct CachePadded<T>(T);

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
            slots,
        }
    }

    #[inline]
    fn idx_gen(&self, val: usize) -> (usize, usize) {
        (val & self.idx_mask, val & self.gen_mask)
    }

    #[inline]
    fn next(&self, idx: usize, gen: usize) -> usize {
        // Are we still in the same generation?
        if idx + 1 < self.slots.len() {
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
    pub fn push_with<U>(&self, f: impl FnOnce(&mut T) -> U) -> Result<U, AtCapacity> {
        self.push_ref().map(|mut r| r.with_mut(f))
    }

    pub fn push_ref(&self) -> Result<Ref<'_, T>, AtCapacity> {
        let mut backoff = Backoff::new();
        let mut tail = self.tail.load(Ordering::Relaxed);

        loop {
            let (idx, gen) = self.idx_gen(tail);
            test_dbg!(idx);
            test_dbg!(gen);
            let slot = &self.slots[idx];
            let state = slot.state.load(Ordering::Acquire);

            if state == tail {
                // Move the tail index forward by 1.
                let next_tail = self.next(idx, gen);
                match self.tail.compare_exchange(
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
                if self.head.load(Ordering::Acquire).wrapping_add(self.gen) == tail {
                    return Err(AtCapacity(self.slots.len()));
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

        loop {
            test_dbg!(head);
            let (idx, gen) = self.idx_gen(head);
            test_dbg!(idx);
            test_dbg!(gen);
            let slot = &self.slots[idx];
            let state = slot.state.load(Ordering::Acquire);
            test_dbg!(state);

            // If the slot's state is ahead of the head index by one, we can pop
            // it.
            if test_dbg!(state == head + 1) {
                let next_head = self.next(idx, gen);
                match self.head.compare_exchange(
                    head,
                    next_head,
                    Ordering::AcqRel,
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

    pub fn capacity(&self) -> usize {
        self.slots.len()
    }
}

impl<T> fmt::Debug for ThingBuf<T> {
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

// === impl Backoff ===

impl Backoff {
    const MAX_SPINS: u8 = 6;
    const MAX_YIELDS: u8 = 10;
    #[inline]
    fn new() -> Self {
        Self(0)
    }

    #[inline]
    fn spin(&mut self) {
        for _ in 0..test_dbg!(1 << self.0.min(Self::MAX_SPINS)) {
            loom::hint::spin_loop();

            test_println!("spin_loop_hint");
        }

        if self.0 <= Self::MAX_SPINS {
            self.0 += 1;
        }
    }

    #[inline]
    fn spin_yield(&mut self) {
        if self.0 <= Self::MAX_SPINS || cfg!(not(any(feature = "std", test))) {
            for _ in 0..1 << self.0 {
                loom::hint::spin_loop();
                test_println!("spin_loop_hint");
            }
        }

        #[cfg(any(test, feature = "std"))]
        loom::thread::yield_now();

        if self.0 <= Self::MAX_YIELDS {
            self.0 += 1;
        }
    }
}

// === impl CachePadded ===

impl<T> Deref for CachePadded<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> DerefMut for CachePadded<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}
