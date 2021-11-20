use super::*;
use crate::loom::{self, thread};

#[test]
fn basically_works() {
    loom::model(|| {
        let (tx, rx) = sync_channel(crate::ThingBuf::new(4));

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
        let (tx, rx) = sync_channel(crate::ThingBuf::new(ITERATIONS / 2));

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
