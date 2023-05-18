#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]
#![doc = include_str!("../README.md")]
#![warn(missing_docs)]
use core::{cmp, fmt, mem::MaybeUninit, ops, ptr};

#[macro_use]
mod macros;

mod loom;
pub mod mpsc;
pub mod recycling;
mod util;
mod wait;

pub use self::recycling::Recycle;

// TODO(eliza): finish writing this
// #[doc = include_str!("../mpsc_perf_comparison.md")]
// pub mod mpsc_perf_comparison {
//     // Empty module, used only for documentation.
// }

feature! {
    #![all(feature = "static", not(all(loom, test)))]
    mod static_thingbuf;
    pub use self::static_thingbuf::StaticThingBuf;
}

feature! {
    #![feature = "alloc"]
    extern crate alloc;

    mod thingbuf;
    pub use self::thingbuf::ThingBuf;
}

use crate::{
    loom::{
        atomic::{AtomicUsize, Ordering::*},
        cell::{MutPtr, UnsafeCell},
    },
    mpsc::errors::{TryRecvError, TrySendError},
    util::{Backoff, CachePadded},
};

const HAS_READER: usize = 1 << (usize::BITS - 1);

/// Maximum capacity of a `ThingBuf`. This is the largest number of elements that
/// can be stored in a `ThingBuf`. This is the highest power of two that can be expressed by a
/// `usize`, excluding the most significant bit reserved for the "has reader" flag.
pub const MAX_CAPACITY: usize = usize::MAX & !HAS_READER;

/// A reference to an entry in a [`ThingBuf`].
///
/// A `Ref` represents the exclusive permission to mutate a given element in a
/// queue. A `Ref<T>` [implements `DerefMut<T>`] to allow writing to that
/// element.
///
/// `Ref`s are returned by the [`ThingBuf::push_ref`] and [`ThingBuf::pop_ref`]
/// methods. When the `Ref` is dropped, the exclusive write access to that
/// element is released, and the push or pop operation is completed &mdash;
/// calling `push_ref` or `pop_ref` *begins* a push or pop operation, which ends
/// when the returned `Ref` is complete. When the `Ref` is dropped, the pushed
/// element will become available to a subsequent `pop_ref`, or the popped
/// element will be able to be written to by a `push_ref`, respectively.
///
/// [implements `DerefMut<T>`]: #impl-DerefMut
pub struct Ref<'slot, T> {
    ptr: MutPtr<MaybeUninit<T>>,
    slot: &'slot Slot<T>,
    new_state: usize,
    is_pop: bool,
}

/// Error indicating that a `push` operation failed because a queue was at
/// capacity.
///
/// This is returned by the [`ThingBuf::push`] and [`ThingBuf::push_ref`] (and
/// [`StaticThingBuf::push`]/[`StaticThingBuf::push_ref`]) methods.
#[derive(PartialEq, Eq)]
pub struct Full<T = ()>(T);

/// State variables for the atomic ring buffer algorithm.
///
/// This is separated from the actual storage array used to implement the ring
/// buffer, so that it can be used by both the dynamically-sized implementation
/// (`Box<[Slot<T>>]`) and the statically-sized (`[T; const usize]`)
/// implementation.
///
/// A `Core`, when provided with a reference to the storage array, knows how to
/// actually perform the ring buffer operations on that array.
///
/// The atomic ring buffer is based on the [MPMC bounded queue from 1024cores][1].
///
/// [1]: https://www.1024cores.net/home/lock-free-algorithms/queues/bounded-mpmc-queue
#[derive(Debug)]
struct Core {
    head: CachePadded<AtomicUsize>,
    tail: CachePadded<AtomicUsize>,
    gen: usize,
    gen_mask: usize,
    idx_mask: usize,
    closed: usize,
    capacity: usize,
    /// Set when dropping the slots in the ring buffer, to avoid potential double-frees.
    has_dropped_slots: bool,
}

