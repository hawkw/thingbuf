use super::*;
use crate::{
    loom::{self, future, thread},
    ThingBuf,
};

#[test]
fn mpsc_try_send_recv() {
    loom::model(|| {
        let (tx, rx) = channel(ThingBuf::new(3));

        let p1 = {
            let tx = tx.clone();
            thread::spawn(move || {
                *tx.try_send_ref().unwrap() = 1;
            })
        };
        let p2 = thread::spawn(move || {
            *tx.try_send_ref().unwrap() = 2;
            *tx.try_send_ref().unwrap() = 3;
        });

        let mut vals = future::block_on(async move {
            let mut vals = Vec::new();
            while let Some(val) = rx.recv_ref().await {
                vals.push(*val);
            }
            vals
        });

        vals.sort_unstable();
        assert_eq!(vals, vec![1, 2, 3]);

        p1.join().unwrap();
        p2.join().unwrap();
    })
}

#[test]
fn rx_closes() {
    const ITERATIONS: usize = 6;
    loom::model(|| {
        let (tx, rx) = channel(ThingBuf::new(ITERATIONS / 2));

        let producer = thread::spawn(move || {
            'iters: for i in 0..=ITERATIONS {
                test_println!("sending {}...", i);
                'send: loop {
                    match tx.try_send_ref() {
                        Ok(mut slot) => {
                            *slot = i;
                            break 'send;
                        }
                        Err(TrySendError::Full(_)) => thread::yield_now(),
                        Err(TrySendError::Closed(_)) => break 'iters,
                    }
                }
                test_println!("sent {}\n", i);
            }
        });

        future::block_on(async move {
            for i in 0..ITERATIONS - 1 {
                test_println!("receiving {}...", i);
                let n = rx.recv().await;

                test_println!("recv {:?}\n", n);
                assert_eq!(n, Some(i));
            }
        });

        producer.join().unwrap();
    })
}

#[test]
fn spsc_recv_then_send() {
    loom::model(|| {
        let (tx, rx) = channel(ThingBuf::<i32>::new(4));
        let consumer = thread::spawn(move || {
            future::block_on(async move {
                assert_eq!(rx.recv().await.unwrap(), 10);
            })
        });

        tx.try_send(10).unwrap();
        consumer.join().unwrap();
    })
}

#[test]
fn spsc_recv_then_close() {
    loom::model(|| {
        let (tx, rx) = channel(ThingBuf::<i32>::new(4));
        let producer = thread::spawn(move || {
            drop(tx);
        });

        future::block_on(async move {
            let recv = rx.recv().await;
            assert_eq!(recv, None);
        });

        producer.join().unwrap();
    });
}

#[test]
fn spsc_recv_then_send_then_close() {
    loom::model(|| {
        let (tx, rx) = channel(ThingBuf::<i32>::new(2));
        let consumer = thread::spawn(move || {
            future::block_on(async move {
                assert_eq!(rx.recv().await.unwrap(), 10);
                assert_eq!(rx.recv().await.unwrap(), 20);
                assert_eq!(rx.recv().await, None);
            })
        });

        tx.try_send(10).unwrap();
        tx.try_send(20).unwrap();
        drop(tx);
        consumer.join().unwrap();
    })
}

#[test]
fn spsc_send_recv_in_order_no_wrap() {
    const N_SENDS: usize = 4;
    loom::model(|| {
        let (tx, rx) = channel(ThingBuf::<usize>::new(N_SENDS));
        let consumer = thread::spawn(move || {
            future::block_on(async move {
                for i in 1..=N_SENDS {
                    assert_eq!(rx.recv().await, Some(i));
                }
                assert_eq!(rx.recv().await, None);
            })
        });
        future::block_on(async move {
            for i in 1..=N_SENDS {
                tx.send(i).await.unwrap()
            }
        });
        consumer.join().unwrap();
    })
}

#[test]
fn spsc_send_recv_in_order_wrap() {
    const N_SENDS: usize = 2;
    loom::model(|| {
        let (tx, rx) = channel(ThingBuf::<usize>::new(N_SENDS / 2));
        let consumer = thread::spawn(move || {
            future::block_on(async move {
                for i in 1..=N_SENDS {
                    assert_eq!(rx.recv().await, Some(i));
                }
                assert_eq!(rx.recv().await, None);
            })
        });
        future::block_on(async move {
            for i in 1..=N_SENDS {
                tx.send(i).await.unwrap()
            }
        });
        consumer.join().unwrap();
    })
}

#[test]
fn mpsc_send_recv_wrap() {
    loom::model(|| {
        let (tx, rx) = channel(ThingBuf::<usize>::new(1));
        let producer1 = do_producer(tx.clone(), 10);
        let producer2 = do_producer(tx, 20);

        let results = future::block_on(async move {
            let mut results = Vec::new();
            while let Some(val) = rx.recv().await {
                test_println!("RECEIVED {:?}", val);
                results.push(val);
            }
            results
        });

        producer1.join().expect("producer 1 panicked");
        producer2.join().expect("producer 2 panicked");

        assert_eq!(results.len(), 2);
        assert!(
            results.contains(&10),
            "missing value from producer 1; results={:?}",
            results
        );

        assert!(
            results.contains(&20),
            "missing value from producer 2; results={:?}",
            results
        );
    })
}

#[test]
fn mpsc_send_recv_no_wrap() {
    loom::model(|| {
        let (tx, rx) = channel(ThingBuf::<usize>::new(2));
        let producer1 = do_producer(tx.clone(), 10);
        let producer2 = do_producer(tx, 20);

        let results = future::block_on(async move {
            let mut results = Vec::new();
            while let Some(val) = rx.recv().await {
                test_println!("RECEIVED {:?}", val);
                results.push(val);
            }
            results
        });

        producer1.join().expect("producer 1 panicked");
        producer2.join().expect("producer 2 panicked");

        assert_eq!(results.len(), 2);
        assert!(
            results.contains(&10),
            "missing value from producer 1; results={:?}",
            results
        );

        assert!(
            results.contains(&20),
            "missing value from producer 2; results={:?}",
            results
        );
    })
}

fn do_producer(tx: Sender<usize>, tag: usize) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        future::block_on(async move {
            test_println!("SENDING {:?}", tag);
            tx.send(tag).await.unwrap();
            test_println!("SENT {:?}", tag);
        })
    })
}

#[test]
fn tx_close_wakes() {
    loom::model(|| {
        let (tx, rx) = channel(ThingBuf::<i32>::new(2));
        let consumer = thread::spawn(move || {
            future::block_on(async move {
                assert_eq!(rx.recv().await, None);
            })
        });
        drop(tx);
        consumer.join().unwrap();
    });
}
