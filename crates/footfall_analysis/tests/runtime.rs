// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Traces of real runtime snapshots: the questions a frame-loop
//! investigation asks, windows read one after another as a subscription
//! delivers them, events the runtime evicted, and the way from a span to
//! its code.

use core::time::Duration;
use footfall_analysis::{Continuity, GroupBy, OwnedSnapshot, Selection, Trace};
use footfall_core::{
    Args, Backend as _, Context, ContextHandle, ContextId, CounterDesc, LabelKey, MarkerKind,
    MarkerPhase, ScopeDesc,
};
use footfall_runtime::Runtime;

static FRAME: ScopeDesc = ScopeDesc::new("frame");
static LAYOUT: ScopeDesc = ScopeDesc::new("layout");
static RENDER: ScopeDesc = ScopeDesc::new("render");
static WORK: ScopeDesc = ScopeDesc::new("work");
static LOCATED: ScopeDesc = footfall_core::scope_desc!("located", "runtime::located");
static UPLOAD_BYTES: CounterDesc = CounterDesc::bytes("upload_bytes");
static PASS: LabelKey = LabelKey::new("pass");

/// How long frame 1's layout takes: long enough that no scheduling delay
/// elsewhere can rival it, so assertions about which stage and which frame
/// dominate hold on a busy machine.
const SLOW_LAYOUT: Duration = Duration::from_millis(50);

fn scope(runtime: &Runtime, desc: &'static ScopeDesc, context: ContextHandle, work: impl FnOnce()) {
    let token = runtime.scope_begin(desc, context, &Args::none());
    work();
    runtime.scope_end(token);
}

fn window(runtime: &Runtime, cursor: &mut footfall_session::EventCursor) -> OwnedSnapshot {
    let snapshot = runtime.snapshot_since(*cursor);
    *cursor = snapshot.next_cursor();
    snapshot.into()
}

#[test]
fn a_trace_answers_frame_loop_questions() {
    let runtime = Runtime::builder().event_capacity(1024).build();
    let opaque = ContextHandle::new(1);
    runtime.context_registered(opaque, &Context::root().with(&PASS, "opaque"));

    for frame in 0..3_u64 {
        runtime.frame_marker(MarkerKind::Frame, MarkerPhase::Begin, ContextHandle::ROOT);
        scope(&runtime, &FRAME, ContextHandle::ROOT, || {
            scope(&runtime, &LAYOUT, ContextHandle::ROOT, || {
                if frame == 1 {
                    std::thread::sleep(SLOW_LAYOUT);
                }
            });
            scope(&runtime, &RENDER, opaque, || {
                runtime.counter_add(&UPLOAD_BYTES, ContextHandle::ROOT, 1000 * (frame + 1));
            });
        });
        runtime.frame_marker(MarkerKind::Frame, MarkerPhase::End, ContextHandle::ROOT);
    }
    let _open = runtime.scope_begin(&RENDER, opaque, &Args::none());
    let trace = Trace::from_snapshot(&OwnedSnapshot::from(runtime.snapshot()));

    // Which stage dominates?
    let by_name = trace.spans().group_by(GroupBy::Name).table();
    assert_eq!(by_name.rows()[0].describe(), "frame");
    let layout = by_name.row("layout").expect("layout ran");
    assert_eq!(layout.stats().count(), 3);
    assert!(layout.stats().total() > by_name.row("render").expect("render ran").stats().total());
    let frame = by_name.row("frame").expect("frame ran").stats();
    assert!(
        frame.self_time() < frame.total(),
        "nested stages are not the frame's own time"
    );

    // Attribution by label.
    let by_pass = trace
        .spans()
        .group_by(GroupBy::Label("pass".into()))
        .table();
    assert_eq!(
        by_pass
            .row("pass=\"opaque\"")
            .map(|row| row.stats().count()),
        Some(3)
    );

    // Which frame was slow, what did each upload, and what ran in it?
    let frames = trace.frames();
    assert_eq!(frames.len(), 3);
    let slowest = frames
        .iter()
        .max_by_key(|frame| frame.duration())
        .expect("three frames");
    assert_eq!(slowest.id(), frames[1].id());
    let uploads: Vec<_> = frames
        .iter()
        .map(|frame| {
            frame
                .counter("upload_bytes", ContextId::new(0))
                .and_then(|counter| counter.change())
        })
        .collect();
    assert_eq!(uploads, [Some(1000), Some(2000), Some(3000)]);
    assert_eq!(frames[2].total_change("missing"), None);
    let in_slow = trace
        .spans()
        .in_frames(&[slowest.id()])
        .selecting(Selection::Inside)
        .named("layout")
        .instances(1);
    assert!(in_slow.worst()[0].time() >= SLOW_LAYOUT);

    // The still-open scope is reported, not hidden.
    assert_eq!(by_name.basis().open(), 1);
    assert_eq!(by_name.basis().unmatched_ends(), 0);
}