struct Slot<T> {
    value: UnsafeCell<MaybeUninit<T>>,
    /// Each slot's state has two components: a flag indicated by the most significant bit (MSB), and the rest of the state.
    /// The MSB is set when a reader is reading from this slot.
    /// The rest of the state helps determine the availability of the slot for reading or writing:
    /// - A slot is available for reading when the state (excluding the MSB) equals head + 1.
    /// - A slot is available for writing when the state (excluding the MSB) equals tail.
    /// At initialization, each slot's state is set to its ordinal index.
    state: AtomicUsize,
}

impl Core {
    #[cfg(not(all(loom, test)))]
    const fn new(capacity: usize) -> Self {
        assert!(capacity <= MAX_CAPACITY);
        let closed = (capacity + 1).next_power_of_two();
        let idx_mask = closed - 1;
        let gen = closed << 1;
        let gen_mask = !(closed | idx_mask);
        Self {
            head: CachePadded(AtomicUsize::new(0)),
            tail: CachePadded(AtomicUsize::new(0)),
            gen,
            gen_mask,
            closed,
            idx_mask,
            capacity,
            has_dropped_slots: false,
        }
    }

    #[cfg(all(loom, test))]
    fn new(capacity: usize) -> Self {
        let closed = (capacity + 1).next_power_of_two();
        let idx_mask = closed - 1;
        let gen = closed << 1;
        let gen_mask = !(closed | idx_mask);
        Self {
            head: CachePadded(AtomicUsize::new(0)),
            tail: CachePadded(AtomicUsize::new(0)),
            gen,
            closed,
            gen_mask,
            idx_mask,
            capacity,
            #[cfg(debug_assertions)]
            has_dropped_slots: false,
        }
    }

