use super::*;
use crate::loom::{self, alloc::Track, thread};
use crate::mpsc::blocking;
use crate::mpsc::blocking::RecvRef;

#[test]
#[cfg_attr(ci_skip_slow_models, ignore)]
fn mpsc_try_send_recv() {
    loom::model(|| {
        let (tx, rx) = blocking::channel(3);

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

        let mut vals = Vec::new();

        for val in &rx {
            vals.push(*val);
        }

        vals.sort_unstable();
        assert_eq_dbg!(vals, vec![1, 2, 3]);

        p1.join().unwrap();
        p2.join().unwrap();
    })
}

#[test]
#[cfg_attr(ci_skip_slow_models, ignore)]
fn mpsc_try_recv_ref() {
    loom::model(|| {
        let (tx, rx) = blocking::channel(2);

        let p1 = {
            let tx = tx.clone();
            thread::spawn(move || {
                *tx.send_ref().unwrap() = 1;
                thread::yield_now();
                *tx.send_ref().unwrap() = 2;
            })
        };
        let p2 = thread::spawn(move || {
            *tx.send_ref().unwrap() = 3;
            thread::yield_now();
            *tx.send_ref().unwrap() = 4;
        });

        let mut vals = Vec::new();

        while vals.len() < 4 {
            match rx.try_recv_ref() {
                Ok(val) => vals.push(*val),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Closed) => panic!("channel closed"),
            }
            thread::yield_now();
        }

        vals.sort_unstable();
        assert_eq_dbg!(vals, vec![1, 2, 3, 4]);

        p1.join().unwrap();
        p2.join().unwrap();
    })
}

