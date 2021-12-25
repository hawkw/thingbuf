use std::thread;
use thingbuf::mpsc::sync;

#[test]
fn basically_works() {
    use std::collections::HashSet;

    const N_SENDS: usize = 10;
    const N_PRODUCERS: usize = 10;

    fn start_producer(tx: sync::Sender<usize>, n: usize) -> thread::JoinHandle<()> {
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

    let (tx, rx) = sync::channel(N_SENDS / 2);
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

        let (tx, rx) = sync::channel(LEN);
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
