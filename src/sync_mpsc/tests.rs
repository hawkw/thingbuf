use super::*;
use crate::{
    loom::{self, thread},
    ThingBuf,
};

#[test]
fn basically_works() {
    loom::model(|| {
        let (tx, rx) = sync_mpsc(ThingBuf::new(4));

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
        let (tx, rx) = sync_mpsc(ThingBuf::new(ITERATIONS / 2));

        let producer = thread::spawn(move || {
            'iters: for i in 0..=ITERATIONS {
                'send: loop {
                    match tx.try_send(i) {
                        Ok(_) => break 'send,
                        Err(TrySendError::AtCapacity(_)) => thread::yield_now(),
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
fn spsc_recv_then_send() {
    loom::model(|| {
        let (tx, rx) = sync_mpsc(ThingBuf::<i32>::new(4));
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
        let (tx, rx) = sync_mpsc(ThingBuf::<i32>::new(4));
        let producer = thread::spawn(move || {
            drop(tx);
        });

        assert_eq!(rx.recv(), None);

        producer.join().unwrap();
    });
}

#[test]
fn spsc_recv_then_send_then_close() {
    loom::model(|| {
        let (tx, rx) = sync_mpsc(ThingBuf::<i32>::new(2));
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
