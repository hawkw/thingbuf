use crate::Slot;

/// Trait implemented by values that can be used as a backing array for a
/// [`ThingBuf`].
///
/// # Safety
pub trait AsArray<T>: crate::util::Sealed {
    fn as_array(&self) -> &[Slot<T>];

    #[inline]
    fn len(&self) -> usize {
        self.as_array().len()
    }

    #[inline]
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T, const LEN: usize> crate::util::Sealed for [Slot<T>; LEN] {}
impl<T, const LEN: usize> AsArray<T> for [Slot<T>; LEN] {
    #[inline]
    fn as_array(&self) -> &[Slot<T>] {
        &self[..]
    }

    #[inline]
    fn len(&self) -> usize {
        LEN
    }
}

impl<T> crate::util::Sealed for &mut [Slot<T>] {}
impl<T> AsArray<T> for &mut [Slot<T>] {
    #[inline]
    fn as_array(&self) -> &[Slot<T>] {
        &self[..]
    }

    #[inline]
    fn len(&self) -> usize {
        <[Slot<T>]>::len(self)
    }
}

#[cfg(feature = "alloc")]
impl<T> crate::util::Sealed for alloc::boxed::Box<[Slot<T>]> {}

#[cfg(feature = "alloc")]
impl<T> AsArray<T> for alloc::boxed::Box<[Slot<T>]> {
    #[inline]
    fn as_array(&self) -> &[Slot<T>] {
        self.as_ref()
    }

    #[inline]
    fn len(&self) -> usize {
        <[Slot<T>]>::len(&*self)
    }
}

#[cfg(feature = "alloc")]
impl<T> crate::util::Sealed for alloc::vec::Vec<Slot<T>> {}

#[cfg(feature = "alloc")]
impl<T> AsArray<T> for alloc::vec::Vec<Slot<T>> {
    #[inline]
    fn as_array(&self) -> &[Slot<T>] {
        self.as_ref()
    }

    #[inline]
    fn len(&self) -> usize {
        alloc::vec::Vec::len(self)
    }
}
