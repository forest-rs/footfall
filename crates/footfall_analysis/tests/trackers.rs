// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Trackers follow scopes and frames across consecutive windows, such as a
//! subscription delivers.

use footfall_analysis::{Continuity, FrameTracker, OwnedSnapshot, SpanTracker};
use footfall_core::{
    Args, Backend as _, ContextHandle, ContextId, CounterDesc, MarkerKind, MarkerPhase, ScopeDesc,
};
use footfall_runtime::Runtime;

static WORK: ScopeDesc = ScopeDesc::new("work");
static UPLOADS: CounterDesc = CounterDesc::bytes("uploads");

fn window(runtime: &Runtime, cursor: &mut footfall_session::EventCursor) -> OwnedSnapshot {
    let snapshot = runtime.snapshot_since(*cursor);
    *cursor = snapshot.next_cursor();
    snapshot.into()
}

#[test]
fn scopes_and_frames_that_span_windows_are_paired() {
    let runtime = Runtime::default();
    let mut cursor = runtime.cursor();
    let mut spans = SpanTracker::new();
    let mut frames = FrameTracker::new();

    // Window 1: a frame with an upload; a scope begins but does not end.
    runtime.frame_marker(MarkerKind::Frame, MarkerPhase::Begin, ContextHandle::ROOT);
    runtime.counter_add(&UPLOADS, ContextHandle::ROOT, 100);
    runtime.frame_marker(MarkerKind::Frame, MarkerPhase::End, ContextHandle::ROOT);
    let token = runtime.scope_begin(&WORK, ContextHandle::ROOT, &Args::none());
    let first = window(&runtime, &mut cursor);
    let opened = spans.push(&first);
    assert_eq!(spans.continuity(), Continuity::First);
    assert!(opened.spans().is_empty());
    assert_eq!(opened.open().len(), 1);
    let first_frames = frames.push(&first);
    assert_eq!(first_frames.len(), 1);

    // Window 2: the scope ends, and a second frame uploads 50 more.
    runtime.scope_end(token);
    runtime.frame_marker(MarkerKind::Frame, MarkerPhase::Begin, ContextHandle::ROOT);
    runtime.counter_add(&UPLOADS, ContextHandle::ROOT, 50);
    runtime.frame_marker(MarkerKind::Frame, MarkerPhase::End, ContextHandle::ROOT);
    let second = window(&runtime, &mut cursor);
    let closed = spans.push(&second);
    assert_eq!(spans.continuity(), Continuity::Continuous);
    assert_eq!(closed.spans().len(), 1, "the scope begun in window 1");
    assert_eq!(closed.unmatched_ends(), 0);
    let second_frames = frames.push(&second);
    let change = second_frames[0]
        .counter("uploads", ContextId::new(0))
        .and_then(|counter| counter.change());
    assert_eq!(change, Some(50), "from the sample in window 1");
    assert!(second_frames[0].sequence() > first_frames[0].sequence());

    // A snapshot from another runtime restarts the trackers.
    let other = Runtime::default();
    spans.push(&other.snapshot().into());
    assert_eq!(spans.continuity(), Continuity::Restarted);
}

#[test]
fn trackers_start_over_after_evicted_events() {
    let runtime = Runtime::builder().event_capacity(2).build();
    let mut cursor = runtime.cursor();
    let mut spans = SpanTracker::new();
    let token = runtime.scope_begin(&WORK, ContextHandle::ROOT, &Args::none());
    spans.push(&window(&runtime, &mut cursor));

    // Overflow the buffer so the next window misses events.
    for _ in 0..4 {
        runtime.frame_marker(MarkerKind::Frame, MarkerPhase::Begin, ContextHandle::ROOT);
    }
    runtime.scope_end(token);
    let after_gap = spans.push(&window(&runtime, &mut cursor));
    assert_eq!(spans.continuity(), Continuity::Gap);
    assert!(
        after_gap.spans().is_empty(),
        "the begin is not trusted across a gap"
    );
    assert_eq!(after_gap.unmatched_ends(), 1);
}
