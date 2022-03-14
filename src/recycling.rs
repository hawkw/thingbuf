//! Configurable policies for element reuse.

/// A policy defining how pooled elements of type `T` are reused.
///
/// A recycling policy provides two operations: [`Recycle::new_element`], which
/// defines how new elements of type `T` are *initially* constructed, and
/// [`Recycle::recycle`], which, given an `&mut T`, prepares that element for
/// reuse.
///
/// This trait is intended to allow defining custom policies to reuse values
/// that are expensive to create or destroy. If a type `T` owns a large memory
/// allocation or other reuseable resource (such as a file descriptor, network
/// connection, worker thread, et cetera), the [`recycle`](Self::recycle)
/// operation can clear any *data* associated with a particular use of that
/// value while retaining its memory allocation or other resources. For example,
/// the [`WithCapacity`] recycling policy clears standard library collections
/// such as `String` and `Vec` in place, retaining their allocated heap
/// capacity, so that future uses of those collections do not need to
/// reallocate.
pub trait Recycle<T> {
    /// Returns a new instance of type `T`.
    ///
    /// This method will be called to populate the pool with the initial set of
    /// elements. It may also be called if an element is permanently removed
    /// from the pool and will not be returned.
    fn new_element(&self) -> T;

    /// Prepares `element` element for reuse.
    ///
    /// Typically, this clears any data stored in `element` in place, but
    /// retains any previous heap allocations owned by `element` so that they
    /// can be used again.
    ///
    /// This method is called when a `T` value is returned to the pool that owns
    /// it.
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
#[derive(Clone, Debug, Default)]
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
/// use thingbuf::recycling::{self, Recycle};
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
/// impl<T> Recycle<MyCollection<T>> for recycling::WithCapacity {
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
/// When an upper bound is not set, this recycling policy will _always_ reuse
/// any allocated capacity when recycling an element. Over time, the number of
/// reallocations required to grow items in a pool should decrease, amortizing
/// reallocations over the lifetime of the program.
///
/// Of course, this means that it is technically possible for the allocated
/// capacity of the pool to grow infinitely, which can cause a memory leak if
/// used incorrectly. Therefore, it is also possible to set an upper bound on
/// idle capacity, using [`with_max_capacity`]. When such a bound is set,
/// recycled elements will be shrunk down to that capacity if they have grown
/// past the upper bound while in use. If this is the case, reallocations may
/// occur more often, but if the upper bound is higher than the typical required
/// capacity, they should remain infrequent.
///
/// If elements will not require allocations of differing sizes, and the size is
/// known in advance (e.g. a pool of `HashMap`s that always have exactly 64
/// elements), the [`with_max_capacity`] and [`with_min_capacity`] methods can
/// be called with the same value. This way, elements will always be initially
/// allocated with *exactly* that much capacity, and will only be shrunk if they
/// ever exceed that capacity. If the elements never grow beyond the specified
/// capacity, this should mean that no additional allocations will ever occur
/// once the initial pool of elements are allocated.
///
/// [recycle]: Recycle::recycle
/// [`max_capacity`]: Self::max_capacity
/// [`min_capacity`]: Self::min_capacity
/// [`with_max_capacity`]: Self::with_max_capacity
/// [`with_min_capacity`]: Self::with_min_capacity
#[derive(Clone, Debug)]
pub struct WithCapacity {
    min: usize,
    max: usize,
}

// TODO(eliza): consider making this public?
// TODO(eliza): consider making this a trait method with a default impl?
#[inline(always)]
pub(crate) fn take<R, T>(element: &mut T, recycle: &R) -> T
where
    R: Recycle<T>,
{
    core::mem::replace(element, recycle.new_element())
}

impl DefaultRecycle {
    /// Returns a new `DefaultRecycle`.
    pub const fn new() -> Self {
        Self(())
    }
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

