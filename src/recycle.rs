pub trait Recycle<T> {
    /// Returns a new instance of type `T`.
    fn new_element(&self) -> T;

    /// Resets `element` in place.
    ///
    /// Typically, this retains any previous allocations.
    fn recycle(&self, element: &mut T);
}

/// A [`Recycle`] implementation for any type implementing [`Default`] and
/// [`Clone`].
///
/// This [creates new elements] by calling using [`Default::default()`].
/// Existing elements are [recycled] by calling [`Clone::clone_from`] with the
/// default value.
///
/// # Allocation Reuse
///
/// [`Clone::clone_from`] is not *guaranteed* to reuse existing
/// allocations in place. For a number of common types in the standard library,
/// such as [`Box`], [`String`], [`Vec`], and collections based on [`Vec`] (such
/// as [`VecDeque`] and [`BinaryHeap`]), `clone_from` is overridden to reuse
/// existing allocations in place. However, other types may not override
/// `clone_from` in this way.
///
/// `DefaultRecycle` will always *work* for types that implement [`Default`] and
/// [`Clone`], but it cannot be guaranteed to always reuse allocations. For a
/// more restrictive [`Recycle`] implementation that _will_ always reuse
/// existing allocations, consider [`WithCapacity`].
///
/// [creates new elements]: DefaultRecycle::new_element
/// [recycled]: DefaultRecycle::recycle
#[derive(Debug, Default)]
pub struct DefaultRecycle(());

/// A [`Recycle`] implementation for types that provide `with_capacity`,
/// `clear`, and `shrink_to` methods.
///
/// This includes all array-based collections in the Rust standard library, such
/// as [`Vec`], [`String`], [`VecDeque`], and [`BinaryHeap`], as well as
/// [`HashMap`] and [`HashSet`].
///
/// # Usage
///
/// By default, this type will always [recycle] elements by clearing all values
/// in place, returning all allocated capacity. [New elements] are allocated
/// with capacity for 0 values; they will allocate when first used.
///
/// ```rust
/// use thingbuf::recycle::WithCapacity;
///
/// WithCapacity::new()
/// ```
///
///
///
///
/// # Implementations for Other Types
///
/// [`Recycle`] implementations may be added for similar data structures
/// implemented in other libraries. The [`min_capacity`] and
/// [`max_capacity`] methods expose the configured initial capacity and upper
/// bound.
///
/// As an example, a library that implements an array-based data structure with
/// `with_capacity`, `clear`, and `shrink_to` methods can implement [`Recycle`]
/// for `WithCapacity` like so:
///
/// ```
/// use thingbuf::recycle::{self, Recycle};
/// # use std::marker::PhantomData;
///
/// /// Some kind of exciting new heap-allocated collection.
/// pub struct MyCollection<T> {
///     // ...
///     # _p: PhantomData<T>,
/// }
///
/// impl<T> MyCollection<T> {
///     /// Returns a new `MyCollection` with enough capacity to hold
///     /// `capacity` elements without reallocationg.
///     pub fn with_capacity(capacity: usize) -> Self {
///         // ...
///         # unimplemented!()
///     }
///
///     /// Returns the current allocated capacity of this `MyCollection`.
///     pub fn capacity(&self) -> usize {
///         // ...
///         # unimplemented!()
///     }
///
///     /// Shrinks the capacity of the `MyCollection` with a lower bound.
///     ///
///     /// The capacity will remain at least as large as both the length
///     /// and the supplied value.
///     ///
///     /// If the current capacity is less than the lower limit, this is a no-op.
///     pub fn shrink_to(&mut self, min_capacity: usize) {
///         if self.capacity() > min_capacity {
///             // ...
///             # unimplemented!()
///         }
///     }
///
///     /// Clears the `MyCollection`, removing all values.
///     ///
///     /// This does not change the current allocated capacity. The
///     /// `MyCollection` will still have enough allocated storage to hold
///     /// at least the current number of values.
///     pub fn clear(&mut self) {
///         // ...
///         # unimplemented!()
///     }
///
///     // Other cool and exciting methods go here!
/// }
///
/// // Because `MyCollection<T>` has `with_capacity`, `shrink_to`, and `clear` methods,
/// // we can implement `Recycle<MyCollection<T>>` for `WithCapacity` exactly the same
/// // way as it is implemented for standard library collections.
/// impl<T> Recycle<MyCollection<T>> for recycle::WithCapacity {
///     fn new_element(&self) -> MyCollection<T> {
///         // Allocate a new element with the minimum initial capacity:
///         MyCollection::with_capacity(self.min_capacity())
///     }
///
///     fn recycle(&self, element: &mut MyCollection<T>) {
///         // Recycle the element by clearing it in place, and then limiting the
///         // allocated capacity to the upper bound, if one is set:
///         element.clear();
///         element.shrink_to(self.max_capacity());
///     }
/// }
/// ```
///
/// # Allocation Reuse
///
/// When an upper bound is not set, this
#[derive(Debug)]
pub struct WithCapacity {
    min: usize,
    max: usize,
}

