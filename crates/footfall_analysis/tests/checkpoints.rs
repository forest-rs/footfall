// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Checkpoints preserve explicit baselines through capture and overlapping work.

use footfall_analysis::Trace;
use footfall_core::{
    Args, Backend as _, ContextHandle, CounterDesc, EventDesc, InstrumentationLevel, MarkerKind,
    MarkerPhase, ScopeDesc,
};
use footfall_runtime::Runtime;
use footfall_session::{
    OwnedSnapshot,
    capture::{CaptureReader, CaptureWriter},
};

static POINT: EventDesc = EventDesc::new("storage");
static BYTES: CounterDesc = CounterDesc::bytes("copied");
static LIVE: CounterDesc = CounterDesc::bytes("live");
static EDIT: ScopeDesc = ScopeDesc::new("edit");

#[test]
fn checkpoints_capture_global_interval_activity_without_exclusive_attribution() {
    let runtime = Runtime::default();
    let scope = runtime.scope_begin(&EDIT, ContextHandle::ROOT, &Args::none());
    runtime.counter_checkpoint(&POINT, ContextHandle::ROOT);
    runtime.counter_add(&BYTES, ContextHandle::ROOT, 5);
    runtime.gauge_set(&LIVE, ContextHandle::ROOT, 9);
    runtime.counter_checkpoint(&POINT, ContextHandle::ROOT);
    runtime.counter_add(&BYTES, ContextHandle::ROOT, 7);
    runtime.counter_checkpoint(&POINT, ContextHandle::ROOT);
    runtime.counter_add(&BYTES, ContextHandle::ROOT, 3);
    runtime.gauge_set(&LIVE, ContextHandle::ROOT, 4);
    runtime.counter_checkpoint(&POINT, ContextHandle::ROOT);
    runtime.counter_checkpoint(&POINT, ContextHandle::ROOT); // unchanged values still have a boundary
    runtime.scope_end(scope);
    let mut writer = CaptureWriter::new(Vec::new()).expect("capture header");
    writer
        .write_snapshot(&runtime.snapshot())
        .expect("snapshot");
    let captured = Trace::from_capture(
        CaptureReader::new(std::io::Cursor::new(writer.finish().expect("capture")))
            .expect("reader"),
    )
    .expect("trace");
    let trace = captured.trace();
    let points = trace.counter_checkpoints();
    assert_eq!(points.len(), 5);
    assert!(
        points
            .iter()
            .all(|point| point.complete() && point.span().is_some()),
        "all batches and their originating scope are retained"
    );
    let change = |start: usize, end: usize| {
        trace
            .counter_changes(points[start].sequence(), points[end].sequence())
            .expect("interval")
    };
    let a = change(0, 2);
    let b = change(1, 3);
    let bytes = |changes: &footfall_analysis::CounterChanges| {
        changes
            .counters()
            .iter()
            .find(|counter| counter.counter().name() == "copied")
            .expect("copied")
            .change()
    };
    assert_eq!(bytes(&a), Some(12));
    assert_eq!(bytes(&b), Some(10)); // the shared 7 bytes occur in both intervals
    assert_eq!(bytes(&change(3, 4)), Some(0));
    let gauge = b
        .counters()
        .iter()
        .find(|c| c.counter().name() == "live")
        .expect("gauge");
    assert_eq!(
        (gauge.before(), gauge.after(), gauge.change()),
        (Some(9), Some(4), None)
    );
    assert!(
        trace
            .counter_changes(points[3].sequence(), points[0].sequence())
            .is_none(),
        "reversed interval is invalid"
    );
    assert!(
        trace
            .counter_changes(u64::MAX, points[3].sequence())
            .is_none(),
        "unknown boundary is invalid"
    );
}

#[test]
fn missing_samples_disable_deltas_and_sampling_can_be_disabled() {
    let runtime = Runtime::builder().event_capacity(1).build();
    runtime.counter_add(&BYTES, ContextHandle::ROOT, 10);
    runtime.counter_checkpoint(&POINT, ContextHandle::ROOT);
    let trace = Trace::from_snapshot(&OwnedSnapshot::from(runtime.snapshot()));
    let point = &trace.counter_checkpoints()[0];
    assert!(
        !point.complete(),
        "the sample was evicted but its boundary survived"
    );
    assert!(
        !trace
            .counter_changes(point.sequence(), point.sequence())
            .expect("interval")
            .complete()
    );

    let quiet = Runtime::builder().counter_samples(false).build();
    quiet.counter_add(&BYTES, ContextHandle::ROOT, 1);
    quiet.counter_checkpoint(&POINT, ContextHandle::ROOT);
    assert!(
        quiet.snapshot().events().is_empty(),
        "sampling disabled means no events or scans"
    );

    let counters = Runtime::builder()
        .level(InstrumentationLevel::Counters)
        .build();
    counters.counter_add(&BYTES, ContextHandle::ROOT, 1);
    counters.counter_checkpoint(&POINT, ContextHandle::ROOT);
    let trace = Trace::from_snapshot(&OwnedSnapshot::from(counters.snapshot()));
    assert_eq!(
        trace.counter_checkpoints().len(),
        1,
        "explicit sampling needs no frame markers"
    );
}

#[test]
fn transaction_sampling_is_explicitly_configurable() {
    let runtime = Runtime::builder()
        .counter_samples_on(MarkerKind::Transaction, true)
        .counter_samples_on(MarkerKind::Frame, false)
        .build();
    runtime.frame_marker(
        MarkerKind::Transaction,
        MarkerPhase::Begin,
        ContextHandle::ROOT,
    );
    runtime.counter_add(&BYTES, ContextHandle::ROOT, 3);
    runtime.frame_marker(
        MarkerKind::Transaction,
        MarkerPhase::End,
        ContextHandle::ROOT,
    );
    let trace = Trace::from_snapshot(&OwnedSnapshot::from(runtime.snapshot()));
    assert_eq!(
        trace.marks(MarkerKind::Transaction)[0].total_change("copied"),
        Some(3)
    );
}

#[test]
fn a_checkpoint_batch_can_cross_capture_windows() {
    use footfall_runtime::{EventWindow, Snapshot};
    let runtime = Runtime::default();
    runtime.counter_add(&BYTES, ContextHandle::ROOT, 7);
    runtime.counter_checkpoint(&POINT, ContextHandle::ROOT);
    let snapshot = runtime.snapshot();
    assert_eq!(snapshot.events().len(), 2);
    let first = Snapshot::builder(snapshot.metadata().clone())
        .window(EventWindow::new(0, 1, 0))
        .captured_at_nanos(snapshot.captured_at_nanos())
        .event(snapshot.events()[0].clone())
        .build();
    let second = Snapshot::builder(snapshot.metadata().clone())
        .window(EventWindow::new(1, 2, 0))
        .captured_at_nanos(snapshot.captured_at_nanos())
        .event(snapshot.events()[1].clone())
        .build();
    let mut trace = Trace::from_snapshot(&OwnedSnapshot::from(first));
    assert!(
        trace.counter_checkpoints().is_empty(),
        "values alone do not invent a boundary"
    );
    trace
        .push(&OwnedSnapshot::from(second))
        .expect("continuation");
    let point = &trace.counter_checkpoints()[0];
    assert!(point.complete(), "batch survives a capture window boundary");
    assert_eq!(point.values()[0].value(), 7);
}
