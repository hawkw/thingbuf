use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use tokio::{runtime, task};

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
    let mut group = c.benchmark_group("async/spsc/try_send_reusable");
    static THE_STRING: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\
aaaaaaaaaaaaaa";

    for size in [100, 500, 1_000, 5_000, 10_000] {
        group.throughput(Throughput::Elements(size));

        group.bench_with_input(BenchmarkId::new("ThingBuf", size), &size, |b, &i| {
            let rt = runtime::Builder::new_current_thread().build().unwrap();
            b.to_async(rt).iter(|| async {
                use thingbuf::mpsc::{self, TrySendError};

                let (tx, rx) = mpsc::channel::<String>(100);
                task::spawn(async move {
                    loop {
                        match tx.try_send_ref() {
                            Ok(mut slot) => {
                                slot.clear();
                                slot.push_str(THE_STRING);
                            }
                            Err(TrySendError::Closed(_)) => break,
                            _ => task::yield_now().await,
                        }
                    }
                });
                for _ in 0..i {
                    let val = rx.recv_ref().await.unwrap();
                    criterion::black_box(&*val);
                }
            })
        });

        #[cfg(feature = "futures")]
        group.bench_with_input(
            BenchmarkId::new("futures::channel::mpsc", size),
            &size,
            |b, &i| {
                let rt = runtime::Builder::new_current_thread().build().unwrap();
                b.to_async(rt).iter(|| async {
                    use futures::{channel::mpsc, stream::StreamExt};
                    let (mut tx, mut rx) = mpsc::channel(100);
                    task::spawn(async move {
                        loop {
                            match tx.try_send(String::from(THE_STRING)) {
                                Ok(()) => {}
                                Err(e) if e.is_disconnected() => break,
                                _ => task::yield_now().await,
                            }
                        }
                    });
                    for _ in 0..i {
                        let val = rx.next().await.unwrap();
                        criterion::black_box(&val);
                    }
                })
            },
        );

        #[cfg(feature = "tokio-sync")]
        group.bench_with_input(
            BenchmarkId::new("tokio::sync::mpsc", size),
            &size,
            |b, &i| {
                let rt = runtime::Builder::new_current_thread().build().unwrap();
                b.to_async(rt).iter(|| {
                    // turn off Tokio's automatic cooperative yielding for this
                    // benchmark. in code with a large number of concurrent
                    // tasks, this feature makes the MPSC channel (and other
                    // Tokio synchronization primitives) better "team players"
                    // than other implementations, since it prevents them from
                    // using too much scheduler time.
                    //
                    // in this benchmark, though, there *are* no other tasks
                    // running, so automatic yielding just means we spend more
                    // time ping-ponging through the scheduler than every other
                    // implementation.
                    tokio::task::unconstrained(async {
                        use tokio::sync::mpsc::{self, error::TrySendError};
                        let (tx, mut rx) = mpsc::channel(100);
                        task::spawn(tokio::task::unconstrained(async move {
                            loop {
                                // this actually brings Tokio's MPSC closer to what
                                // `ThingBuf` can do than all the other impls --- we
                                // only allocate if we _were_ able to reserve send
                                // capacity. but, we will still allocate and
                                // deallocate a string for every message...
                                match tx.try_reserve() {
                                    Ok(permit) => permit.send(String::from(THE_STRING)),
                                    Err(TrySendError::Closed(_)) => break,
                                    _ => task::yield_now().await,
                                }
                            }
                        }));
                        for _ in 0..i {
                            let val = rx.recv().await.unwrap();
                            criterion::black_box(&val);
                        }
                    })
                })
            },
        );

        #[cfg(feature = "async-std")]
        group.bench_with_input(
            BenchmarkId::new("async_std::channel::bounded", size),
            &size,
            |b, &i| {
                let rt = runtime::Builder::new_current_thread().build().unwrap();
                b.to_async(rt).iter(|| async {
                    use async_std::channel::{self, TrySendError};
                    let (tx, rx) = channel::bounded(100);
                    task::spawn(async move {
                        loop {
                            match tx.try_send(String::from(THE_STRING)) {
                                Ok(()) => {}
                                Err(TrySendError::Closed(_)) => break,
                                _ => task::yield_now().await,
                            }
                        }
                    });
                    for _ in 0..i {
                        let val = rx.recv().await.unwrap();
                        criterion::black_box(&val);
                    }
                })
            },
        );
    }

    group.finish();
}

