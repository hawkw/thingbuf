use crate::loom::atomic::{AtomicPointer, Ordering};

pub struct Clist<N> {
    head: AtomicPointer<N>,
}

pub struct ClistNode<N> {
    next: AtomicPointer<Self>,
    prev: AtomicPointer<Self>,
    val: N,
}

impl<N> Clist<N> {
    pub fn push_front(&self, node: &mut ClistNode<N>) {
        let mut head = self.head.load(Ordering::Acquire);
    }
}