impl<T> Recycle<T> for DefaultRecycle
where
    T: Default + Clone,
{
    fn new_element(&self) -> T {
        T::default()
    }

    fn recycle(&self, element: &mut T) {
        element.clone_from(&T::default())
    }
}

// === impl WithCapacity ===

impl WithCapacity {
    /// Returns a new [`WithCapacity`].
    ///
    /// By default, the maximum capacity is unconstrained, and the minimum
    /// capacity is 0. Existing allocations will always be reused, regardless
    /// of size, and new elements will be created with 0 capacity.
    ///
    /// To add an upper bound on re-used capacity, use
    /// [`WithCapacity::with_max_capacity`]. To allocate elements with an
    /// initial capacity, use [`WithCapacity::with_min_capacity`].
    pub const fn new() -> Self {
        Self {
            max: core::usize::MAX,
            min: 0,
        }
    }

    pub const fn with_max_capacity(self, max: usize) -> Self {
        Self { max, ..self }
    }

    pub const fn with_min_capacity(self, min: usize) -> Self {
        Self { min, ..self }
    }

    /// Returns the minimum allocated capacity.
    ///
    /// [New elements] will be allocated with this capacity.
    pub fn min_capacity(&self) -> usize {
        self.min
    }

    pub fn max_capacity(&self) -> usize {
        self.max
    }
}

impl Default for WithCapacity {
    fn default() -> Self {
        Self::new()
    }
}

feature! {
    #![feature = "alloc"]
    use alloc::{vec::Vec, string::String, collections::{VecDeque, BinaryHeap}};

    impl<T> Recycle<Vec<T>> for WithCapacity {
        fn new_element(&self) -> Vec<T> {
            Vec::with_capacity(self.min)
        }

        fn recycle(&self, element: &mut Vec<T>) {
            element.clear();
            element.shrink_to(self.max);
        }
    }

    impl Recycle<String> for WithCapacity {
        fn new_element(&self) -> String {
            String::with_capacity(self.min)
        }

        fn recycle(&self, element: &mut String) {
            element.clear();
            element.shrink_to(self.max);
        }
    }

    impl<T> Recycle<VecDeque<T>> for WithCapacity {
        fn new_element(&self) -> VecDeque<T> {
            VecDeque::with_capacity(self.min)
        }

        fn recycle(&self, element: &mut VecDeque<T>) {
            element.clear();
            element.shrink_to(self.max);
        }
    }

    impl<T: core::cmp::Ord> Recycle<BinaryHeap<T>> for WithCapacity {
        fn new_element(&self) -> BinaryHeap<T> {
            BinaryHeap::with_capacity(self.min)
        }

        fn recycle(&self, element: &mut BinaryHeap<T>) {
            element.clear();
            element.shrink_to(self.max);
        }
    }
}

feature! {
    #![feature = "std"]
    use std::{hash::{Hash, BuildHasher}, collections::{HashMap, HashSet}};

    impl<K, V, S> Recycle<HashMap<K, V, S>> for WithCapacity
    where
        K: Hash + Eq,
        S: BuildHasher + Default
    {
        fn new_element(&self) -> HashMap<K, V, S> {
            HashMap::with_capacity_and_hasher(self.min, Default::default())
        }

        fn recycle(&self, element: &mut HashMap<K, V, S>) {
            element.clear();
            element.shrink_to(self.max);
        }
    }

    impl<K, S> Recycle<HashSet<K, S>> for WithCapacity
    where
        K: Hash + Eq,
        S: BuildHasher + Default
    {
        fn new_element(&self) -> HashSet<K, S> {
            HashSet::with_capacity_and_hasher(self.min, Default::default())
        }

        fn recycle(&self, element: &mut HashSet<K, S>) {
            element.clear();
            element.shrink_to(self.max);
        }
    }
}