#[test]
fn a_window_without_the_start_of_history_reports_unknown_first_changes() {
    let runtime = Runtime::default();
    runtime.counter_add(&UPLOAD_BYTES, ContextHandle::ROOT, 5);
    runtime.frame_marker(MarkerKind::Frame, MarkerPhase::Begin, ContextHandle::ROOT);
    runtime.frame_marker(MarkerKind::Frame, MarkerPhase::End, ContextHandle::ROOT);
    let cursor = runtime.cursor();
    runtime.frame_marker(MarkerKind::Frame, MarkerPhase::Begin, ContextHandle::ROOT);
    runtime.counter_add(&UPLOAD_BYTES, ContextHandle::ROOT, 7);
    runtime.frame_marker(MarkerKind::Frame, MarkerPhase::End, ContextHandle::ROOT);

    let trace = Trace::from_snapshot(&OwnedSnapshot::from(runtime.snapshot_since(cursor)));
    let counter = trace.frames()[0]
        .counter("upload_bytes", ContextId::new(0))
        .expect("the frame updated uploads");
    assert_eq!(counter.value(), 12);
    assert_eq!(
        counter.change(),
        None,
        "the earlier sample is not in the trace"
    );
    assert!(
        trace.gaps()[0].missed() > 0,
        "the events before the cursor were never seen"
    );
}

#[test]
fn spans_and_frames_that_cross_windows_are_paired() {
    let runtime = Runtime::default();
    let mut cursor = runtime.cursor();
    let mut trace = Trace::new();

    // Window 1: a frame with an upload; a scope begins but does not end.
    runtime.frame_marker(MarkerKind::Frame, MarkerPhase::Begin, ContextHandle::ROOT);
    runtime.counter_add(&UPLOAD_BYTES, ContextHandle::ROOT, 100);
    runtime.frame_marker(MarkerKind::Frame, MarkerPhase::End, ContextHandle::ROOT);
    let token = runtime.scope_begin(&WORK, ContextHandle::ROOT, &Args::none());
    let first = trace
        .push(&window(&runtime, &mut cursor))
        .expect("one instance");
    assert_eq!(first.continuity(), Continuity::First);
    assert!(first.spans().is_empty());
    assert_eq!(first.frames().len(), 1);
    assert_eq!(trace.open_spans().len(), 1);

    // Window 2: the scope ends, and a second frame uploads 50 more.
    runtime.scope_end(token);
    runtime.frame_marker(MarkerKind::Frame, MarkerPhase::Begin, ContextHandle::ROOT);
    runtime.counter_add(&UPLOAD_BYTES, ContextHandle::ROOT, 50);
    runtime.frame_marker(MarkerKind::Frame, MarkerPhase::End, ContextHandle::ROOT);
    let second = trace
        .push(&window(&runtime, &mut cursor))
        .expect("one instance");
    assert_eq!(second.continuity(), Continuity::Continuous);
    assert_eq!(second.spans().len(), 1, "the scope begun in window 1");
    assert!(trace.open_spans().is_empty());
    let frame = trace.frame(second.frames()[0]).expect("the trace holds it");
    assert_eq!(
        frame
            .counter("upload_bytes", ContextId::new(0))
            .and_then(|counter| counter.change()),
        Some(50),
        "from the sample in window 1"
    );

    // A snapshot from another runtime starts another trace.
    assert!(trace.push(&Runtime::default().snapshot().into()).is_err());
}

#[test]
fn evicted_events_leave_a_gap_and_an_interrupted_span() {
    let runtime = Runtime::builder().event_capacity(2).build();
    let mut cursor = runtime.cursor();
    let mut trace = Trace::new();
    let token = runtime.scope_begin(&WORK, ContextHandle::ROOT, &Args::none());
    trace
        .push(&window(&runtime, &mut cursor))
        .expect("one instance");

    // Overflow the buffer so the next window misses events.
    for _ in 0..4 {
        runtime.frame_marker(MarkerKind::Frame, MarkerPhase::Begin, ContextHandle::ROOT);
    }
    runtime.scope_end(token);
    let after = trace
        .push(&window(&runtime, &mut cursor))
        .expect("one instance");
    assert!(matches!(after.continuity(), Continuity::Gap { missed } if missed > 0));
    let work = trace.span(after.spans()[0]).expect("the scope completed");
    assert!(work.interrupted(), "events went missing while it ran");
    assert!(!trace.spans().basis().is_complete());
}

#[test]
fn a_span_leads_to_its_code_in_process_and_from_a_capture() {
    let runtime = Runtime::default();
    scope(&runtime, &LOCATED, ContextHandle::ROOT, || {});
    scope(&runtime, &WORK, ContextHandle::ROOT, || {});
    scope(&runtime, &LOCATED, ContextHandle::ROOT, || {});
    let snapshot = runtime.snapshot();

    let mut file =
        footfall_session::capture::CaptureWriter::new(Vec::new()).expect("a capture starts");
    file.write_snapshot(&snapshot)
        .expect("the snapshot is written");
    let bytes = file.finish().expect("the capture ends");
    let read = footfall_session::capture::CaptureReader::new(bytes.as_slice())
        .expect("the capture opens")
        .read_snapshot()
        .expect("the snapshot reads")
        .expect("one snapshot");

    let declared = LOCATED.location().expect("scope_desc! records a location");
    for trace in [
        Trace::from_snapshot(&OwnedSnapshot::from(snapshot)),
        Trace::from_snapshot(&read),
    ] {
        let spans = trace.spans().iter();
        let located: Vec<_> = spans
            .iter()
            .filter(|selected| selected.span().name() == "located")
            .map(|selected| selected.span().location().expect("a location"))
            .collect();
        assert_eq!(located.len(), 2);
        for location in located {
            assert_eq!(location.function(), "runtime::located");
            assert_eq!(location.file(), file!());
            assert_eq!(location.line(), declared.line());
        }
        let work = spans
            .iter()
            .find(|selected| selected.span().name() == "work")
            .expect("work ran");
        assert_eq!(work.span().location(), None, "ScopeDesc::new records none");
    }
}