    #[inline(always)]
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
            wrapping_add(gen, self.gen)
        }
    }

    #[inline]
    fn capacity(&self) -> usize {
        self.capacity
    }

    fn close(&self) -> bool {
        test_println!("Core::close");
        if crate::util::panic::panicking() {
            return false;
        }
        test_dbg!(self.tail.fetch_or(self.closed, SeqCst) & self.closed == 0)
    }

    #[inline(always)]
    fn push_ref<'slots, T, R>(
        &self,
        slots: &'slots [Slot<T>],
        recycle: &R,
    ) -> Result<Ref<'slots, T>, TrySendError<()>>
    where
        R: Recycle<T>,
    {
        test_println!("push_ref");
        let mut backoff = Backoff::new();
        let mut tail = test_dbg!(self.tail.load(Relaxed));
        loop {
            if test_dbg!(tail & self.closed != 0) {
                return Err(TrySendError::Closed(()));
            }
            let (idx, gen) = self.idx_gen(tail);
            test_dbg!(idx);
            test_dbg!(gen);
            let slot = unsafe {
                // Safety: `get_unchecked` does not check that the accessed
                // index was within bounds. However, `idx` is produced by
                // masking the current tail index to extract only the part
                // that's within the array's bounds.
                debug_assert!(
                    idx < slots.len(),
                    "index out of bounds (index was {} but the length was {})\n\n\
                    /!\\ EXTREMELY SERIOUS WARNING /!\\: in release mode, this \
                    access would not have been bounds checked, resulting in \
                    undefined behavior!\nthis is a bug in `thingbuf`! please \
                    report an issue immediately!",
                    idx,
                    slots.len()
                );
                slots.get_unchecked(idx)
            };
            let raw_state = test_dbg!(slot.state.load(SeqCst));
            let state = test_dbg!(clear_has_reader(raw_state));
            // slot is writable
            if test_dbg!(state == tail) {
                let next_tail = self.next(idx, gen);
                // try to advance the tail
                match test_dbg!(self
                    .tail
                    .compare_exchange_weak(tail, next_tail, SeqCst, Acquire))
                {
                    Ok(_) if test_dbg!(check_has_reader(raw_state)) => {
                        test_println!(
                            "advanced tail {} to {}; has an active reader, skipping slot [{}]",
                            tail,
                            next_tail,
                            idx
                        );
                        let next_state = wrapping_add(tail, self.gen);
                        test_dbg!(slot
                            .state
                            .fetch_update(SeqCst, SeqCst, |state| {
                                Some(state & HAS_READER | next_state)
                            })
                            .unwrap_or_else(|_| unreachable!()));
                        backoff.spin();
                        continue;
                    }
                    Ok(_) => {
                        // We got the slot! It's now okay to write to it
                        test_println!(
                            "advanced tail {} to {}; claimed slot [{}]",
                            tail,
                            next_tail,
                            idx
                        );
                        // Claim exclusive ownership over the slot
                        let ptr = slot.value.get_mut();
                        // Initialize or recycle the element.
                        unsafe {
                            // Safety: we have just claimed exclusive ownership over
                            // this slot.
                            let ptr = ptr.deref();
                            if gen == 0 {
                                ptr.write(recycle.new_element());
                                test_println!("-> initialized");
                            } else {
                                // Safety: if the generation is > 0, then the
                                // slot has already been initialized.
                                recycle.recycle(ptr.assume_init_mut());
                                test_println!("-> recycled");
                            }
                        }
                        return Ok(Ref {
                            ptr,
                            new_state: tail + 1,
                            slot,
                            is_pop: false,
                        });
                    }
                    Err(actual) => {
                        // Someone else took this slot and advanced the tail
                        // index. Try to claim the new tail.
                        test_println!("failed to advance tail {} to {}", tail, next_tail);
                        tail = actual;
                        backoff.spin();
                        continue;
                    }
                }
            }

            // check if we have any available slots
            // fake RMW op to placate loom. this should be equivalent to
            // doing a relaxed load after a SeqCst fence (per Godbolt
            // https://godbolt.org/z/zb15qfEa9), however, loom understands
            // this correctly, while it does not understand an explicit
            // SeqCst fence and a load.
            // XXX(eliza): this makes me DEEPLY UNCOMFORTABLE but if it's a
            // load it gets reordered differently in the model checker lmao...
            let head = test_dbg!(self.head.fetch_or(0, SeqCst));
            if test_dbg!(wrapping_add(head, self.gen) == tail) {
                test_println!("channel full");
                return Err(TrySendError::Full(()));
            }

            backoff.spin_yield();
            tail = test_dbg!(self.tail.load(Acquire));
        }
    }

    #[inline(always)]
    fn pop_ref<'slots, T>(&self, slots: &'slots [Slot<T>]) -> Result<Ref<'slots, T>, TryRecvError> {
        test_println!("pop_ref");
        let mut backoff = Backoff::new();
        let mut head = self.head.load(Relaxed);

        loop {
            test_dbg!(head);
            let (idx, gen) = self.idx_gen(head);
            test_dbg!(idx);
            test_dbg!(gen);
            let slot = unsafe {
                // Safety: `get_unchecked` does not check that the accessed
                // index was within bounds. However, `idx` is produced by
                // masking the current head index to extract only the part
                // that's within the array's bounds.
                debug_assert!(
                    idx < slots.len(),
                    "index out of bounds (index was {} but the length was {})\n\n\
                    /!\\ EXTREMELY SERIOUS WARNING /!\\: in release mode, this \
                    access would not have been bounds checked, resulting in \
                    undefined behavior!\nthis is a bug in `thingbuf`! please \
                    report an issue immediately!",
                    idx,
                    slots.len()
                );
                slots.get_unchecked(idx)
            };

            let raw_state = test_dbg!(slot.state.load(Acquire));
            let next_head = self.next(idx, gen);

            // If the slot's state is ahead of the head index by one, we can pop it.
            if test_dbg!(raw_state == head + 1) {
                // try to advance the head index
                match test_dbg!(self
                    .head
                    .compare_exchange_weak(head, next_head, SeqCst, Acquire))
                {
                    Ok(_) => {
                        test_println!("advanced head {} to {}", head, next_head);
                        test_println!("claimed slot [{}]", idx);
                        let mut new_state = wrapping_add(head, self.gen);
                        new_state = set_has_reader(new_state);
                        test_dbg!(slot.state.store(test_dbg!(new_state), SeqCst));
                        return Ok(Ref {
                            new_state,
                            ptr: slot.value.get_mut(),
                            slot,
                            is_pop: true,
                        });
                    }
                    Err(actual) => {
                        test_println!("failed to advance head, head={}, actual={}", head, actual);
                        head = actual;
                        backoff.spin();
                        continue;
                    }
                }
            } else {
                // Maybe we reached the tail index? If so, the buffer is empty.
                // fake RMW op to placate loom. this should be equivalent to
                // doing a relaxed load after a SeqCst fence (per Godbolt
                // https://godbolt.org/z/zb15qfEa9), however, loom understands
                // this correctly, while it does not understand an explicit
                // SeqCst fence and a load.
                // XXX(eliza): this makes me DEEPLY UNCOMFORTABLE but if it's a
                // load it gets reordered differently in the model checker lmao...
                let tail = test_dbg!(self.tail.fetch_or(0, SeqCst));
                if test_dbg!(tail & !self.closed == head) {
                    return if test_dbg!(tail & self.closed != 0) {
                        Err(TryRecvError::Closed)
                    } else {
                        test_println!("--> channel empty!");
                        Err(TryRecvError::Empty)
                    };
                }

                // Is anyone writing to the slot from this generation?
                if test_dbg!(raw_state == head) {
                    if test_dbg!(backoff.done_spinning()) {
                        return Err(TryRecvError::Empty);
                    }
                    backoff.spin();
                    continue;
                }

                // The slot is in an invalid state (was skipped). Try to advance the head index.
                match test_dbg!(self.head.compare_exchange(head, next_head, SeqCst, Acquire)) {
                    Ok(_) => {
                        test_println!("skipped head slot [{}], new head={}", idx, next_head);
                    }
                    Err(actual) => {
                        test_println!(
                            "failed to skip head slot [{}], head={}, actual={}",
                            idx,
                            head,
                            actual
                        );
                        head = actual;
                        backoff.spin();
                    }
                }
            }
        }
    }

    fn len(&self) -> usize {
        loop {
            let tail = self.tail.load(SeqCst);
            let head = self.head.load(SeqCst);

            if self.tail.load(SeqCst) == tail {
                let (head_idx, _) = self.idx_gen(head);
                let (tail_idx, _) = self.idx_gen(tail);
                return match head_idx.cmp(&tail_idx) {
                    cmp::Ordering::Less => tail_idx - head_idx,
                    cmp::Ordering::Greater => self.capacity - head_idx + tail_idx,
                    _ if tail == head => 0,
                    _ => self.capacity,
                };
            }
        }
    }

    fn drop_slots<T>(&mut self, slots: &mut [Slot<T>]) {
        debug_assert!(
            !self.has_dropped_slots,
            "tried to drop slots twice! core={:#?}",
            self
        );
        if self.has_dropped_slots {
            return;
        }

        let tail = self.tail.load(SeqCst);
        let (idx, gen) = self.idx_gen(tail);
        let num_initialized = if gen > 0 { self.capacity() } else { idx };
        for slot in &mut slots[..num_initialized] {
            unsafe {
                slot.value
                    .with_mut(|value| ptr::drop_in_place((*value).as_mut_ptr()));
            }
        }

        self.has_dropped_slots = true;
    }
}

