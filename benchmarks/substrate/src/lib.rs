// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shared fixtures for the Footfall substrate benchmarks.
//!
//! The benchmarks themselves live in `benches/`, one Criterion target per
//! backend group. `footfall_core` installs one backend per process and never
//! uninstalls it, and Cargo runs every bench target in its own process, so
//! each target owns exactly one backend configuration:
//!
//! - `none`: no backend installed, measuring the bare emission seam
//! - `runtime`: `footfall_runtime` installed directly, plus the
//!   `footfall_tracing` layer as the global `tracing` subscriber
//! - `fanout`: `footfall_fanout` over the runtime and the core no-op backend
//!
//! Run them with `cargo bench -p footfall_benchmarks`.

use footfall::{
    Context, CounterDesc, EventDesc, LabelKey, MarkerKind, MarkerPhase, ResourceId,
    ResourceKindDesc, ScopeDesc, counter_add, frame_marker, resource_update, scope_begin,
    scope_end,
};
use std::{
    hint::black_box,
    sync::Barrier,
    thread,
    time::{Duration, Instant},
};

/// Label key for frame numbers.
pub static FRAME: LabelKey = LabelKey::new("frame");
/// Label key for transactions.
pub static TRANSACTION: LabelKey = LabelKey::new("transaction");
/// Label key for pass numbers.
pub static PASS: LabelKey = LabelKey::new("pass");
/// Label key for subsystems.
pub static SUBSYSTEM: LabelKey = LabelKey::new("subsystem");
/// Label key for scene nodes.
pub static NODE: LabelKey = LabelKey::new("node");
/// Label key for widgets.
pub static WIDGET: LabelKey = LabelKey::new("widget");
/// Label key for layers.
pub static LAYER: LabelKey = LabelKey::new("layer");
/// Label key for resources.
pub static RESOURCE: LabelKey = LabelKey::new("resource");
/// Label key for caches.
pub static CACHE: LabelKey = LabelKey::new("cache");

/// Counter updated by the counter cases.
pub static OPS: CounterDesc = CounterDesc::count("ops");
/// Gauge set by the gauge case.
pub static QUEUE_DEPTH: CounterDesc = CounterDesc::count("queue_depth");
/// Histogram recorded by the histogram case.
pub static LATENCY: CounterDesc = CounterDesc::count("latency");
/// Scope begun and ended by the scope cases.
pub static RECORD: ScopeDesc = ScopeDesc::new("record");
/// Resource kind updated by the resource case.
pub static ATLAS: ResourceKindDesc = ResourceKindDesc::new("atlas");
/// Event descriptor for the message cases.
pub static LOG: EventDesc = EventDesc::new("log");
/// Resource instance updated by the resource case.
pub static ATLAS_ID: ResourceId = ResourceId::new(7);

/// Criterion warmup time per case.
pub const WARM_UP: Duration = Duration::from_secs(1);
/// Criterion measurement time per case.
pub const MEASUREMENT: Duration = Duration::from_secs(3);

/// Iterations timed between runtime resets.
///
/// Criterion grows its iteration counts until a sample fills the measurement
/// time, which for fast cases would overflow any fixed event buffer and
/// measure the drop path instead. [`time_chunked`] resets the runtime, outside
/// the timed region, every `CHUNK_ITERS` iterations.
pub const CHUNK_ITERS: u64 = 1 << 16;
/// Runtime event capacity: room for one chunk of two-event cases, with margin.
pub const EVENT_CAPACITY: usize = 1 << 18;
/// Runtime counter capacity: room for the rotating-context cases.
pub const COUNTER_CAPACITY: usize = 1 << 14;
/// Distinct contexts cycled through by the rotating-context case.
pub const ROTATING_CONTEXTS: usize = 1024;
/// Thread counts for the contended counter cases.
pub const THREAD_COUNTS: [usize; 4] = [1, 2, 4, 8];
/// Frames in the seeded capture used by the snapshot and export cases.
pub const SEEDED_FRAMES: u32 = 128;

