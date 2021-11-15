use std::{fmt::Write, sync::Arc, thread};
use thingbuf::{Slot, StringBuf, ThingBuf};

#[test]
fn static_storage_thingbuf() {
    let thingbuf = Arc::new(ThingBuf::<i32, [Slot<i32>; 16]>::new_static());

    let producer = {
        let thingbuf = thingbuf.clone();
        thread::spawn(move || {
            for i in 0..32 {
                let mut thing = 'write: loop {
                    match thingbuf.push_ref() {
                        Ok(thing) => break 'write thing,
                        _ => thread::yield_now(),
                    }
                };
                thing.with_mut(|thing| *thing = i);
            }
        })
    };

    let mut i = 0;

    // While the producer is writing to the queue, push each entry to the
    // results string.
    while Arc::strong_count(&thingbuf) > 1 {
        match thingbuf.pop_ref() {
            Some(thing) => thing.with(|thing| {
                assert_eq!(*thing, i);
                i += 1;
            }),
            None => thread::yield_now(),
        }
    }

    producer.join().unwrap();

    // drain the queue.
    while let Some(thing) = thingbuf.pop_ref() {
        thing.with(|thing| {
            assert_eq!(*thing, i);
            i += 1;
        })
    }
}

#[test]
fn static_storage_stringbuf() {
    let stringbuf = Arc::new(StringBuf::<[Slot<String>; 8]>::new_static());

    let producer = {
        let stringbuf = stringbuf.clone();
        thread::spawn(move || {
            for i in 0..16 {
                let mut string = 'write: loop {
                    match stringbuf.write() {
                        Ok(string) => break 'write string,
                        _ => thread::yield_now(),
                    }
                };

                write!(&mut string, "{:?}", i).unwrap();
            }
        })
    };

    let mut results = String::new();

    // While the producer is writing to the queue, push each entry to the
    // results string.
    while Arc::strong_count(&stringbuf) > 1 {
        if let Some(string) = stringbuf.pop_ref() {
            writeln!(results, "{}", string).unwrap();
        }
        thread::yield_now();
    }

    producer.join().unwrap();

    // drain the queue.
    while let Some(string) = stringbuf.pop_ref() {
        writeln!(results, "{}", string).unwrap();
    }

    println!("results:\n{}", results);

    for (n, ln) in results.lines().enumerate() {
        assert_eq!(ln.parse::<usize>(), Ok(n));
    }
}
