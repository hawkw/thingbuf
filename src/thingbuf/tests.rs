use super::ThingBuf;
use crate::loom::{self, alloc, thread};
use std::sync::Arc;

#[test]
fn push_many_mpsc() {
    const T1_VALS: &[&str] = &["alice", "bob", "charlie"];
    const T2_VALS: &[&str] = &["dave", "ellen", "francis"];

    fn producer(
        vals: &'static [&'static str],
        q: &Arc<ThingBuf<alloc::Track<String>>>,
    ) -> impl FnOnce() {
        let q = q.clone();
        move || {
            for &val in vals {
                if let Ok(mut slot) = test_dbg!(q.push_ref()) {
                    slot.get_mut().push_str(val);
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
            if let Some(val) = q.pop_ref() {
                all_vals.push(val.get_ref().to_string());
            }
            thread::yield_now();
        }

        t1.join().unwrap();
        t2.join().unwrap();

        while let Some(val) = test_dbg!(q.pop_ref()) {
            all_vals.push(val.get_ref().to_string());
        }

        test_dbg!(&all_vals);
        for &val in T1_VALS.iter().chain(T2_VALS.iter()) {
            assert_dbg!(all_vals.contains(&test_dbg!(val).to_string()))
        }

        assert_eq!(all_vals.len(), T1_VALS.len() + T2_VALS.len());
    })
}

#[test]
fn spsc() {
    const COUNT: usize = 9;
    loom::model(|| {
        let q = Arc::new(ThingBuf::new(3));

        let producer = {
            let q = q.clone();
            thread::spawn(move || {
                for i in 0..COUNT {
                    loop {
                        if let Ok(mut val) = q.push_ref() {
                            *val = i;
                            break;
                        }
                        thread::yield_now();
                    }
                }
            })
        };

        for i in 0..COUNT {
            loop {
                if let Some(val) = q.pop_ref() {
                    assert_eq!(*val, i);
                    break;
                }
                thread::yield_now();
            }
        }

        producer.join().unwrap();
    });
}

#[test]
#[ignore] // this takes about a million years to run
fn linearizable() {
    const THREADS: usize = 4;

    fn thread(i: usize, q: &Arc<ThingBuf<usize>>) -> impl FnOnce() {
        let q = q.clone();
        move || {
            let mut pushed = false;
            while !pushed {
                pushed = q
                    .push_ref()
                    .map(|mut val| {
                        *val = i;
                    })
                    .is_ok();
            }

            if let Some(mut val) = q.pop_ref() {
                *val = 0;
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