static LABEL_KEYS: [&LabelKey; 8] = [
    &FRAME,
    &TRANSACTION,
    &PASS,
    &SUBSYSTEM,
    &NODE,
    &WIDGET,
    &LAYER,
    &RESOURCE,
];

/// Returns the three-label context used by most cases.
#[must_use]
pub fn sample_context() -> Context {
    Context::root()
        .with(&FRAME, 1_u32)
        .with(&PASS, 1_u32)
        .with(&CACHE, "atlas")
}

/// Builds a context carrying `labels` labels, at most eight.
#[must_use]
pub fn build_context(labels: usize) -> Context {
    let mut context = Context::root();
    for (index, key) in LABEL_KEYS.into_iter().take(labels).enumerate() {
        context = context.with(key, u64::try_from(index).unwrap_or(u64::MAX));
    }
    context
}

/// Returns [`ROTATING_CONTEXTS`] distinct contexts derived from `base`.
#[must_use]
pub fn rotating_contexts(base: Context) -> Vec<Context> {
    (0..ROTATING_CONTEXTS)
        .map(|index| base.with(&NODE, u64::try_from(index).unwrap_or(u64::MAX)))
        .collect()
}

/// Emits a fixed [`SEEDED_FRAMES`]-frame capture through the installed backend.
pub fn seed_snapshot(context: Context) {
    for frame in 0..SEEDED_FRAMES {
        let frame_context = context.with(&FRAME, frame);
        frame_marker(MarkerKind::Frame, MarkerPhase::Begin, frame_context);
        counter_add(&OPS, frame_context, u64::from(frame));
        let token = scope_begin(&RECORD, frame_context);
        resource_update(&ATLAS, ATLAS_ID, frame_context, 4096 + u64::from(frame));
        scope_end(token);
        frame_marker(MarkerKind::Frame, MarkerPhase::End, frame_context);
    }
}

/// Times `iters` calls of `op`, calling `reset` outside the timed region
/// before every chunk of at most [`CHUNK_ITERS`] calls.
///
/// Use this from Criterion's `iter_custom` for cases that fill a runtime
/// buffer, so every timed call takes the same path.
pub fn time_chunked(iters: u64, mut reset: impl FnMut(), mut op: impl FnMut()) -> Duration {
    let mut total = Duration::ZERO;
    let mut remaining = iters;
    while remaining > 0 {
        let chunk = remaining.min(CHUNK_ITERS);
        reset();
        let start = Instant::now();
        for _ in 0..chunk {
            op();
        }
        total += start.elapsed();
        remaining -= chunk;
    }
    total
}

/// Times `threads` threads that each emit `iters` counter updates under
/// their own context.
///
/// Threads are spawned and build their contexts before the clock starts, are
/// released together through a barrier, and the clock stops when the last
/// one finishes. The returned duration is the wall time divided by the
/// thread count, so Criterion's per-iteration figure is wall time per
/// operation across all threads: a value above the single-thread cost means
/// aggregate throughput went down.
#[must_use]
pub fn time_parallel_counter_add(threads: usize, iters: u64) -> Duration {
    let start_line = Barrier::new(threads + 1);
    let finish_line = Barrier::new(threads + 1);
    let elapsed = thread::scope(|scope| {
        for thread_index in 0..threads {
            let start_line = &start_line;
            let finish_line = &finish_line;
            scope.spawn(move || {
                let context = Context::root()
                    .with(&FRAME, u64::try_from(thread_index).unwrap_or(u64::MAX))
                    .with(&PASS, 1_u32);
                start_line.wait();
                for _ in 0..iters {
                    counter_add(black_box(&OPS), black_box(context), black_box(1));
                }
                finish_line.wait();
            });
        }
        start_line.wait();
        let start = Instant::now();
        finish_line.wait();
        start.elapsed()
    });
    elapsed / u32::try_from(threads).unwrap_or(u32::MAX).max(1)
}
