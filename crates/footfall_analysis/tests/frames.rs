// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The questions a frame-loop investigation asks: which stage dominates,
//! which frames were slow, and how much each frame uploaded.

use footfall_analysis::{Spans, frames};
use footfall_core::{
    Args, Backend as _, Context, ContextHandle, CounterDesc, LabelKey, MarkerKind, MarkerPhase,
    ScopeDesc,
};
use footfall_runtime::Runtime;
use footfall_session::{OwnedLabelValue, OwnedSnapshot};

static FRAME: ScopeDesc = ScopeDesc::new("frame");
static LAYOUT: ScopeDesc = ScopeDesc::new("layout");
static RENDER: ScopeDesc = ScopeDesc::new("render");
static UPLOAD_BYTES: CounterDesc = CounterDesc::bytes("upload_bytes");
static PASS: LabelKey = LabelKey::new("pass");

/// Records scopes through the runtime's backend entry points.
///
/// Only frame 1's layout sleeps, and for long enough that no scheduling
/// delay elsewhere can rival it, so the assertions about which stage and
/// which frame dominate hold on a busy machine.
struct Recorder<'a> {
    runtime: &'a Runtime,
}

impl Recorder<'_> {
    fn scope(&self, desc: &'static ScopeDesc, context: ContextHandle, work: impl FnOnce()) {
        let token = self.runtime.scope_begin(desc, context, &Args::none());
        work();
        self.runtime.scope_end(token);
    }
}

/// How long frame 1's layout takes.
const SLOW_LAYOUT_MILLIS: u64 = 50;

#[test]
fn spans_and_frames_answer_frame_loop_questions() {
    let runtime = Runtime::builder().event_capacity(1024).build();
    let recorder = Recorder { runtime: &runtime };
    let opaque = ContextHandle::new(1);
    runtime.context_registered(opaque, &Context::root().with(&PASS, "opaque"));

    for frame in 0..3_u64 {
        runtime.frame_marker(MarkerKind::Frame, MarkerPhase::Begin, ContextHandle::ROOT);
        recorder.scope(&FRAME, ContextHandle::ROOT, || {
            recorder.scope(&LAYOUT, ContextHandle::ROOT, || {
                // Frame 1 does a slow layout.
                if frame == 1 {
                    std::thread::sleep(std::time::Duration::from_millis(SLOW_LAYOUT_MILLIS));
                }
            });
            recorder.scope(&RENDER, opaque, || {
                runtime.counter_add(&UPLOAD_BYTES, ContextHandle::ROOT, 1000 * (frame + 1));
            });
        });
        runtime.frame_marker(MarkerKind::Frame, MarkerPhase::End, ContextHandle::ROOT);
    }
    // A scope still open when the snapshot is taken.
    let _open = runtime.scope_begin(&RENDER, opaque, &Args::none());

    let snapshot = OwnedSnapshot::from(runtime.snapshot());
    let spans = Spans::from_snapshot(&snapshot);

    // Which stage dominates?
    let by_name = spans.by_name();
    assert_eq!(by_name["layout"].count(), 3);
    assert!(by_name["layout"].total_micros() > by_name["render"].total_micros());
    assert!(
        by_name["frame"].self_micros() < by_name["frame"].total_micros(),
        "nested stages are not the frame's own time"
    );
    let layout = spans.spans().iter().find(|span| span.name() == "layout");
    assert_eq!(layout.map(|span| span.depth()), Some(1));

    // Attribution by label.
    let by_pass = spans.by_label(&snapshot, "pass");
    let opaque_pass = &by_pass[&OwnedLabelValue::String(String::from("opaque"))];
    assert_eq!(opaque_pass["render"].count(), 3);

    // Which frames were slow, and what did each upload?
    let frames = frames(&snapshot);
    assert_eq!(frames.len(), 3);
    let slowest = frames
        .iter()
        .enumerate()
        .max_by_key(|(_, frame)| frame.duration_micros())
        .map(|(index, _)| index);
    assert_eq!(slowest, Some(1));
    let uploads: Vec<_> = frames
        .iter()
        .map(|frame| {
            frame
                .counter("upload_bytes", footfall_core::ContextId::new(0))
                .and_then(|counter| counter.change())
        })
        .collect();
    assert_eq!(uploads, [Some(1000), Some(2000), Some(3000)]);
    assert_eq!(frames[2].total_change("upload_bytes"), Some(3000));
    assert_eq!(frames[2].total_change("missing"), None);

    // The slow frame's spans are its own.
    let slow = &frames[1];
    assert!(
        spans
            .within(slow.begin_micros(), slow.end_micros())
            .any(|span| span.name() == "layout"
                && span.duration_micros() >= SLOW_LAYOUT_MILLIS * 1000)
    );

    // Scopes cut off by the window are reported, not hidden.
    assert_eq!(spans.open().len(), 1);
    assert_eq!(spans.unmatched_ends(), 0);
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

    let window = OwnedSnapshot::from(runtime.snapshot_since(cursor));
    let frames = frames(&window);
    let counter = frames[0]
        .counter("upload_bytes", footfall_core::ContextId::new(0))
        .expect("the frame updated uploads");
    assert_eq!(counter.value(), 12);
    assert_eq!(
        counter.change(),
        None,
        "the window cannot see the previous sample"
    );
}
