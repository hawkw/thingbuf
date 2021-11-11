use super::ThingBuf;
use crate::loom::{self, thread};
use std::sync::Arc;

#[test]
fn linearizable() {
    const THREADS: usize = 4;

    fn thread(i: usize, q: Arc<ThingBuf<usize>>) {
        while q
            .push_ref()
            .map(|mut r| r.with_mut(|val| *val = i))
            .is_err()
        {}

        if let Some(mut r) = q.pop_ref() {
            r.with_mut(|val| *val = 0);
        }
    }
    loom::model(|| {
        let q = Arc::new(ThingBuf::new(THREADS));
        let q1 = q.clone();
        let q2 = q.clone();
        let q3 = q.clone();
        let t1 = thread::spawn(move || thread(1, q1));
        let t2 = thread::spawn(move || thread(2, q2));
        let t3 = thread::spawn(move || thread(3, q3));
        thread(4, q);
        t1.join().expect("thread 1 panicked!");
        t2.join().expect("thread 2 panicked!");
        t3.join().expect("thread 3 panicked!");
    })
}
