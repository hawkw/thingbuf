use crate::loom;
use core::{
    fmt,
    ops::{Deref, DerefMut},
};

pub(crate) mod panic;
pub(crate) mod wait;

#[derive(Debug)]
pub(crate) struct Backoff(u8);

#[cfg_attr(any(target_arch = "x86_64", target_arch = "aarch64"), repr(align(128)))]
#[cfg_attr(
    not(any(target_arch = "x86_64", target_arch = "aarch64")),
    repr(align(64))
)]
#[derive(Clone, Copy, Default, Hash, PartialEq, Eq)]
pub(crate) struct CachePadded<T>(pub(crate) T);

// === impl Backoff ===

impl Backoff {
    const MAX_SPINS: u8 = 6;
    const MAX_YIELDS: u8 = 10;
    #[inline]
    pub(crate) fn new() -> Self {
        Self(0)
    }

    #[inline]
    pub(crate) fn spin(&mut self) {
        for _ in 0..test_dbg!(1 << self.0.min(Self::MAX_SPINS)) {
            loom::hint::spin_loop();
        }

        if self.0 <= Self::MAX_SPINS {
            self.0 += 1;
        }
    }

    #[inline]
    pub(crate) fn spin_yield(&mut self) {
        if self.0 <= Self::MAX_SPINS || cfg!(not(any(feature = "std", test))) {
            for _ in 0..1 << self.0 {
                loom::hint::spin_loop();
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

impl<T: fmt::Debug> fmt::Debug for CachePadded<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
