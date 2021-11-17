use std::{
    fmt::Write,
    sync::atomic::{AtomicBool, Ordering},
    thread,
};
use thingbuf::{StaticStringBuf, StaticThingBuf};

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
            thing.with_mut(|thing| *thing = i);
        }
        PRODUCER_LIVE.store(false, Ordering::Release);
    });

    let mut i = 0;

    // While the producer is writing to the queue, push each entry to the
    // results string.
    while PRODUCER_LIVE.load(Ordering::Acquire) {
        match BUF.pop_ref() {
            Some(thing) => thing.with(|thing| {
                assert_eq!(*thing, i);
                i += 1;
            }),
            None => thread::yield_now(),
        }
    }

    producer.join().unwrap();

    // drain the queue.
    while let Some(thing) = BUF.pop_ref() {
        thing.with(|thing| {
            assert_eq!(*thing, i);
            i += 1;
        })
    }
}

#[test]
fn static_storage_stringbuf() {
    static BUF: StaticStringBuf<8> = StaticStringBuf::new();
    static PRODUCER_LIVE: AtomicBool = AtomicBool::new(true);

    let producer = {
        thread::spawn(move || {
            for i in 0..16 {
                let mut string = 'write: loop {
                    match BUF.write() {
                        Ok(string) => break 'write string,
                        _ => thread::yield_now(),
                    }
                };

                write!(&mut string, "{:?}", i).unwrap();

                PRODUCER_LIVE.store(false, Ordering::Release);
            }
        })
    };

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

    // drain the queue.
    while let Some(string) = BUF.pop_ref() {
        writeln!(results, "{}", string).unwrap();
    }

    println!("results:\n{}", results);

    for (n, ln) in results.lines().enumerate() {
        assert_eq!(ln.parse::<usize>(), Ok(n));
    }
}
