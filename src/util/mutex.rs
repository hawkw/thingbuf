feature! {
    #![all(feature = "std", not(feature = "parking_lot"))]
    pub(crate) use self::std_impl::*;
    mod std_impl;
}

#[cfg(not(feature = "std"))]
pub(crate) use self::spin_impl::*;

#[cfg(any(not(feature = "std"), test))]
mod spin_impl;

feature! {
    #![all(feature = "std", feature = "parking_lot")]
    #[allow(unused_imports)]
    pub(crate) use parking_lot::{Mutex, MutexGuard};
}
