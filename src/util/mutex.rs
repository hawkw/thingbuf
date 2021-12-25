#[cfg(not(feature = "std"))]
pub(crate) use self::spin_impl::*;

#[cfg(any(not(feature = "std"), test))]
mod spin_impl;

feature! {
    #![feature = "std"]
    #[allow(unused_imports)]
    pub(crate) use parking_lot::{Mutex, MutexGuard, const_mutex};
}
