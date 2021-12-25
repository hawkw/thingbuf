use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::thread;

/// This benchmark simulates sending a bunch of strings over a channel. It's
/// intended to simulate the sort of workload that a `thingbuf` is intended
/// for, where the type of element in the buffer is expensive to allocate,
/// copy, or drop, but they can be re-used in place without
/// allocating/deallocating.
///
/// So, this may not be strictly representative of performance in the case of,
/// say, sending a bunch of integers over the channel; instead it simulates
/// the kind of scenario that `thingbuf` is optimized for.
fn bench_spsc_try_send_reusable(c: &mut Criterion) {
    let mut group = c.benchmark_group("sync/spsc/try_send_reusable");
    static THE_STRING: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\
aaaaaaaaaaaaaa";

    for size in [100, 500, 1_000, 5_000, 10_000] {
        group.throughput(Throughput::Elements(size));
        group.bench_with_input(BenchmarkId::new("ThingBuf", size), &size, |b, &i| {
            b.iter(|| {
                use thingbuf::mpsc::{sync, TrySendError};
                let (tx, rx) = sync::channel::<String>(100);
                let producer = thread::spawn(move || loop {
                    match tx.try_send_ref() {
                        Ok(mut slot) => {
                            slot.clear();
                            slot.push_str(THE_STRING)
                        }
                        Err(TrySendError::Closed(_)) => break,
                        _ => thread::yield_now(),
                    }
                });
                for _ in 0..i {
                    let r = rx.recv_ref().unwrap();
                    r.with(|val| {
                        criterion::black_box(val);
                    });
                }
                drop(rx);
                producer.join().unwrap();
            })
        });

        #[cfg(feature = "std-sync")]
        group.bench_with_input(BenchmarkId::new("std::sync::mpsc", size), &size, |b, &i| {
            b.iter(|| {
                use std::sync::mpsc::{self, TrySendError};
                let (tx, rx) = mpsc::sync_channel(100);
                let producer = thread::spawn(move || loop {
                    match tx.try_send(String::from(THE_STRING)) {
                        Ok(()) => {}
                        Err(TrySendError::Disconnected(_)) => break,
                        _ => thread::yield_now(),
                    }
                });
                for _ in 0..i {
                    let val = rx.recv().unwrap();
                    criterion::black_box(&val);
                }
                drop(rx);
                producer.join().unwrap();
            })
        });

        #[cfg(feature = "crossbeam")]
        group.bench_with_input(
            BenchmarkId::new("crossbeam::channel::bounded", size),
            &size,
            |b, &i| {
                b.iter(|| {
                    use crossbeam::channel::{self, TrySendError};
                    let (tx, rx) = channel::bounded(100);

                    let producer = thread::spawn(move || loop {
                        match tx.try_send(String::from(THE_STRING)) {
                            Ok(()) => {}
                            Err(TrySendError::Disconnected(_)) => break,
                            _ => thread::yield_now(),
                        }
                    });

                    for _ in 0..i {
                        let val = rx.recv().unwrap();
                        criterion::black_box(&val);
                    }

                    drop(rx);
                    producer.join().unwrap();
                })
            },
        );
    }
    group.finish();
}

fn bench_spsc_blocking_send_reusable(c: &mut Criterion) {
    let mut group = c.benchmark_group("sync/spsc/blocking_send_reusable");
    static THE_STRING: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\
aaaaaaaaaaaaaa";

    for size in [100, 500, 1_000, 5_000, 10_000] {
        group.throughput(Throughput::Elements(size));
        group.bench_with_input(BenchmarkId::new("ThingBuf", size), &size, |b, &i| {
            b.iter(|| {
                use thingbuf::mpsc::sync;
                let (tx, rx) = sync::channel::<String>(100);
                let producer = thread::spawn(move || {
                    while let Ok(mut slot) = tx.send_ref() {
                        slot.clear();
                        slot.push_str(THE_STRING);
                    }
                });
                for _ in 0..i {
                    let r = rx.recv_ref().unwrap();
                    r.with(|val| {
                        criterion::black_box(val);
                    });
                }
                drop(rx);
                producer.join().unwrap();
            })
        });

        #[cfg(feature = "std-sync")]
        group.bench_with_input(BenchmarkId::new("std::sync::mpsc", size), &size, |b, &i| {
            b.iter(|| {
                use std::sync::mpsc;
                let (tx, rx) = mpsc::sync_channel(100);
                let producer =
                    thread::spawn(move || while tx.send(String::from(THE_STRING)).is_ok() {});
                for _ in 0..i {
                    let val = rx.recv().unwrap();
                    criterion::black_box(&val);
                }
                drop(rx);
                producer.join().unwrap();
            })
        });

        #[cfg(feature = "crossbeam")]
        group.bench_with_input(
            BenchmarkId::new("crossbeam::channel::bounded", size),
            &size,
            |b, &i| {
                b.iter(|| {
                    use crossbeam::channel;
                    let (tx, rx) = channel::bounded(100);

                    let producer =
                        thread::spawn(move || while tx.send(String::from(THE_STRING)).is_ok() {});

                    for _ in 0..i {
                        let val = rx.recv().unwrap();
                        criterion::black_box(&val);
                    }

                    drop(rx);
                    producer.join().unwrap();
                })
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_spsc_try_send_reusable,
    bench_spsc_blocking_send_reusable
);
criterion_main!(benches);
