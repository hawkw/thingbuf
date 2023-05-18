use super::*;
use crate::loom::{self, alloc::Track, future, thread};

#[test]
#[cfg_attr(ci_skip_slow_models, ignore)]
fn mpsc_try_send_recv() {
    loom::model(|| {
        let (tx, rx) = channel(3);

        let p1 = {
            let tx = tx.clone();
            thread::spawn(move || {
                *tx.try_send_ref().unwrap() = Track::new(1);
            })
        };
        let p2 = thread::spawn(move || {
            *tx.try_send_ref().unwrap() = Track::new(2);
            *tx.try_send_ref().unwrap() = Track::new(3);
        });

        let mut vals = future::block_on(async move {
            let mut vals = Vec::new();
            while let Some(val) = rx.recv_ref().await {
                vals.push(*val.get_ref());
            }
            vals
        });

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
        let (tx, rx) = channel(2);

        let p1 = {
            let tx = tx.clone();
            thread::spawn(move || {
                future::block_on(async move {
                    tx.send(1).await.unwrap();
                    tx.send(2).await.unwrap();
                })
            })
        };
        let p2 = thread::spawn(move || {
            future::block_on(async move {
                tx.send(3).await.unwrap();
                tx.send(4).await.unwrap();
            })
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
        let (tx, rx) = channel(2);

        let p1 = {
            let tx = tx.clone();
            thread::spawn(move || {
                future::block_on(async move {
                    tx.send(1).await.unwrap();
                    tx.send(2).await.unwrap();
                })
            })
        };

        let p2 = {
            thread::spawn(move || {
                future::block_on(async move {
                    tx.send(3).await.unwrap();
                    tx.send(4).await.unwrap();
                })
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
        let (tx, rx) = channel(ITERATIONS / 2);

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
                assert_eq_dbg!(n, Some(i));
            }
        });

        producer.join().unwrap();
    })
}

#[test]
fn rx_close_unconsumed_spsc() {
    // Tests that messages that have not been consumed by the receiver are
    // dropped when dropping the channel.
    const MESSAGES: usize = 4;

    loom::model(|| {
        let (tx, rx) = channel(MESSAGES);

        let consumer = thread::spawn(move || {
            future::block_on(async move {
                // recieve one message
                let msg = rx.recv().await;
                test_println!("recv {:?}", msg);
                assert_dbg!(msg.is_some());
                // drop the receiver...
            })
        });

        future::block_on(async move {
            let mut i = 1;
            while let Ok(mut slot) = tx.send_ref().await {
                test_println!("producer sending {}...", i);
                *slot = Track::new(i);
                i += 1;
            }
        });

        consumer.join().unwrap();
    })
}

#[test]
#[ignore] // This is marked as `ignore` because it takes over an hour to run.
fn rx_close_unconsumed_mpsc() {
    const MESSAGES: usize = 2;

    async fn do_producer(tx: Sender<Track<i32>>, n: usize) {
        let mut i = 1;
        while let Ok(mut slot) = tx.send_ref().await {
            test_println!("producer {} sending {}...", n, i);
            *slot = Track::new(i);
            i += 1;
        }
    }

    loom::model(|| {
        let (tx, rx) = channel(MESSAGES);

        let consumer = thread::spawn(move || {
            future::block_on(async move {
                // recieve one message
                let msg = rx.recv().await;
                test_println!("recv {:?}", msg);
                assert_dbg!(msg.is_some());
                // drop the receiver...
            })
        });

        let producer = {
            let tx = tx.clone();
            thread::spawn(move || future::block_on(do_producer(tx, 1)))
        };

        future::block_on(do_producer(tx, 2));

        producer.join().unwrap();
        consumer.join().unwrap();
    })
}

#[test]
fn spsc_recv_then_send() {
    loom::model(|| {
        let (tx, rx) = channel::<i32>(4);
        let consumer = thread::spawn(move || {
            future::block_on(async move {
                assert_eq_dbg!(rx.recv().await.unwrap(), 10);
            })
        });

        tx.try_send(10).unwrap();
        consumer.join().unwrap();
    })
}

#[test]
fn spsc_recv_then_close() {
    loom::model(|| {
        let (tx, rx) = channel::<i32>(4);
        let producer = thread::spawn(move || {
            drop(tx);
        });

        future::block_on(async move {
            let recv = rx.recv().await;
            assert_eq_dbg!(recv, None);
        });

        producer.join().unwrap();
    });
}

#[test]
fn spsc_recv_then_send_then_close() {
    loom::model(|| {
        let (tx, rx) = channel::<i32>(2);
        let consumer = thread::spawn(move || {
            future::block_on(async move {
                assert_eq_dbg!(rx.recv().await.unwrap(), 10);
                assert_eq_dbg!(rx.recv().await.unwrap(), 20);
                assert_eq_dbg!(rx.recv().await, None);
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
        let (tx, rx) = channel::<usize>(N_SENDS);
        let consumer = thread::spawn(move || {
            future::block_on(async move {
                for i in 1..=N_SENDS {
                    assert_eq_dbg!(rx.recv().await, Some(i));
                }
                assert_eq_dbg!(rx.recv().await, None);
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
        let (tx, rx) = channel::<usize>(N_SENDS / 2);
        let consumer = thread::spawn(move || {
            future::block_on(async move {
                for i in 1..=N_SENDS {
                    assert_eq_dbg!(rx.recv().await, Some(i));
                }
                assert_eq_dbg!(rx.recv().await, None);
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
fn spsc_send_recv_in_order_skip_wrap() {
    const N_SENDS: usize = 5;
    loom::model(|| {
        let (tx, rx) = channel::<usize>((N_SENDS + 1) / 2);
        let consumer = thread::spawn(move || {
            future::block_on(async move {
                let mut hold = Vec::new();
                assert_eq_dbg!(rx.recv().await, Some(1));
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
                    assert_eq_dbg!(rx.recv().await, Some(i));
                }
                assert_eq_dbg!(rx.recv().await, None);
            });
        });
        future::block_on(async move {
            for i in 1..=N_SENDS {
                tx.send(i).await.unwrap();
            }
        });
        consumer.join().unwrap();
    });
}

#[test]
#[cfg_attr(ci_skip_slow_models, ignore)]
fn mpsc_send_recv_wrap() {
    loom::model(|| {
        let (tx, rx) = channel::<usize>(1);
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
        let (tx, rx) = channel::<usize>(2);
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
        let (tx, rx) = channel::<i32>(2);
        let consumer = thread::spawn(move || {
            future::block_on(async move {
                assert_eq_dbg!(rx.recv().await, None);
            })
        });
        drop(tx);
        consumer.join().unwrap();
    });
}

#[test]
fn tx_close_drains_queue() {
    const LEN: usize = 4;
    loom::model(|| {
        let (tx, rx) = channel(LEN);
        let producer = thread::spawn(move || {
            future::block_on(async move {
                for i in 0..LEN {
                    tx.send(i).await.unwrap();
                }
            })
        });

        future::block_on(async move {
            for i in 0..LEN {
                assert_eq_dbg!(rx.recv().await, Some(i))
            }
        });

        producer.join().unwrap();
    });
}
