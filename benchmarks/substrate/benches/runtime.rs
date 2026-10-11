// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The emission seam with `footfall_runtime` installed directly.
//!
//! This target installs the runtime as the process-wide backend, and the
//! `footfall_tracing` layer as the global `tracing` subscriber, once per
//! process. Each group sets the capture level it needs before its cases run.
//! Cases that buffer events time through `time_chunked`, which resets the
//! runtime outside the timed region so no sample reaches the drop path.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use footfall::{
    Args, Context, InstrumentationLevel, counter_add, gauge_set, histogram_record, message,
    message_with, resource_update, scope, scope_begin, scope_end,
};
use footfall_benchmarks::{
    ATLAS, ATLAS_ID, CACHE, COUNTER_CAPACITY, EVENT_CAPACITY, FRAME, LATENCY, LOG, MEASUREMENT,
    OPS, PASS, QUEUE_DEPTH, RECORD, SEEDED_FRAMES, THREAD_COUNTS, WARM_UP, rotating_contexts,
    sample_context, seed_snapshot, time_chunked, time_parallel_counter_add,
};
use footfall_export::to_chrome_trace_json;
use footfall_runtime::Runtime;
use footfall_tracing::FootfallLayer;
use std::{hint::black_box, sync::OnceLock};
use tracing_subscriber::layer::SubscriberExt as _;

/// Installs the runtime and the tracing layer on first use.
fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<&'static Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        let runtime = Runtime::named_with_level(
            "footfall_benchmarks",
            COUNTER_CAPACITY,
            EVENT_CAPACITY,
            InstrumentationLevel::Off,
        )
        .install_global()
        .expect("benchmark runtime should install once per process");
        tracing::subscriber::set_global_default(
            tracing_subscriber::registry().with(FootfallLayer::new()),
        )
        .expect("benchmark tracing subscriber should install once per process");
        runtime
    })
}

/// The runtime installed but filtering everything.
fn off(c: &mut Criterion) {
    let runtime = runtime();
    runtime.set_level(InstrumentationLevel::Off);
    let context = sample_context();
    let mut group = c.benchmark_group("runtime/off");
    group.bench_function("counter_add", |b| {
        b.iter(|| counter_add(black_box(&OPS), black_box(&context), black_box(1)));
    });
    group.bench_function("scope begin+end", |b| {
        b.iter(|| {
            let token = scope_begin(black_box(&RECORD), black_box(&context));
            scope_end(black_box(token));
        });
    });
    group.finish();
}

/// Aggregated counter, gauge, and histogram updates with no event buffer.
fn counters(c: &mut Criterion) {
    let runtime = runtime();
    runtime.set_level(InstrumentationLevel::Counters);
    let reset = || runtime.reset();
    let context = sample_context();
    let mut group = c.benchmark_group("runtime/counters");
    group.bench_function("counter_add", |b| {
        b.iter_custom(|iters| {
            time_chunked(iters, reset, || {
                counter_add(black_box(&OPS), black_box(&context), black_box(1));
            })
        });
    });
    group.bench_function("gauge_set", |b| {
        b.iter_custom(|iters| {
            time_chunked(iters, reset, || {
                gauge_set(black_box(&QUEUE_DEPTH), black_box(&context), black_box(17));
            })
        });
    });
    group.bench_function("histogram_record", |b| {
        b.iter_custom(|iters| {
            time_chunked(iters, reset, || {
                histogram_record(black_box(&LATENCY), black_box(&context), black_box(1_500));
            })
        });
    });

    let rotating = rotating_contexts(context);
    group.bench_function(
        format!("counter_add {} rotating contexts", rotating.len()),
        |b| {
            b.iter_custom(|iters| {
                let mut next = 0_usize;
                time_chunked(iters, reset, || {
                    counter_add(black_box(&OPS), black_box(&rotating[next]), black_box(1));
                    next = (next + 1) % rotating.len();
                })
            });
        },
    );
    group.bench_function("counter_add context built per call", |b| {
        b.iter_custom(|iters| {
            time_chunked(iters, reset, || {
                let context = Context::root()
                    .with(&FRAME, black_box(7_u32))
                    .with(&PASS, black_box(1_u32))
                    .with(&CACHE, "atlas");
                counter_add(black_box(&OPS), black_box(&context), black_box(1));
            })
        });
    });

    for threads in THREAD_COUNTS {
        group.bench_with_input(
            BenchmarkId::new("counter_add threads", threads),
            &threads,
            |b, &threads| {
                b.iter_custom(|iters| {
                    runtime.reset();
                    time_parallel_counter_add(threads, iters)
                });
            },
        );
    }
    group.finish();
}

