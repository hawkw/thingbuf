#[cfg(all(test, loom))]
use crate::loom::sync::Mutex as Inner;
#[cfg(all(test, loom))]
pub(crate) use crate::loom::sync::MutexGuard;

#[cfg(not(all(test, loom)))]
use std::sync::Mutex as Inner;

#[cfg(not(all(test, loom)))]
pub(crate) use std::sync::MutexGuard;

use std::sync::PoisonError;

#[derive(Debug)]
pub(crate) struct Mutex<T>(Inner<T>);

impl<T> Mutex<T> {
    pub(crate) fn new(data: T) -> Self {
        Self(Inner::new(data))
    }

    #[inline]
    pub(crate) fn lock(&self) -> MutexGuard<'_, T> {
        test_println!("locking {}...", core::any::type_name::<T>());
        let lock = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        test_println!("-> locked {}!", core::any::type_name::<T>());
        lock
    }
}
