use super::ThingBuf;
use crate::loom::{self, thread};
use std::sync::Arc;

#[test]
fn push_many_mpsc() {
    const T1_VALS: &[&str] = &["alice", "bob", "charlie"];
    const T2_VALS: &[&str] = &["dave", "ellen", "francis"];

    fn producer(vals: &'static [&'static str], q: &Arc<ThingBuf<String>>) -> impl FnOnce() {
        let q = q.clone();
        move || {
            for &val in vals {
                if let Ok(mut r) = test_dbg!(q.push_ref()) {
                    r.with_mut(|r| r.push_str(val));
                } else {
                    return;
                }
            }
        }
    }

    loom::model(|| {
        let q = Arc::new(ThingBuf::new(6));

        let t1 = thread::spawn(producer(T1_VALS, &q));
        let t2 = thread::spawn(producer(T2_VALS, &q));

        let mut all_vals = Vec::new();

        while Arc::strong_count(&q) > 1 {
            if let Some(r) = q.pop_ref() {
                r.with(|val| all_vals.push(val.to_string()));
            }
            thread::yield_now();
        }

        t1.join().unwrap();
        t2.join().unwrap();

        while let Some(r) = test_dbg!(q.pop_ref()) {
            r.with(|val| all_vals.push(val.to_string()));
        }

        test_dbg!(&all_vals);
        for &val in T1_VALS.iter().chain(T2_VALS.iter()) {
            assert!(all_vals.contains(&test_dbg!(val).to_string()))
        }

        assert_eq!(all_vals.len(), T1_VALS.len() + T2_VALS.len());
    })
}

#[test]
#[ignore] // this takes about a million years to run
fn linearizable() {
    const THREADS: usize = 4;

    fn thread(i: usize, q: &Arc<ThingBuf<usize>>) -> impl FnOnce() {
        let q = q.clone();
        move || {
            while q
                .push_ref()
                .map(|mut r| r.with_mut(|val| *val = i))
                .is_err()
            {}

            if let Some(mut r) = q.pop_ref() {
                r.with_mut(|val| *val = 0);
            }
        }
    }
    loom::model(|| {
        let q = Arc::new(ThingBuf::new(THREADS));
        let t1 = thread::spawn(thread(1, &q));
        let t2 = thread::spawn(thread(2, &q));
        let t3 = thread::spawn(thread(3, &q));
        thread(4, &q)();
        t1.join().expect("thread 1 panicked!");
        t2.join().expect("thread 2 panicked!");
        t3.join().expect("thread 3 panicked!");
    })
}
