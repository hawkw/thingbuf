use super::*;
use crate::{
    loom::{
        atomic::{AtomicUsize, Ordering},
        sync::Arc,
        thread::{self, Thread},
    },
    wait::WaitCell,
};

#[cfg(test)]
mod tests;

pub fn sync_channel<T>(thingbuf: ThingBuf<T>) -> (Sender<T>, Receiver<T>) {
    let inner = Arc::new(Inner {
        thingbuf,
        rx_wait: WaitCell::new(),
        tx_count: AtomicUsize::new(1),
    });
    let tx = Sender {
        inner: inner.clone(),
    };
    let rx = Receiver { inner };
    (tx, rx)
}

pub struct Sender<T> {
    inner: Arc<Inner<T>>,
}

pub struct Receiver<T> {
    inner: Arc<Inner<T>>,
}

pub struct SendRef<'a, T> {
    inner: &'a Inner<T>,
    slot: Ref<'a, T>,
}

struct Inner<T> {
    thingbuf: ThingBuf<T>,
    rx_wait: WaitCell<Thread>,
    tx_count: AtomicUsize,
}

#[derive(Debug)]
#[non_exhaustive]
pub enum TrySendError {
    AtCapacity(AtCapacity),
    Closed,
}

// === impl Sender ===

impl<T: Default> Sender<T> {
    pub fn try_send_ref(&self) -> Result<SendRef<'_, T>, TrySendError> {
        self.inner
            .thingbuf
            .push_ref()
            .map(|slot| SendRef {
                inner: &*self.inner,
                slot,
            })
            .map_err(TrySendError::AtCapacity)
    }

    pub fn try_send(&self, val: T) -> Result<(), TrySendError> {
        self.inner
            .thingbuf
            .push_ref()
            .map_err(TrySendError::AtCapacity)?
            .with_mut(|slot| {
                *slot = val;
            });
        Ok(())
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        self.inner.tx_count.fetch_add(1, Ordering::Relaxed);
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        if self.inner.tx_count.fetch_sub(1, Ordering::Release) != 1 {
            return;
        }

        // if we are the last sender, synchronize
        let _ = self.inner.tx_count.load(Ordering::Acquire);
        self.inner.rx_wait.notify();
    }
}

// === impl Receiver ===

impl<T: Default> Receiver<T> {
    pub fn recv_ref(&self) -> Option<Ref<'_, T>> {
        loop {
            // If we got a value, return it!
            if let Some(r) = self.inner.thingbuf.pop_ref() {
                return Some(r);
            }

            // Are there live senders? If not, the channel is closed...
            if self.is_closed() {
                return None;
            }

            // otherwise, gotosleep
            if self.inner.rx_wait.wait_with(thread::current) {
                #[cfg(test)]
                {
                    test_println!("sleeping ({:?})", thread::current());
                    thread::yield_now();
                }
                #[cfg(not(test))]
                thread::park();
            }
        }
    }

    pub fn is_closed(&self) -> bool {
        self.inner.tx_count.load(Ordering::SeqCst) <= 1
    }
}

impl<'a, T: Default> Iterator for &'a Receiver<T> {
    type Item = Ref<'a, T>;

    fn next(&mut self) -> Option<Self::Item> {
        self.recv_ref()
    }
}

// === impl SendRef ===

impl<T> SendRef<'_, T> {
    #[inline]
    pub fn with<U>(&self, f: impl FnOnce(&T) -> U) -> U {
        self.slot.with(f)
    }

    #[inline]
    pub fn with_mut<U>(&mut self, f: impl FnOnce(&mut T) -> U) -> U {
        self.slot.with_mut(f)
    }
}

impl<T> Drop for SendRef<'_, T> {
    #[inline]
    fn drop(&mut self) {
        test_println!("drop SendRef");
        self.inner.rx_wait.notify();
    }
}

impl<T: fmt::Debug> fmt::Debug for SendRef<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.with(|val| fmt::Debug::fmt(val, f))
    }
}

impl<T: fmt::Display> fmt::Display for SendRef<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.with(|val| fmt::Display::fmt(val, f))
    }
}

impl<T: fmt::Write> fmt::Write for SendRef<'_, T> {
    #[inline]
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.with_mut(|val| val.write_str(s))
    }

    #[inline]
    fn write_char(&mut self, c: char) -> fmt::Result {
        self.with_mut(|val| val.write_char(c))
    }

    #[inline]
    fn write_fmt(&mut self, f: fmt::Arguments<'_>) -> fmt::Result {
        self.with_mut(|val| val.write_fmt(f))
    }
}
