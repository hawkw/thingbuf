use std::thread;
use thingbuf::mpsc::blocking;
use thingbuf::mpsc::errors::{TryRecvError, TrySendError};

#[test]
fn basically_works() {
    use std::collections::HashSet;

    const N_SENDS: usize = 10;
    const N_PRODUCERS: usize = 10;

    fn start_producer(tx: blocking::Sender<usize>, n: usize) -> thread::JoinHandle<()> {
        let tag = n * N_SENDS;
        thread::Builder::new()
            .name(format!("producer {}", n))
            .spawn(move || {
                for i in 0..N_SENDS {
                    let msg = i + tag;
                    println!("[producer {}] sending {}...", n, msg);
                    tx.send(msg).unwrap();
                    println!("[producer {}] sent {}!", n, msg);
                }
                println!("[producer {}] DONE!", n);
            })
            .expect("spawning threads should succeed")
    }

    let (tx, rx) = blocking::channel(N_SENDS / 2);
    for n in 0..N_PRODUCERS {
        start_producer(tx.clone(), n);
    }
    drop(tx);

    let mut results = HashSet::new();
    while let Some(val) = {
        println!("receiving...");
        rx.recv()
    } {
        println!("received {}!", val);
        results.insert(val);
    }

    let results = dbg!(results);

    for n in 0..N_PRODUCERS {
        let tag = n * N_SENDS;
        for i in 0..N_SENDS {
            let msg = i + tag;
            assert!(results.contains(&msg), "missing message {:?}", msg);
        }
    }
}

#[test]
fn tx_close_drains_queue() {
    const LEN: usize = 4;
    for i in 0..10000 {
        println!("\n\n--- iteration {} ---\n\n", i);

        let (tx, rx) = blocking::channel(LEN);
        let producer = thread::spawn(move || {
            for i in 0..LEN {
                tx.send(i).unwrap();
            }
        });

        for i in 0..LEN {
            assert_eq!(rx.recv(), Some(i))
        }

        producer.join().unwrap();
    }
}

#[test]
fn spsc_skip_slot() {
    let (tx, rx) = blocking::channel::<usize>(3);
    // 0 lap
    tx.send(0).unwrap();
    assert_eq!(rx.recv(), Some(0));
    tx.send(1).unwrap();
    let msg_ref = rx.try_recv_ref().unwrap();
    tx.send(2).unwrap();
    assert_eq!(rx.recv(), Some(2));
    // 1 lap
    tx.send(3).unwrap();
    assert_eq!(rx.recv(), Some(3));
    tx.send(4).unwrap();
    assert_eq!(rx.recv(), Some(4));
    drop(msg_ref);
    // 2 lap
    tx.send(5).unwrap();
    tx.send(6).unwrap();
    tx.send(7).unwrap();
    assert!(matches!(tx.try_send_ref(), Err(TrySendError::Full(_))));
    assert_eq!(rx.recv(), Some(5));
    assert_eq!(rx.recv(), Some(6));
    assert_eq!(rx.recv(), Some(7));
}

#[test]
fn spsc_full_after_skipped() {
    let (tx, rx) = blocking::channel::<usize>(3);
    // 0 lap
    tx.send(0).unwrap();
    assert_eq!(rx.recv(), Some(0));
    tx.send(1).unwrap();
    let _msg_ref = rx.try_recv_ref().unwrap();
    tx.send(2).unwrap();
    // lap 1
    tx.send(3).unwrap();
    assert!(matches!(tx.try_send_ref(), Err(TrySendError::Full(_))));
}

#[test]
fn spsc_empty_after_skipped() {
    let (tx, rx) = blocking::channel::<usize>(2);
    // 0 lap
    tx.send(0).unwrap();
    tx.send(1).unwrap();
    let _msg_ref = rx.try_recv_ref().unwrap();
    assert_eq!(rx.recv(), Some(1));
    assert!(matches!(rx.try_recv_ref(), Err(TryRecvError::Empty)));
}
