// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Loaded-query and capture-load costs on a caller-supplied representative capture.
//! `FOOTFALL_CAPTURE=... cargo bench -p footfall_benchmarks --bench inspection`

use criterion::{Criterion, criterion_group, criterion_main};
use footfall_analysis::{FlowQuery, MessageQuery, Page, ResourceGrouping, Trace, WorkId};
use footfall_session::capture::CaptureReader;
use std::hint::black_box;

fn inspection(c: &mut Criterion) {
    let Ok(path) = std::env::var("FOOTFALL_CAPTURE") else {
        eprintln!("set FOOTFALL_CAPTURE to a representative .ffcap");
        return;
    };
    let bytes = std::fs::read(path).expect("FOOTFALL_CAPTURE must name a readable capture; use an absolute path because Cargo runs benchmarks from the package directory");
    let load = || {
        Trace::from_capture(CaptureReader::new(bytes.as_slice()).expect("reader")).expect("capture")
    };
    let captured = load();
    assert!(
        captured.complete(),
        "benchmark requires a complete file read"
    );
    let trace = captured.trace();
    let work = trace
        .spans()
        .named("procedural.edit")
        .with_arg("kind", "rejected")
        .iter();
    let id = work.first().expect("procedural rejected edit").span().id();
    let mut group = c.benchmark_group("analysis/inspection");
    group.bench_function("decode_and_construct", |b| b.iter(|| black_box(load())));
    group.bench_function("messages_loaded", |b| {
        b.iter(|| {
            black_box(
                trace
                    .select_messages(&MessageQuery::default(), Page::default())
                    .expect("query"),
            )
        });
    });
    group.bench_function("flows_loaded", |b| {
        b.iter(|| {
            black_box(
                trace
                    .select_flows(&FlowQuery::default(), Page::default())
                    .expect("query"),
            )
        });
    });
    group.bench_function("work_loaded", |b| {
        b.iter(|| black_box(trace.inspect_work(WorkId::Scope(id)).expect("work")));
    });
    group.bench_function("resources_loaded", |b| {
        b.iter(|| {
            black_box(trace.resource_interval(trace.observed(), Some(ResourceGrouping::Kind)))
        });
    });
    group.finish();
}
criterion_group!(benches, inspection);
criterion_main!(benches);
