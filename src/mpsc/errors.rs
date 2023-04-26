//! Errors returned by channels.
use core::fmt;

/// Error returned by the [`Sender::try_send`] or [`Sender::try_send_ref`] (and
/// [`StaticSender::try_send`]/[`StaticSender::try_send_ref`]) methods.
///
/// [`Sender::try_send`]: super::Sender::try_send
/// [`Sender::try_send_ref`]: super::Sender::try_send_ref
/// [`StaticSender::try_send`]: super::StaticSender::try_send
/// [`StaticSender::try_send_ref`]: super::StaticSender::try_send_ref
#[non_exhaustive]
#[derive(PartialEq, Eq)]
pub enum TrySendError<T = ()> {
    /// The data could not be sent on the channel because the channel is
    /// currently full and sending would require waiting for capacity.
    Full(T),
    /// The data could not be sent because the [`Receiver`] half of the channel
    /// has been dropped.
    ///
    /// [`Receiver`]: super::Receiver
    Closed(T),
}

/// Error returned by the [`Receiver::recv_timeout`] and [`Receiver::recv_ref_timeout`] methods
/// (blocking only).
///
/// [`Receiver::recv_timeout`]: super::blocking::Receiver::recv_timeout
/// [`Receiver::recv_ref_timeout`]: super::blocking::Receiver::recv_ref_timeout
#[cfg(feature = "std")]
#[non_exhaustive]
#[derive(Debug, PartialEq, Eq)]
pub enum RecvTimeoutError {
    /// The timeout elapsed before data could be received.
    Timeout,
    /// The channel is closed.
    Closed,
}

/// Error returned by the [`Receiver::try_recv`] and [`Receiver::try_recv_ref`] methods.
///
/// [`Receiver::try_recv`]: super::Receiver::try_recv
/// [`Receiver::try_recv_ref`]: super::Receiver::try_recv_ref
#[non_exhaustive]
#[derive(Debug, PartialEq, Eq)]
pub enum TryRecvError {
    /// The channel is empty.
    Empty,
    /// The channel is closed.
    Closed,
}

/// Error returned by [`Sender::send`] or [`Sender::send_ref`] (and
/// [`StaticSender::send`]/[`StaticSender::send_ref`]), if the
/// [`Receiver`] half of the channel has been dropped.
///
/// [`Sender::send`]: super::Sender::send
/// [`Sender::send_ref`]: super::Sender::send_ref
/// [`StaticSender::send`]: super::StaticSender::send
/// [`StaticSender::send_ref`]: super::StaticSender::send_ref
/// [`Receiver`]: super::Receiver
#[derive(PartialEq, Eq)]
pub struct Closed<T = ()>(pub(crate) T);

// === impl Closed ===

impl<T> Closed<T> {
    /// Unwraps the inner `T` value held by this error.
    ///
    /// This method allows recovering the original message when sending to a
    /// channel has failed.
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> fmt::Debug for Closed<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Closed(..)")
    }
}

impl<T> fmt::Display for Closed<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("channel closed")
    }
}

#[cfg(feature = "std")]
impl<T> std::error::Error for Closed<T> {}

// === impl TrySendError ===

impl TrySendError {
    pub(crate) fn with_value<T>(self, value: T) -> TrySendError<T> {
        match self {
            Self::Full(()) => TrySendError::Full(value),
            Self::Closed(()) => TrySendError::Closed(value),
        }
    }
}

impl<T> TrySendError<T> {
    /// Returns `true` if this error was returned because the channel was at
    /// capacity.
    pub fn is_full(&self) -> bool {
        matches!(self, Self::Full(_))
    }

    /// Returns `true` if this error was returned because the channel has closed
    /// (e.g. the [`Receiver`] end has been dropped).
    ///
    /// If this returns `true`, no future [`try_send`] or [`send`] operation on
    /// this channel will succeed.
    ///
    /// [`Receiver`]: super::Receiver
    /// [`try_send`]: super::Sender::try_send
    /// [`send`]: super::Sender::send
    /// [`Receiver`]: super::Receiver
    pub fn is_closed(&self) -> bool {
        matches!(self, Self::Full(_))
    }

    /// Unwraps the inner `T` value held by this error.
    ///
    /// This method allows recovering the original message when sending to a
    /// channel has failed.
    pub fn into_inner(self) -> T {
        match self {
            Self::Full(val) => val,
            Self::Closed(val) => val,
        }
    }
}

impl<T> fmt::Debug for TrySendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Full(_) => "TrySendError::Full(..)",
            Self::Closed(_) => "TrySendError::Closed(..)",
        })
    }
}

impl<T> fmt::Display for TrySendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Full(_) => "no available capacity",
            Self::Closed(_) => "channel closed",
        })
    }
}

#[cfg(feature = "std")]
impl<T> std::error::Error for TrySendError<T> {}

// === impl RecvTimeoutError ===

#[cfg(feature = "std")]
impl std::error::Error for RecvTimeoutError {}

#[cfg(feature = "std")]
impl fmt::Display for RecvTimeoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Timeout => "timed out waiting on channel",
            Self::Closed => "channel closed",
        })
    }
}

// == impl TryRecvError ==

#[cfg(feature = "std")]
impl std::error::Error for TryRecvError {}

impl fmt::Display for TryRecvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "channel is empty",
            Self::Closed => "channel closed",
        })
    }
}
