pub(crate) use self::inner::*;
pub(crate) use core::panic::*;

#[cfg(feature = "std")]
mod inner {
    pub(crate) fn panicking() -> bool {
        std::thread::panicking()
    }

    pub use std::panic::{catch_unwind, resume_unwind};
}

#[cfg(not(feature = "std"))]
mod inner {
    use super::*;
    pub(crate) fn panicking() -> bool {
        false
    }

    pub(crate) fn catch_unwind<F: FnOnce() -> R + UnwindSafe, R>(f: F) -> Result<R, ()> {
        Ok(f())
    }

    pub(crate) fn resume_unwind(_: ()) -> ! {
        unreachable_unchecked!("code compiled with no_std cannot unwind!")
    }
}
