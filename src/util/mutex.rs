feature! {
    #![feature = "std"]
    pub(crate) use self::std_impl::*;
    mod std_impl;
}

#[cfg(not(feature = "std"))]
pub(crate) use self::spin_impl::*;

#[cfg(any(not(feature = "std"), test))]
mod spin_impl;
