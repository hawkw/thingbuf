//! Multi-producer, single-consumer channels using [`ThingBuf`](crate::ThingBuf).
//!
//! The default MPSC channel returned by the [`channel`] function is
//! _asynchronous_: receiving from the channel is an `async fn`, and the
//! receiving task willwait when there are no messages in the channel.
//!
//! If the "std" feature flag is enabled, this module also provides a
//! synchronous channel, in the [`sync`] module. The synchronous  receiver will
//! instead wait for new messages by blocking the current thread. Naturally,
//! this requires the Rust standard library. A synchronous channel
//! can be constructed using the [`sync::channel`] function.
mod async_impl;
pub use self::async_impl::*;

feature! {
    #![feature = "std"]
    pub mod sync;
}

#[derive(Debug)]
#[non_exhaustive]
pub enum TrySendError<T = ()> {
    Full(T),
    Closed(T),
}

impl TrySendError {
    fn with_value<T>(self, value: T) -> TrySendError<T> {
        match self {
            Self::Full(()) => TrySendError::Full(value),
            Self::Closed(()) => TrySendError::Closed(value),
        }
    }
}

#[cfg(test)]
mod tests;
