use super::*;
use crate::{
    loom::{self, thread},
    ThingBuf,
};

#[test]
fn basically_works() {
    loom::model(|| {
        let (tx, rx) = sync::channel(ThingBuf::new(4));

        let p1 = {
            let tx = tx.clone();
            thread::spawn(move || {
                tx.try_send_ref().unwrap().with_mut(|val| *val = 1);
                tx.try_send_ref().unwrap().with_mut(|val| *val = 2);
            })
        };
        let p2 = thread::spawn(move || {
            tx.try_send(3).unwrap();
            tx.try_send(4).unwrap();
        });

        let mut vals = Vec::new();

        for val in &rx {
            val.with(|val| vals.push(*val));
        }

        vals.sort_unstable();
        assert_eq!(vals, vec![1, 2, 3, 4]);

        p1.join().unwrap();
        p2.join().unwrap();
    })
}

#[test]
fn rx_closes() {
    const ITERATIONS: usize = 6;
    loom::model(|| {
        let (tx, rx) = sync::channel(ThingBuf::new(ITERATIONS / 2));

        let producer = thread::spawn(move || {
            'iters: for i in 0..=ITERATIONS {
                test_println!("sending {}", i);
                'send: loop {
                    match tx.try_send(i) {
                        Ok(_) => break 'send,
                        Err(TrySendError::Full(_)) => thread::yield_now(),
                        Err(TrySendError::Closed(_)) => break 'iters,
                    }
                }
                test_println!("sent {}\n", i);
            }
        });

        for i in 0..ITERATIONS - 1 {
            let n = rx.recv();

            test_println!("recv {:?}\n", n);
            assert_eq!(n, Some(i));
        }
        drop(rx);

        producer.join().unwrap();
    })
}

#[test]
fn spsc_recv_then_try_send() {
    loom::model(|| {
        let (tx, rx) = sync::channel(ThingBuf::<i32>::new(4));
        let consumer = thread::spawn(move || {
            assert_eq!(rx.recv().unwrap(), 10);
        });

        tx.try_send(10).unwrap();
        consumer.join().unwrap();
    })
}

#[test]
fn spsc_recv_then_close() {
    loom::model(|| {
        let (tx, rx) = sync::channel(ThingBuf::<i32>::new(4));
        let producer = thread::spawn(move || {
            drop(tx);
        });

        assert_eq!(rx.recv(), None);

        producer.join().unwrap();
    });
}

#[test]
fn spsc_recv_then_try_send_then_close() {
    loom::model(|| {
        let (tx, rx) = sync::channel(ThingBuf::<i32>::new(2));
        let consumer = thread::spawn(move || {
            assert_eq!(rx.recv().unwrap(), 10);
            assert_eq!(rx.recv().unwrap(), 20);
            assert_eq!(rx.recv(), None);
        });

        tx.try_send(10).unwrap();
        tx.try_send(20).unwrap();
        drop(tx);
        consumer.join().unwrap();
    })
}

#[test]
fn mpsc_send_recv() {
    // make this bigger at YOUR OWN RISK; this model takes like a gazillion
    // years to run otherwise.
    const N_SENDS: usize = 2;

    fn do_producer(tx: sync::Sender<usize>, tag: usize) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            for i in 1..=N_SENDS {
                test_println!("SENDING {:?}", i + tag);
                tx.send(i + tag).unwrap();
                test_println!("SENT {:?}", i + tag);
            }
        })
    }

    let mut builder = loom::model::Builder::default();
    builder.max_branches = 10_000;
    loom::run_builder(builder, || {
        let (tx, rx) = sync::channel(ThingBuf::<usize>::new(N_SENDS));
        let producer1 = do_producer(tx.clone(), 10);
        let producer2 = do_producer(tx, 20);

        let mut results = Vec::new();
        while let Some(val) = rx.recv() {
            test_println!("RECEIVED {:?}", val);
            results.push(val);
        }

        producer1.join().expect("producer 1 panicked");
        producer2.join().expect("producer 2 panicked");

        assert_eq!(results.len(), N_SENDS * 2);
        for i in 1..=N_SENDS {
            assert!(
                results.contains(&(i + 10)),
                "missing value from producer 1; i={:?}; results={:?}",
                i,
                results
            );
            assert!(
                results.contains(&(i + 20)),
                "missing value from producer 2; i={:?}; results={:?}",
                i,
                results
            );
        }
    })
}

#[test]
fn spsc_send_recv_in_order() {
    const N_SENDS: usize = 4;
    loom::model(|| {
        let (tx, rx) = sync::channel(ThingBuf::<usize>::new(N_SENDS / 2));
        let consumer = thread::spawn(move || {
            for i in 1..=N_SENDS {
                assert_eq!(rx.recv(), Some(i));
            }
            assert_eq!(rx.recv(), None);
        });

        for i in 1..=N_SENDS {
            tx.send(i).unwrap()
        }
        drop(tx);
        consumer.join().unwrap();
    })
}

#[test]
fn tx_close_wakes() {
    loom::model(|| {
        let (tx, rx) = sync::channel(ThingBuf::<i32>::new(2));
        let consumer = thread::spawn(move || {
            assert_eq!(rx.recv(), None);
        });
        drop(tx);
        consumer.join().unwrap();
    });
}
