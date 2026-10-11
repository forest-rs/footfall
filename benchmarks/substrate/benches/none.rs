// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The emission seam with no backend installed.
//!
//! This target never installs a backend, so every call takes the built-in
//! no-op path. It runs in its own process, separate from the targets that
//! install one.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use footfall::{
    RegisteredContext, counter_add, message, register_context, release_context, scope, scope_begin,
    scope_end,
};
use footfall_benchmarks::{LOG, MEASUREMENT, OPS, RECORD, WARM_UP, build_context, sample_context};
use std::hint::black_box;

fn emission(c: &mut Criterion) {
    let registered = RegisteredContext::new(&sample_context());
    let context = registered.handle();
    let mut group = c.benchmark_group("none");
    group.bench_function("counter_add", |b| {
        b.iter(|| counter_add(black_box(&OPS), black_box(context), black_box(1)));
    });
    group.bench_function("scope begin+end", |b| {
        b.iter(|| {
            let token = scope_begin(black_box(&RECORD), black_box(context));
            scope_end(black_box(token));
        });
    });
    group.bench_function("scope guard", |b| {
        b.iter(|| drop(black_box(scope(black_box(&RECORD), black_box(context)))));
    });
    group.bench_function("message", |b| {
        b.iter(|| {
            message(
                black_box(&LOG),
                black_box(context),
                black_box("uploaded atlas"),
            );
        });
    });
    let built = sample_context();
    group.bench_function("context register+release", |b| {
        b.iter(|| release_context(black_box(register_context(black_box(&built)))));
    });
    group.finish();
}

fn context_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("none/context build");
    for labels in [1, 4, 8] {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{labels} labels")),
            &labels,
            |b, &labels| b.iter(|| build_context(black_box(labels))),
        );
    }
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().warm_up_time(WARM_UP).measurement_time(MEASUREMENT);
    targets = emission, context_build
}
criterion_main!(benches);
