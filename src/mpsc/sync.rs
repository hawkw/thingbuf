//! A synchronous multi-producer, single-consumer channel.
//!
//! This provides an equivalent API to the [`mpsc`](crate::mpsc) module, but the
//! [`Receiver`] type in this module waits by blocking the current thread,
//! rather than asynchronously yielding.
use super::*;
use crate::{
    loom::{
        atomic::{self, Ordering},
        sync::Arc,
        thread::{self, Thread},
    },
    Ref, ThingBuf,
};
use core::fmt;

/// Returns a new asynchronous multi-producer, single consumer channel.
pub fn channel<T>(thingbuf: ThingBuf<T>) -> (Sender<T>, Receiver<T>) {
    let inner = Arc::new(Inner::new(thingbuf));
    let tx = Sender {
        inner: inner.clone(),
    };
    let rx = Receiver { inner };
    (tx, rx)
}

#[derive(Debug)]
pub struct Sender<T> {
    inner: Arc<Inner<T, Thread>>,
}

#[derive(Debug)]
pub struct Receiver<T> {
    inner: Arc<Inner<T, Thread>>,
}

impl_send_ref! {
    pub struct SendRef<Thread>;
}

impl_recv_ref! {
    pub struct RecvRef<Thread>;
}

// === impl Sender ===

impl<T: Default> Sender<T> {
    pub fn try_send_ref(&self) -> Result<SendRef<'_, T>, TrySendError> {
        self.inner.try_send_ref().map(SendRef)
    }

    pub fn try_send(&self, val: T) -> Result<(), TrySendError<T>> {
        self.inner.try_send(val)
    }

    pub fn send_ref(&self) -> Result<SendRef<'_, T>, Closed> {
        loop {
            // perform one send ref loop iteration
            if let Poll::Ready(result) = self.inner.poll_send_ref(thread::current) {
                return result.map(SendRef);
            }

            // if that iteration failed, park the thread.
            thread::park();
        }
    }

    pub fn send(&self, val: T) -> Result<(), Closed<T>> {
        match self.send_ref() {
            Err(Closed(())) => Err(Closed(val)),
            Ok(mut slot) => {
                slot.with_mut(|slot| *slot = val);
                Ok(())
            }
        }
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        test_dbg!(self.inner.tx_count.fetch_add(1, Ordering::Relaxed));
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        if test_dbg!(self.inner.tx_count.fetch_sub(1, Ordering::Release)) > 1 {
            return;
        }

        // if we are the last sender, synchronize
        test_dbg!(atomic::fence(Ordering::SeqCst));
        if self.inner.thingbuf.core.close() {
            self.inner.rx_wait.close_tx();
        }
    }
}

// === impl Receiver ===

impl<T: Default> Receiver<T> {
    pub fn recv_ref(&self) -> Option<RecvRef<'_, T>> {
        loop {
            match self.inner.poll_recv_ref(thread::current) {
                Poll::Ready(r) => {
                    return r.map(|slot| RecvRef {
                        slot,
                        inner: &*self.inner,
                    })
                }
                Poll::Pending => {
                    test_println!("parking ({:?})", thread::current());
                    thread::park();
                }
            }
        }
    }

    pub fn try_recv_ref(&self) -> Option<Ref<'_, T>> {
        self.inner.thingbuf.pop_ref()
    }

    pub fn recv(&self) -> Option<T> {
        let val = self.recv_ref()?.with_mut(core::mem::take);
        Some(val)
    }

    pub fn is_closed(&self) -> bool {
        test_dbg!(self.inner.tx_count.load(Ordering::SeqCst)) <= 1
    }
}

impl<'a, T: Default> Iterator for &'a Receiver<T> {
    type Item = RecvRef<'a, T>;

    fn next(&mut self) -> Option<Self::Item> {
        self.recv_ref()
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.inner.close_rx();
    }
}
