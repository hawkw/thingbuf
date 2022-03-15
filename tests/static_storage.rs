#![cfg(feature = "static")]
use std::{
    fmt::Write,
    sync::atomic::{AtomicBool, Ordering},
    thread,
};
use thingbuf::{recycling, StaticThingBuf};

#[test]
fn static_storage_thingbuf() {
    static BUF: StaticThingBuf<i32, 16> = StaticThingBuf::new();
    static PRODUCER_LIVE: AtomicBool = AtomicBool::new(true);

    let producer = thread::spawn(move || {
        for i in 0..32 {
            let mut thing = 'write: loop {
                match BUF.push_ref() {
                    Ok(thing) => break 'write thing,
                    _ => thread::yield_now(),
                }
            };
            *thing = i;
        }
        PRODUCER_LIVE.store(false, Ordering::Release);
    });

    let mut i = 0;

    // While the producer is writing to the queue, push each entry to the
    // results string.
    while PRODUCER_LIVE.load(Ordering::Acquire) {
        match BUF.pop_ref() {
            Some(thing) => {
                assert_eq!(*thing, i);
                i += 1;
            }
            None => thread::yield_now(),
        }
    }

    producer.join().unwrap();

    // drain the queue.
    while let Some(thing) = BUF.pop_ref() {
        assert_eq!(*thing, i);
        i += 1;
    }
}

#[test]
fn static_storage_stringbuf() {
    use recycling::WithCapacity;

    static BUF: StaticThingBuf<String, 8, WithCapacity> =
        StaticThingBuf::with_recycle(WithCapacity::new().with_max_capacity(8));
    static PRODUCER_LIVE: AtomicBool = AtomicBool::new(true);

    let producer = thread::spawn(move || {
        for i in 0..16 {
            let mut string = 'write: loop {
                match BUF.push_ref() {
                    Ok(string) => break 'write string,
                    _ => thread::yield_now(),
                }
            };

            write!(&mut string, "{:?}", i).unwrap();
        }

        PRODUCER_LIVE.store(false, Ordering::Release);
        println!("producer done");
    });

    let mut results = String::new();

    // While the producer is writing to the queue, push each entry to the
    // results string.
    while PRODUCER_LIVE.load(Ordering::Acquire) {
        if let Some(string) = BUF.pop_ref() {
            writeln!(results, "{}", string).unwrap();
        }
        thread::yield_now();
    }

    producer.join().unwrap();
    println!("producer done...");

    // drain the queue.
    while let Some(string) = BUF.pop_ref() {
        writeln!(results, "{}", string).unwrap();
    }

    println!("results:\n{}", results);

    for (n, ln) in results.lines().enumerate() {
        assert_eq!(ln.parse::<usize>(), Ok(n));
    }
}

#[tokio::test]
async fn static_async_channel() {
    use std::collections::HashSet;
    use thingbuf::mpsc;

    const N_PRODUCERS: usize = 8;
    const N_SENDS: usize = N_PRODUCERS * 2;

    static CHANNEL: mpsc::StaticChannel<usize, N_PRODUCERS> = mpsc::StaticChannel::new();

    async fn do_producer(tx: mpsc::StaticSender<usize>, n: usize) {
        let tag = n * N_SENDS;
        for i in 0..N_SENDS {
            let msg = i + tag;
            println!("sending {}...", msg);
            tx.send(msg).await.unwrap();
            println!("sent {}!", msg);
        }
        println!("PRODUCER {} DONE!", n);
    }

    let (tx, rx) = CHANNEL.split();
    for n in 0..N_PRODUCERS {
        tokio::spawn(do_producer(tx.clone(), n));
    }
    drop(tx);

    let mut results = HashSet::new();
    while let Some(val) = {
        println!("receiving...");
        rx.recv().await
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
fn static_blocking_channel() {
    use std::collections::HashSet;
    use thingbuf::mpsc::blocking;

    const N_PRODUCERS: usize = 8;
    const N_SENDS: usize = N_PRODUCERS * 2;

    static CHANNEL: blocking::StaticChannel<usize, N_PRODUCERS> = blocking::StaticChannel::new();

    fn do_producer(tx: blocking::StaticSender<usize>, n: usize) -> impl FnOnce() {
        move || {
            let tag = n * N_SENDS;
            for i in 0..N_SENDS {
                let msg = i + tag;
                println!("sending {}...", msg);
                tx.send(msg).unwrap();
                println!("sent {}!", msg);
            }
            println!("PRODUCER {} DONE!", n);
        }
    }

    let (tx, rx) = CHANNEL.split();
    for n in 0..N_PRODUCERS {
        std::thread::spawn(do_producer(tx.clone(), n));
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