fn bench_spsc_reusable(c: &mut Criterion) {
    let mut group = c.benchmark_group("async/spsc/reusable");
    static THE_STRING: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\
aaaaaaaaaaaaaa";

    for size in [100, 500, 1_000, 5_000, 10_000] {
        group.throughput(Throughput::Elements(size));

        group.bench_with_input(BenchmarkId::new("ThingBuf", size), &size, |b, &i| {
            let rt = runtime::Builder::new_current_thread().build().unwrap();
            b.to_async(rt).iter(|| async {
                use thingbuf::mpsc;
                let (tx, rx) = mpsc::channel::<String>(100);
                task::spawn(async move {
                    while let Ok(mut slot) = tx.send_ref().await {
                        slot.clear();
                        slot.push_str(THE_STRING);
                    }
                });
                for _ in 0..i {
                    let val = rx.recv_ref().await.unwrap();
                    criterion::black_box(&*val);
                }
            })
        });

        #[cfg(feature = "futures")]
        group.bench_with_input(
            BenchmarkId::new("futures::channel::mpsc", size),
            &size,
            |b, &i| {
                let rt = runtime::Builder::new_current_thread().build().unwrap();
                b.to_async(rt).iter(|| async {
                    use futures::{channel::mpsc, sink::SinkExt, stream::StreamExt};
                    let (mut tx, mut rx) = mpsc::channel(100);
                    task::spawn(
                        async move { while tx.send(String::from(THE_STRING)).await.is_ok() {} },
                    );
                    for _ in 0..i {
                        let val = rx.next().await.unwrap();
                        criterion::black_box(&val);
                    }
                })
            },
        );

        #[cfg(feature = "tokio-sync")]
        group.bench_with_input(
            BenchmarkId::new("tokio::sync::mpsc", size),
            &size,
            |b, &i| {
                let rt = runtime::Builder::new_current_thread().build().unwrap();
                b.to_async(rt).iter(|| {
                    // turn off Tokio's automatic cooperative yielding for this
                    // benchmark. in code with a large number of concurrent
                    // tasks, this feature makes the MPSC channel (and other
                    // Tokio synchronization primitives) better "team players"
                    // than other implementations, since it prevents them from
                    // using too much scheduler time.
                    //
                    // in this benchmark, though, there *are* no other tasks
                    // running, so automatic yielding just means we spend more
                    // time ping-ponging through the scheduler than every other
                    // implementation.
                    tokio::task::unconstrained(async {
                        use tokio::sync::mpsc;
                        let (tx, mut rx) = mpsc::channel(100);
                        task::spawn(tokio::task::unconstrained(async move {
                            // this actually brings Tokio's MPSC closer to what
                            // `ThingBuf` can do than all the other impls --- we
                            // only allocate if we _were_ able to reserve send
                            // capacity. but, we will still allocate and
                            // deallocate a string for every message...
                            while let Ok(permit) = tx.reserve().await {
                                permit.send(String::from(THE_STRING));
                            }
                        }));
                        for _ in 0..i {
                            let val = rx.recv().await.unwrap();
                            criterion::black_box(&val);
                        }
                    })
                })
            },
        );

        #[cfg(feature = "async-std")]
        group.bench_with_input(
            BenchmarkId::new("async_std::channel::bounded", size),
            &size,
            |b, &i| {
                let rt = runtime::Builder::new_current_thread().build().unwrap();
                b.to_async(rt).iter(|| async {
                    use async_std::channel;
                    let (tx, rx) = channel::bounded(100);
                    task::spawn(
                        async move { while tx.send(String::from(THE_STRING)).await.is_ok() {} },
                    );
                    for _ in 0..i {
                        let val = rx.recv().await.unwrap();
                        criterion::black_box(&val);
                    }
                })
            },
        );
    }

    group.finish();
}

