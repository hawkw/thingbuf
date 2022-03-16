mod async_mpsc_utils;
use criterion::{criterion_group, criterion_main, Criterion};

fn bench_mpsc_reusable(c: &mut Criterion) {
    let group = c.benchmark_group("async/nowait/mpsc_reusable");
    async_mpsc_utils::bench_mpsc_reusable(group, 200, 256);
}

fn bench_mpsc_integer(c: &mut Criterion) {
    let group = c.benchmark_group("async/nowait/mpsc_integer");
    async_mpsc_utils::bench_mpsc_integer(group, 200, 256);
}

criterion_group!(benches, bench_mpsc_reusable, bench_mpsc_integer,);
criterion_main!(benches);
