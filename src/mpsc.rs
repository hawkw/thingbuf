/// Multi-producer, single-consumer channels using [`ThingBuf`].
///
/// The default MPSC channel returned by [`mpsc::channel`] is _asynchronous_:
/// receiving from the channel is an `async fn`, and the receiving task will
/// wait when there are no messages in the channel.
///
/// If the "std" feature flag is enabled, this module also provides a
/// synchronous channel, in the [`mpsc::sync`] module. The synchronous receiver
/// will instead wait for new messages by blocking the current thread.
/// Naturally, this requires the Rust standard library.
mod async_impl;
pub use self::async_impl::*;

/// A synchronous multi-producer, single-consumer channel.
#[cfg(feature = "std")]
pub mod sync;

#[derive(Debug)]
#[non_exhaustive]
pub enum TrySendError {
    AtCapacity(crate::AtCapacity),
    Closed(Closed),
}

#[derive(Debug)]
pub struct Closed(pub(crate) ());

#[cfg(test)]
mod tests;