#[test]
#[cfg_attr(ci_skip_slow_models, ignore)]
fn mpsc_test_skip_slot() {
    // This test emulates a situation where we might need to skip a slot. The setup includes two writing
    // threads that write elements to the channel and one reading thread that maintains a RecvRef to the
    // third element until the end of the test, necessitating the skip:
    // Given that the channel capacity is 2, here's the sequence of operations:
    //   Thread 1 writes: 1, 2
    //   Thread 2 writes: 3, 4
    // The main thread reads from slots in this order: 0, 1, 0 (holds ref), 1, 1.
    // As a result, the third slot is skipped during this process.
    loom::model(|| {
        let (tx, rx) = blocking::channel(2);

        let p1 = {
            let tx = tx.clone();
            thread::spawn(move || {
                *tx.send_ref().unwrap() = 1;
                thread::yield_now();
                *tx.send_ref().unwrap() = 2;
            })
        };

        let p2 = {
            thread::spawn(move || {
                *tx.send_ref().unwrap() = 3;
                thread::yield_now();
                *tx.send_ref().unwrap() = 4;
            })
        };

        let mut vals = Vec::new();
        let mut hold: Vec<RecvRef<usize>> = Vec::new();

        while vals.len() < 4 {
            match rx.try_recv_ref() {
                Ok(val) => {
                    if vals.len() == 2 && !hold.is_empty() {
                        vals.push(*hold.pop().unwrap());
                        vals.push(*val);
                    } else if vals.len() == 1 && hold.is_empty() {
                        hold.push(val);
                    } else {
                        vals.push(*val);
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Closed) => {
                    panic!("channel closed");
                }
            }
            thread::yield_now();
        }

        vals.sort_unstable();
        assert_eq_dbg!(vals, vec![1, 2, 3, 4]);

        p1.join().unwrap();
        p2.join().unwrap();
    })
}

#[test]
fn rx_closes() {
    const ITERATIONS: usize = 6;
    loom::model(|| {
        let (tx, rx) = blocking::channel(ITERATIONS / 2);

        let producer = thread::spawn(move || {
            'iters: for i in 0..=ITERATIONS {
                test_println!("sending {}", i);
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

        for i in 0..ITERATIONS - 1 {
            let n = rx.recv();

            test_println!("recv {:?}\n", n);
            assert_eq_dbg!(n, Some(i));
        }
        drop(rx);

        producer.join().unwrap();
    })
}

#[test]
fn rx_close_unconsumed_spsc() {
    // Tests that messages that have not been consumed by the receiver are
    // dropped when dropping the channel.
    const MESSAGES: usize = 4;

    loom::model(|| {
        let (tx, rx) = blocking::channel(MESSAGES);

        let consumer = thread::spawn(move || {
            // recieve one message
            let msg = rx.recv();
            test_println!("recv {:?}", msg);
            assert_dbg!(msg.is_some());
            // drop the receiver...
        });

        let mut i = 1;
        while let Ok(mut slot) = tx.send_ref() {
            test_println!("producer sending {}...", i);
            *slot = Track::new(i);
            i += 1;
        }

        consumer.join().unwrap();
        drop(tx);
    })
}

#[test]
#[ignore] // This is marked as `ignore` because it takes over an hour to run.
fn rx_close_unconsumed_mpsc() {
    const MESSAGES: usize = 2;

    fn do_producer(tx: blocking::Sender<Track<i32>>, n: usize) -> impl FnOnce() + Send + Sync {
        move || {
            let mut i = 1;
            while let Ok(mut slot) = tx.send_ref() {
                test_println!("producer {} sending {}...", n, i);
                *slot = Track::new(i);
                i += 1;
            }
        }
    }

    loom::model(|| {
        let (tx, rx) = blocking::channel(MESSAGES);

        let consumer = thread::spawn(move || {
            // recieve one message
            let msg = rx.recv();
            test_println!("recv {:?}", msg);
            assert_dbg!(msg.is_some());
            // drop the receiver...
        });

        let producer = thread::spawn(do_producer(tx.clone(), 1));

        do_producer(tx, 2)();

        producer.join().unwrap();
        consumer.join().unwrap();
    })
}

#[test]
fn spsc_recv_then_try_send() {
    loom::model(|| {
        let (tx, rx) = blocking::channel::<i32>(4);
        let consumer = thread::spawn(move || {
            assert_eq_dbg!(rx.recv().unwrap(), 10);
        });

        tx.try_send(10).unwrap();
        consumer.join().unwrap();
    })
}

#[test]
fn spsc_recv_then_close() {
    loom::model(|| {
        let (tx, rx) = blocking::channel::<i32>(4);
        let producer = thread::spawn(move || {
            drop(tx);
        });

        assert_eq_dbg!(rx.recv(), None);

        producer.join().unwrap();
    });
}

#[test]
fn spsc_recv_then_try_send_then_close() {
    loom::model(|| {
        let (tx, rx) = blocking::channel::<i32>(2);
        let consumer = thread::spawn(move || {
            assert_eq_dbg!(rx.recv().unwrap(), 10);
            assert_eq_dbg!(rx.recv().unwrap(), 20);
            assert_eq_dbg!(rx.recv(), None);
        });

        tx.try_send(10).unwrap();
        tx.try_send(20).unwrap();
        drop(tx);
        consumer.join().unwrap();
    })
}

#[test]
#[cfg_attr(ci_skip_slow_models, ignore)]
fn mpsc_send_recv_wrap() {
    loom::model(|| {
        let (tx, rx) = blocking::channel::<usize>(1);
        let producer1 = do_producer(tx.clone(), 10);
        let producer2 = do_producer(tx, 20);

        let mut results = Vec::new();
        while let Some(val) = rx.recv() {
            test_println!("RECEIVED {:?}", val);
            results.push(val);
        }

        producer1.join().expect("producer 1 panicked");
        producer2.join().expect("producer 2 panicked");

        assert_eq_dbg!(results.len(), 2);
        assert_dbg!(
            results.contains(&10),
            "missing value from producer 1; results={:?}",
            results
        );

        assert_dbg!(
            results.contains(&20),
            "missing value from producer 2; results={:?}",
            results
        );
    })
}

#[test]
fn mpsc_send_recv_no_wrap() {
    loom::model(|| {
        let (tx, rx) = blocking::channel::<usize>(2);
        let producer1 = do_producer(tx.clone(), 10);
        let producer2 = do_producer(tx, 20);

        let mut results = Vec::new();
        while let Some(val) = rx.recv() {
            test_println!("RECEIVED {:?}", val);
            results.push(val);
        }

        producer1.join().expect("producer 1 panicked");
        producer2.join().expect("producer 2 panicked");

        assert_eq_dbg!(results.len(), 2);
        assert_dbg!(
            results.contains(&10),
            "missing value from producer 1; results={:?}",
            results
        );

        assert_dbg!(
            results.contains(&20),
            "missing value from producer 2; results={:?}",
            results
        );
    })
}

fn do_producer(tx: blocking::Sender<usize>, tag: usize) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        test_println!("SENDING {:?}", tag);
        tx.send(tag).unwrap();
        test_println!("SENT {:?}", tag);
    })
}

