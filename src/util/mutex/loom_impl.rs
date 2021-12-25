pub(crate) use crate::loom::sync::MutexGuard;

use std::sync::PoisonError;

#[derive(Debug)]
pub(crate) struct Mutex<T>(crate::loom::sync::Mutex<T>);

impl<T> Mutex<T> {
    pub(crate) fn new(data: T) -> Self {
        Self(crate::loom::sync::Mutex::new(data))
    }

    #[inline]
    pub(crate) fn lock(&self) -> MutexGuard<'_, T> {
        test_println!("locking {}...", core::any::type_name::<T>());
        let lock = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        test_println!("-> locked {}!", core::any::type_name::<T>());
        lock
    }
}