    /// Sets an upper bound on the capacity that will be reused when [recycling]
    /// elements.
    ///
    /// When an element is recycled, if its capacity exceeds the max value, it
    /// will be shrunk down to that capacity. This will result in a
    /// reallocation, but limits the total capacity allocated by the pool,
    /// preventing unbounded memory use.
    ///
    /// Elements may still exceed the configured max capacity *while they are in
    /// use*; this value only configures what happens when they are returned to
    /// the pool.
    ///
    /// # Examples
    ///
    /// ```
    /// use thingbuf::recycling::{Recycle, WithCapacity};
    ///
    /// // Create a recycler with max capacity of 8.
    /// let recycle = WithCapacity::new().with_max_capacity(8);
    ///
    /// // Create a new string using that recycler.
    /// let mut s: String = recycle.new_element();
    /// assert_eq!(s.capacity(), 0);
    ///
    /// // Now, write some data to the string.
    /// s.push_str("hello, world");
    ///
    /// // The string's capacity must be at least the length of the
    /// // string 'hello, world'.
    /// assert!(s.capacity() >= "hello, world".len());
    ///
    /// // After recycling the string, its capacity will be shrunk down
    /// // to the configured max capacity.
    /// recycle.recycle(&mut s);
    /// assert_eq!(s.capacity(), 8);
    /// ```
    ///
    /// [recycling]: Recycle::recycle
    pub const fn with_max_capacity(self, max: usize) -> Self {
        Self { max, ..self }
    }

    /// Sets the minimum capacity when [allocating new elements][new].
    ///
    /// When new elements are created, they will be allocated with at least
    /// `min` capacity.
    ///
    /// Note that this is a *lower bound*. Elements may be allocated with
    /// greater than the minimum capacity, depending on the behavior of the
    /// element being allocated, but there will always be *at least* `min`
    /// capacity.
    ///
    /// # Examples
    ///
    /// ```
    /// use thingbuf::recycling::{Recycle, WithCapacity};
    ///
    /// // A recycler without a minimum capacity.
    /// let no_min = WithCapacity::new();
    ///
    /// // A new element created by this recycler will not
    /// // allocate any capacity until it is used.
    /// let s: String = no_min.new_element();
    /// assert_eq!(s.capacity(), 0);
    ///
    /// // Now, configure a minimum capacity.
    /// let with_min = WithCapacity::new().with_min_capacity(8);
    ///
    /// // New elements created by this recycler will always be allocated
    /// // with at least the specified capacity.
    /// let s: String = with_min.new_element();
    /// assert!(s.capacity() >= 8);
    /// ```
    ///
    /// [new]: Recycle::new_element
    pub const fn with_min_capacity(self, min: usize) -> Self {
        Self { min, ..self }
    }

    /// Returns the minimum initial capacity when [allocating new
    /// elements][new].
    ///
    /// This method can be used to implement `Recycle<T> for WithCapacity` where
    /// `T` is a type defined outside of this crate. See [the `WithCapacity`
    /// documentation][impling] for details.
    ///
    /// # Examples
    ///
    /// ```
    /// use thingbuf::recycling::{Recycle, WithCapacity};
    ///
    /// let recycle = WithCapacity::new();
    /// assert_eq!(recycle.min_capacity(), 0);
    /// ```
    ///
    /// ```
    /// use thingbuf::recycling::{Recycle, WithCapacity};
    ///
    /// let recycle = WithCapacity::new().with_min_capacity(64);
    /// assert_eq!(recycle.min_capacity(), 64);
    /// ```
    ///
    /// [new]: Recycle::new_element
    /// [impling]: WithCapacity#implementations-for-other-types
    pub fn min_capacity(&self) -> usize {
        self.min
    }

    /// Returns the maximum retained capacity when [recycling
    /// elements][recycle].
    ///
    /// If no upper bound is configured, this will return [`usize::MAX`].
    ///
    /// This method can be used to implement `Recycle<T> for WithCapacity` where
    /// `T` is a type defined outside of this crate. See [the `WithCapacity`
    /// documentation][impling] for details.
    ///
    /// # Examples
    ///
    /// ```
    /// use thingbuf::recycling::{Recycle, WithCapacity};
    ///
    /// let recycle = WithCapacity::new();
    /// assert_eq!(recycle.max_capacity(), usize::MAX);
    /// ```
    ///
    /// ```
    /// use thingbuf::recycling::{Recycle, WithCapacity};
    ///
    /// let recycle = WithCapacity::new().with_max_capacity(64);
    /// assert_eq!(recycle.max_capacity(), 64);
    /// ```
    ///
    /// [recycle]: Recycle::recycle
    /// [impling]: WithCapacity#implementations-for-other-types
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
    use alloc::{
        collections::{VecDeque, BinaryHeap},
        string::String,
        sync::Arc,
        vec::Vec,
    };

    impl<T, R> Recycle<T> for Arc<R>
    where
        R: Recycle<T>,
    {
        #[inline]
        fn new_element(&self) -> T {
            (**self).new_element()
        }

        #[inline]
        fn recycle(&self, element: &mut T) {
            (**self).recycle(element)
        }
    }

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