/// The same benchmark, but with integers. Messages are not heap allocated, so
/// non-thingbuf channel impls are not burdened by allocator churn for messages.
fn bench_spsc_try_send_integer(c: &mut Criterion) {
    let mut group = c.benchmark_group("async/spsc/try_send_integer");

    for size in [100, 500, 1_000, 5_000, 10_000] {
        group.throughput(Throughput::Elements(size));
        group.bench_with_input(BenchmarkId::new("ThingBuf", size), &size, |b, &i| {
            let rt = runtime::Builder::new_current_thread().build().unwrap();
            b.to_async(rt).iter(|| async {
                use thingbuf::mpsc::{self, TrySendError};
                let (tx, rx) = mpsc::channel(100);
                task::spawn(async move {
                    let mut i = 0;
                    loop {
                        match tx.try_send(i) {
                            Ok(()) => {
                                i += 1;
                            }
                            Err(TrySendError::Closed(_)) => break,
                            _ => task::yield_now().await,
                        }
                    }
                });
                for n in 0..i {
                    let val = rx.recv().await.unwrap();
                    assert_eq!(n, val);
                }
            })
        });

        #[cfg(feature = "futures")]
        group.bench_with_input(
            BenchmarkId::new("futures::channel::mpsc", size),
            &size,
            |b, &i| {
                let rt = runtime::Builder::new_current_thread().build().unwrap();
                b.to_async(rt).iter(|| async {
                    use futures::{channel::mpsc, stream::StreamExt};
                    let (mut tx, mut rx) = mpsc::channel(100);
                    task::spawn(async move {
                        let mut i = 0;
                        loop {
                            match tx.try_send(i) {
                                Ok(()) => {
                                    i += 1;
                                }
                                Err(e) if e.is_disconnected() => break,
                                _ => task::yield_now().await,
                            }
                        }
                    });
                    for n in 0..i {
                        let val = rx.next().await.unwrap();
                        assert_eq!(n, val);
                    }
                })
            },
        );

        #[cfg(feature = "tokio-sync")]
        group.bench_with_input(
            BenchmarkId::new("tokio::sync::mpsc", size),
            &size,
            |b, &i| {
                let rt = runtime::Builder::new_current_thread().build().unwrap();
                b.to_async(rt).iter(|| {
                    // turn off Tokio's automatic cooperative yielding for this
                    // benchmark. in code with a large number of concurrent
                    // tasks, this feature makes the MPSC channel (and other
                    // Tokio synchronization primitives) better "team players"
                    // than other implementations, since it prevents them from
                    // using too much scheduler time.
                    //
                    // in this benchmark, though, there *are* no other tasks
                    // running, so automatic yielding just means we spend more
                    // time ping-ponging through the scheduler than every other
                    // implementation.
                    tokio::task::unconstrained(async {
                        use tokio::sync::mpsc::{self, error::TrySendError};
                        let (tx, mut rx) = mpsc::channel(100);
                        task::spawn(tokio::task::unconstrained(async move {
                            let mut i = 0;
                            loop {
                                match tx.try_send(i) {
                                    Ok(()) => {
                                        i += 1;
                                    }
                                    Err(TrySendError::Closed(_)) => break,
                                    _ => task::yield_now().await,
                                }
                            }
                        }));
                        for n in 0..i {
                            let val = rx.recv().await.unwrap();
                            assert_eq!(n, val);
                        }
                    })
                })
            },
        );

        #[cfg(feature = "async-std")]
        group.bench_with_input(
            BenchmarkId::new("async_std::channel::bounded", size),
            &size,
            |b, &i| {
                let rt = runtime::Builder::new_current_thread().build().unwrap();
                b.to_async(rt).iter(|| async {
                    use async_std::channel::{self, TrySendError};
                    let (tx, rx) = channel::bounded(100);
                    task::spawn(async move {
                        let mut i = 0;
                        loop {
                            match tx.try_send(i) {
                                Ok(()) => {
                                    i += 1;
                                }
                                Err(TrySendError::Closed(_)) => break,
                                _ => task::yield_now().await,
                            }
                        }
                    });
                    for n in 0..i {
                        let val = rx.recv().await.unwrap();
                        assert_eq!(n, val);
                    }
                })
            },
        );
    }

    group.finish();
}
criterion_group!(
    benches,
    bench_spsc_try_send_reusable,
    bench_spsc_try_send_integer,
    bench_spsc_reusable,
);
criterion_main!(benches);