/// Buffered scopes, messages, and the `tracing` bridge.
fn scoped(c: &mut Criterion) {
    let runtime = runtime();
    runtime.set_level(InstrumentationLevel::Scoped);
    let reset = || runtime.reset();
    let context = sample_context();
    let mut group = c.benchmark_group("runtime/scoped");
    group.bench_function("scope begin+end", |b| {
        b.iter_custom(|iters| {
            time_chunked(iters, reset, || {
                let token = scope_begin(black_box(&RECORD), black_box(&context));
                scope_end(black_box(token));
            })
        });
    });
    group.bench_function("scope guard", |b| {
        b.iter_custom(|iters| {
            time_chunked(iters, reset, || {
                drop(black_box(scope(black_box(&RECORD), black_box(&context))));
            })
        });
    });
    group.bench_function("message", |b| {
        b.iter_custom(|iters| {
            time_chunked(iters, reset, || {
                message(
                    black_box(&LOG),
                    black_box(&context),
                    black_box("uploaded atlas"),
                );
            })
        });
    });
    let args = Args::none().with("bytes", 4096_u32).with("items", 3_u32);
    group.bench_function("message_with 2 args", |b| {
        b.iter_custom(|iters| {
            time_chunked(iters, reset, || {
                message_with(
                    black_box(&LOG),
                    black_box(&context),
                    black_box("uploaded atlas"),
                    black_box(&args),
                );
            })
        });
    });
    group.bench_function("tracing span enter+exit", |b| {
        b.iter_custom(|iters| {
            time_chunked(iters, reset, || {
                let span = tracing::info_span!("record_pass");
                drop(black_box(span.enter()));
            })
        });
    });
    group.bench_function("tracing event", |b| {
        b.iter_custom(|iters| {
            time_chunked(iters, reset, || {
                tracing::info!(bytes = 4096_u64, "uploaded atlas");
            })
        });
    });
    group.finish();
}

/// Resource observations, captured only at the trace level.
fn trace(c: &mut Criterion) {
    let runtime = runtime();
    runtime.set_level(InstrumentationLevel::Trace);
    let reset = || runtime.reset();
    let context = sample_context();
    let mut group = c.benchmark_group("runtime/trace");
    group.bench_function("resource_update", |b| {
        b.iter_custom(|iters| {
            time_chunked(iters, reset, || {
                resource_update(
                    black_box(&ATLAS),
                    black_box(ATLAS_ID),
                    black_box(&context),
                    black_box(4096),
                );
            })
        });
    });
    group.finish();
}

/// Snapshot and Chrome export cost over a fixed seeded capture.
fn capture(c: &mut Criterion) {
    let runtime = runtime();
    runtime.set_level(InstrumentationLevel::Trace);
    runtime.reset();
    seed_snapshot(sample_context());
    let snapshot = runtime.snapshot();

    let mut group = c.benchmark_group("runtime/capture");
    group.throughput(Throughput::Elements(u64::from(SEEDED_FRAMES)));
    group.bench_function(format!("snapshot {SEEDED_FRAMES} frames"), |b| {
        b.iter(|| black_box(runtime.snapshot()));
    });
    group.bench_function(format!("chrome trace export {SEEDED_FRAMES} frames"), |b| {
        b.iter(|| black_box(to_chrome_trace_json(black_box(&snapshot))));
    });
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().warm_up_time(WARM_UP).measurement_time(MEASUREMENT);
    targets = off, counters, scoped, trace, capture
}
criterion_main!(benches);
