// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Snapshot and export cost over a fixed seeded capture.
//!
//! This target installs its own runtime and records only the seeded capture,
//! so the cases measure snapshotting and exporting exactly that capture
//! rather than whatever earlier cases left in a shared runtime.

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use footfall::InstrumentationLevel;
use footfall_benchmarks::{
    COUNTER_CAPACITY, EVENT_CAPACITY, MEASUREMENT, SEEDED_FRAMES, WARM_UP, sample_context,
    seed_snapshot,
};
use footfall_export::to_chrome_trace_json;
use footfall_runtime::Runtime;
use std::hint::black_box;

fn capture(c: &mut Criterion) {
    let runtime = Runtime::builder()
        .process_name("footfall_benchmarks")
        .level(InstrumentationLevel::Trace)
        .counter_capacity(COUNTER_CAPACITY)
        .event_capacity(EVENT_CAPACITY)
        .install_global()
        .expect("benchmark runtime should install once per process");
    let frames = seed_snapshot(sample_context());
    let owned = runtime.snapshot().into();

    let mut group = c.benchmark_group("runtime/capture");
    group.throughput(Throughput::Elements(u64::from(SEEDED_FRAMES)));
    group.bench_function(format!("snapshot {SEEDED_FRAMES} frames"), |b| {
        b.iter(|| black_box(runtime.snapshot()));
    });
    group.bench_function(format!("chrome trace export {SEEDED_FRAMES} frames"), |b| {
        b.iter(|| black_box(to_chrome_trace_json(black_box(&owned))));
    });
    group.finish();
    drop(frames);
}

criterion_group! {
    name = benches;
    config = Criterion::default().warm_up_time(WARM_UP).measurement_time(MEASUREMENT);
    targets = capture
}
criterion_main!(benches);
