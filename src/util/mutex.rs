#[cfg(not(any(feature = "std", all(loom, test))))]
pub(crate) use self::spin_impl::*;

#[cfg(any(not(feature = "std"), test))]
mod spin_impl;

feature! {
    #![all(feature = "std", not(all(loom, test)))]
    #[allow(unused_imports)]
    pub(crate) use parking_lot::{Mutex, MutexGuard, const_mutex};
}

feature! {
    #![all(loom, test)]
    mod loom_impl;
    pub(crate) use self::loom_impl::*;
}
