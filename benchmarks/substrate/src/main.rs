// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Simple Footfall substrate wind tunnel.
//!
//! This is intentionally a normal top-level binary instead of a Criterion
//! benchmark so it adds no dependency pressure to the workspace.

use footfall::{
    Context, CounterDesc, CounterDescId, InstrumentationLevel, LabelKey, MarkerKind, MarkerPhase,
    ResourceId, ResourceKindDesc, ResourceKindId, ScopeDesc, ScopeDescId, counter_add,
    frame_marker, resource_update, scope_begin, scope_end,
};
use footfall_export::to_chrome_trace_json;
use footfall_runtime::{Runtime, install_global};
use std::{
    hint::black_box,
    thread,
    time::{Duration, Instant},
};

static OPS: CounterDesc = CounterDesc::count(CounterDescId::new(1), "ops");
static RECORD: ScopeDesc = ScopeDesc::new(ScopeDescId::new(2), "record");
static ATLAS: ResourceKindDesc = ResourceKindDesc::new(ResourceKindId::new(3), "atlas");
static ATLAS_ID: ResourceId = ResourceId::new(4);

const FAST_ITERS: u64 = 100_000;
const EVENT_ITERS: u64 = 20_000;
const PARALLEL_THREADS: u64 = 4;
const PARALLEL_ITERS_PER_THREAD: u64 = 25_000;
const SNAPSHOT_ITERS: u64 = 1_000;
const EXPORT_ITERS: u64 = 200;

fn main() {
    let context = Context::root()
        .with(LabelKey::Frame, 1_u32)
        .with(LabelKey::Pass, 1_u32)
        .with(LabelKey::Cache, "atlas");

    let mut results = Vec::new();
    results.push(run("disabled/no-backend counter_add", FAST_ITERS, || {
        counter_add(black_box(&OPS), black_box(context), black_box(1));
    }));

    let runtime = Box::leak(Box::new(Runtime::named_with_level(
        "footfall_benchmarks",
        4096,
        usize::try_from(EVENT_ITERS.saturating_mul(4)).unwrap_or(usize::MAX),
        InstrumentationLevel::Off,
    )));
    install_global(runtime).expect("benchmark runtime should install once");

    results.push(run("runtime/off counter_add", FAST_ITERS, || {
        counter_add(black_box(&OPS), black_box(context), black_box(1));
    }));

    runtime.set_level(InstrumentationLevel::Counters);
    runtime.reset();
    results.push(run("runtime/counters counter_add", FAST_ITERS, || {
        counter_add(black_box(&OPS), black_box(context), black_box(1));
    }));

    runtime.reset();
    results.push(run_parallel_counter_updates(
        "runtime/counters parallel counter_add",
        PARALLEL_THREADS,
        PARALLEL_ITERS_PER_THREAD,
    ));

    runtime.set_level(InstrumentationLevel::Scoped);
    runtime.reset();
    results.push(run("runtime/scoped scope pair", EVENT_ITERS, || {
        let token = scope_begin(black_box(&RECORD), black_box(context));
        scope_end(black_box(token));
    }));

    results.push(run("context extend", FAST_ITERS, || {
        let context = Context::root()
            .with(LabelKey::Frame, black_box(7_u32))
            .with(LabelKey::Pass, black_box(3_u32))
            .with(LabelKey::Node, black_box(42_u32));
        black_box(context);
    }));

    runtime.set_level(InstrumentationLevel::Trace);
    runtime.reset();
    results.push(run("runtime/trace resource_update", EVENT_ITERS, || {
        resource_update(
            black_box(&ATLAS),
            black_box(ATLAS_ID),
            black_box(context),
            black_box(4096),
        );
    }));

    runtime.reset();
    seed_snapshot(context);
    results.push(run("snapshot clone", SNAPSHOT_ITERS, || {
        black_box(runtime.snapshot());
    }));

    let snapshot = runtime.snapshot();
    results.push(run("chrome trace export", EXPORT_ITERS, || {
        black_box(to_chrome_trace_json(black_box(&snapshot)));
    }));

    println!("footfall substrate wind tunnel");
    println!("metric,iters,total_us,ns_per_op");
    for result in results {
        println!(
            "{},{},{},{}",
            result.name,
            result.iters,
            result.total.as_micros(),
            result.ns_per_op()
        );
    }
}

fn seed_snapshot(context: Context) {
    for frame in 0..128_u32 {
        let frame_context = context.with(LabelKey::Frame, frame);
        frame_marker(MarkerKind::Frame, MarkerPhase::Begin, frame_context);
        counter_add(&OPS, frame_context, u64::from(frame));
        let token = scope_begin(&RECORD, frame_context);
        resource_update(&ATLAS, ATLAS_ID, frame_context, 4096 + u64::from(frame));
        scope_end(token);
        frame_marker(MarkerKind::Frame, MarkerPhase::End, frame_context);
    }
}

fn run(name: &'static str, iters: u64, mut f: impl FnMut()) -> ResultRow {
    let start = Instant::now();
    for _ in 0..iters {
        f();
    }
    ResultRow {
        name,
        iters,
        total: start.elapsed(),
    }
}

fn run_parallel_counter_updates(
    name: &'static str,
    threads: u64,
    iters_per_thread: u64,
) -> ResultRow {
    let start = Instant::now();
    thread::scope(|scope| {
        for thread_index in 0..threads {
            scope.spawn(move || {
                let context = Context::root()
                    .with(
                        LabelKey::Frame,
                        u32::try_from(thread_index).unwrap_or(u32::MAX),
                    )
                    .with(LabelKey::Pass, 1_u32);
                for _ in 0..iters_per_thread {
                    counter_add(black_box(&OPS), black_box(context), black_box(1));
                }
            });
        }
    });
    ResultRow {
        name,
        iters: threads.saturating_mul(iters_per_thread),
        total: start.elapsed(),
    }
}

#[derive(Debug)]
struct ResultRow {
    name: &'static str,
    iters: u64,
    total: Duration,
}

impl ResultRow {
    fn ns_per_op(&self) -> u128 {
        self.total.as_nanos() / u128::from(self.iters.max(1))
    }
}