#[inline]
fn check_has_reader(state: usize) -> bool {
    state & HAS_READER == HAS_READER
}

#[inline]
fn set_has_reader(state: usize) -> usize {
    state | HAS_READER
}

#[inline]
fn clear_has_reader(state: usize) -> usize {
    state & !HAS_READER
}

#[inline]
fn wrapping_add(a: usize, b: usize) -> usize {
    (a + b) & MAX_CAPACITY
}

impl Drop for Core {
    fn drop(&mut self) {
        debug_assert!(
            self.has_dropped_slots,
            "tried to drop Core without dropping slots! core={:#?}",
            self
        );
    }
}

// === impl Ref ===

impl<T> Ref<'_, T> {
    #[inline]
    fn with<U>(&self, f: impl FnOnce(&T) -> U) -> U {
        self.ptr.with(|value| unsafe {
            // Safety: if a `Ref` exists, we have exclusive ownership of the
            // slot. A `Ref` is only created if the slot has already been
            // initialized.
            // TODO(eliza): use `MaybeUninit::assume_init_ref` here once it's
            // supported by `tracing-appender`'s MSRV.
            f(&*(*value).as_ptr())
        })
    }

    #[inline]
    fn with_mut<U>(&mut self, f: impl FnOnce(&mut T) -> U) -> U {
        self.ptr.with(|value| unsafe {
            // Safety: if a `Ref` exists, we have exclusive ownership of the
            // slot.
            // TODO(eliza): use `MaybeUninit::assume_init_mut` here once it's
            // supported by `tracing-appender`'s MSRV.
            f(&mut *(*value).as_mut_ptr())
        })
    }
}