#[test]
fn spsc_send_recv_in_order_no_wrap() {
    const N_SENDS: usize = 4;
    loom::model(|| {
        let (tx, rx) = blocking::channel::<usize>(N_SENDS);
        let consumer = thread::spawn(move || {
            for i in 1..=N_SENDS {
                assert_eq_dbg!(rx.recv(), Some(i));
            }
            assert_eq_dbg!(rx.recv(), None);
        });

        for i in 1..=N_SENDS {
            tx.send(i).unwrap()
        }
        drop(tx);
        consumer.join().unwrap();
    })
}

#[test]
fn spsc_send_recv_in_order_wrap() {
    const N_SENDS: usize = 2;
    loom::model(|| {
        let (tx, rx) = blocking::channel::<usize>(N_SENDS / 2);
        let consumer = thread::spawn(move || {
            for i in 1..=N_SENDS {
                assert_eq_dbg!(rx.recv(), Some(i));
            }
            assert_eq_dbg!(rx.recv(), None);
        });

        for i in 1..=N_SENDS {
            tx.send(i).unwrap()
        }
        drop(tx);
        consumer.join().unwrap();
    })
}

#[test]
fn spsc_send_recv_in_order_skip_wrap() {
    const N_SENDS: usize = 5;
    loom::model(|| {
        let (tx, rx) = blocking::channel::<usize>((N_SENDS + 1) / 2);
        let consumer = thread::spawn(move || {
            let mut hold = Vec::new();
            assert_eq_dbg!(rx.recv(), Some(1));
            loop {
                match rx.try_recv_ref() {
                    Ok(val) => {
                        assert_eq_dbg!(*val, 2);
                        hold.push(val);
                        break;
                    }
                    Err(TryRecvError::Empty) => {
                        loom::thread::yield_now();
                    }
                    Err(TryRecvError::Closed) => panic!("channel closed"),
                }
            }
            for i in 3..=N_SENDS {
                assert_eq_dbg!(rx.recv(), Some(i));
            }
            assert_eq_dbg!(rx.recv(), None);
        });
        for i in 1..=N_SENDS {
            tx.send(i).unwrap();
        }
        drop(tx);
        consumer.join().unwrap();
    });
}

#[test]
fn tx_close_wakes() {
    loom::model(|| {
        let (tx, rx) = blocking::channel::<i32>(2);
        let consumer = thread::spawn(move || {
            assert_eq_dbg!(rx.recv(), None);
        });
        drop(tx);
        consumer.join().unwrap();
    });
}

#[test]
fn tx_close_drains_queue() {
    const LEN: usize = 4;
    loom::model(|| {
        let (tx, rx) = blocking::channel(LEN);
        let producer = thread::spawn(move || {
            for i in 0..LEN {
                tx.send(i).unwrap();
            }
        });

        for i in 0..LEN {
            assert_eq_dbg!(rx.recv(), Some(i))
        }

        producer.join().unwrap();
    });
}
