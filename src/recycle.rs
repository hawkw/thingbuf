pub trait Recycle<T> {
    /// Returns a new instance of type `T`.
    fn new_element(&self) -> T;

    /// Resets `element` in place.
    ///
    /// Typically, this retains any previous allocations.
    fn recycle(&self, element: &mut T);
}

#[derive(Debug, Default)]
pub struct DefaultRecycle(());

#[derive(Debug, Default)]
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
    pub fn new(max: usize) -> Self {
        Self { max, min: 0 }
    }

    pub fn with_min(self, min: usize) -> Self {
        Self { min, ..self }
    }

    pub fn min(&self) -> usize {
        self.min
    }

    pub fn max(&self) -> usize {
        self.max
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
