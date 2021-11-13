use super::*;
use crate::loom::sync::{Condvar, Mutex};

pub struct Sender<T> {
    inner: Arc<Inner<T>>,
}

pub struct Receiver<T> {
    inner: Arc<Inner<T>>,
}

struct Inner<T> {
    lock: Mutex<bool>,
    cv: Condvar,
    buf: ThingBuf<T>,
}

impl<T> Receiver<T> {
    
}
