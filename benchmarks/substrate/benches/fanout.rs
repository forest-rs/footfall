// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The runtime behind `footfall_fanout`.
//!
//! This target installs a fanout over the runtime and the core no-op
//! backend, once per process, so the cases measure what the fanout adapter
//! adds on top of the runtime-direct cases in the `runtime` target.

use criterion::{Criterion, criterion_group, criterion_main};
use footfall::{
    InstrumentationLevel, NoopBackend, RegisteredContext, counter_add, scope_begin, scope_end,
};
use footfall_benchmarks::{
    COUNTER_CAPACITY, EVENT_CAPACITY, MEASUREMENT, OPS, RECORD, WARM_UP, sample_context,
    time_chunked,
};
use footfall_fanout::Fanout;
use footfall_runtime::Runtime;
use std::{hint::black_box, sync::OnceLock};

/// Installs the fanout over the runtime on first use.
fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<&'static Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        let runtime: &'static Runtime = Box::leak(Box::new(Runtime::named_with_level(
            "footfall_benchmarks",
            COUNTER_CAPACITY,
            EVENT_CAPACITY,
            InstrumentationLevel::Off,
        )));
        Fanout::new(runtime, &NoopBackend)
            .install_global()
            .expect("benchmark fanout should install once per process");
        runtime
    })
}

fn counters(c: &mut Criterion) {
    let runtime = runtime();
    runtime.set_level(InstrumentationLevel::Counters);
    let registered = RegisteredContext::new(&sample_context());
    let context = registered.handle();
    let mut group = c.benchmark_group("fanout/counters");
    group.bench_function("counter_add", |b| {
        b.iter_custom(|iters| {
            time_chunked(
                iters,
                || runtime.reset(),
                || counter_add(black_box(&OPS), black_box(context), black_box(1)),
            )
        });
    });
    group.finish();
}

fn scoped(c: &mut Criterion) {
    let runtime = runtime();
    runtime.set_level(InstrumentationLevel::Scoped);
    let registered = RegisteredContext::new(&sample_context());
    let context = registered.handle();
    let mut group = c.benchmark_group("fanout/scoped");
    group.bench_function("scope begin+end", |b| {
        b.iter_custom(|iters| {
            time_chunked(
                iters,
                || runtime.reset(),
                || {
                    let token = scope_begin(black_box(&RECORD), black_box(context));
                    scope_end(black_box(token));
                },
            )
        });
    });
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().warm_up_time(WARM_UP).measurement_time(MEASUREMENT);
    targets = counters, scoped
}
criterion_main!(benches);