impl<T> ops::Deref for Ref<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        unsafe {
            // Safety: if a `Ref` exists, we have exclusive ownership of the
            // slot. A `Ref` is only created if the slot has already been
            // initialized.
            &*self.ptr.deref().as_ptr()
        }
    }
}

impl<T> ops::DerefMut for Ref<'_, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe {
            // Safety: if a `Ref` exists, we have exclusive ownership of the
            // slot. A `Ref` is only created if the slot has already been
            // initialized.
            &mut *self.ptr.deref().as_mut_ptr()
        }
    }
}

impl<T> Drop for Ref<'_, T> {
    #[inline]
    fn drop(&mut self) {
        if self.is_pop {
            test_println!("drop Ref<{}> (pop)", core::any::type_name::<T>());
            test_dbg!(self.slot.state.fetch_and(!HAS_READER, SeqCst));
        } else {
            test_println!(
                "drop Ref<{}> (push), new_state = {}",
                core::any::type_name::<T>(),
                self.new_state
            );
            test_dbg!(self.slot.state.store(test_dbg!(self.new_state), Release));
        }
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

unsafe impl<T: Send> Send for Ref<'_, T> {}
unsafe impl<T: Send> Sync for Ref<'_, T> {}

// === impl Slot ===

impl<T> Slot<T> {
    #[cfg(feature = "alloc")]
    pub(crate) fn make_boxed_array(capacity: usize) -> alloc::boxed::Box<[Self]> {
        (0..capacity).map(|i| Slot::new(i)).collect()
    }

    feature! {
        #![all(feature = "static", not(all(loom, test)))]

        const EMPTY: Self = Self::new(usize::MAX);

        pub(crate) const fn make_static_array<const CAPACITY: usize>() -> [Self; CAPACITY] {
            let mut array = [Self::EMPTY; CAPACITY];
            let mut i = 0;
            while i < CAPACITY {
                array[i] = Self::new(i);
                i += 1;
            }

            array
        }
    }

    #[cfg(not(all(loom, test)))]
    const fn new(idx: usize) -> Self {
        Self {
            value: UnsafeCell::new(MaybeUninit::uninit()),
            state: AtomicUsize::new(idx),
        }
    }

    #[cfg(all(loom, test))]
    fn new(idx: usize) -> Self {
        Self {
            value: UnsafeCell::new(MaybeUninit::uninit()),
            state: AtomicUsize::new(idx),
        }
    }
}

unsafe impl<T: Sync> Sync for Slot<T> {}

// === impl Full ===

impl<T> Full<T> {
    /// Unwraps the inner `T` value held by this error.
    ///
    /// This method allows recovering the original message when sending to a
    /// channel has failed.
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> fmt::Debug for Full<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Full(..)")
    }
}

impl<T> fmt::Display for Full<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("queue at capacity")
    }
}

#[cfg(feature = "std")]
impl<T> std::error::Error for Full<T> {}
