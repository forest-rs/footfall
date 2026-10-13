// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The trace model on hand-built snapshots, with exact times: nesting and
//! identity, how spans count toward ranges, grouping by labels and
//! arguments, what answers cover, gaps, comparisons, and what a timeline
//! draws.

use core::time::Duration;
use footfall_analysis::{
    Caveat, Comparison, Continuity, DurationStats, FrameId, GroupBy, KeyPart, Metric,
    OwnedLabelValue, Population, PushError, Recorded, Selection, SpanId, TimeRange, Trace, Zone,
};
use footfall_core::{
    Args, ClockDesc, ClockReading, Context, ContextId, CounterDesc, EventDesc, FlowId, FlowPhase,
    InstrumentationLevel, LabelKey, MarkerKind, MarkerPhase, ResourceId, ResourceKindDesc,
    ScopeDesc, ScopeToken, TickPeriod, TimelineDesc, TimelineSpan,
};
use footfall_runtime::{
    CaptureSettings, ClockTicks, ContextSnapshot, CounterKind, EventWindow, Observation, Snapshot,
    SnapshotMetadata, ThreadSnapshot, TimedObservation, TimelineWork,
};
use footfall_session::OwnedSnapshot;

static UPDATE: ScopeDesc = ScopeDesc::new("update");
static LAYOUT: ScopeDesc = ScopeDesc::new("layout");
static PAINT: ScopeDesc = ScopeDesc::new("paint");
static DECODE: ScopeDesc = ScopeDesc::new("decode");
static TASK: ScopeDesc = ScopeDesc::new("task");
static LOG: EventDesc = EventDesc::new("log");
static UPLOADS: CounterDesc = CounterDesc::bytes("uploads");
static TEXTURE: ResourceKindDesc = ResourceKindDesc::new("texture");
static PASS: LabelKey = LabelKey::new("pass");
static VIEW: LabelKey = LabelKey::new("view");

const MAIN: u64 = 1;
const WORKER: u64 = 2;

fn us(micros: u64) -> Duration {
    Duration::from_micros(micros)
}

fn range(start: u64, end: u64) -> TimeRange {
    TimeRange::new(us(start), us(end))
}

/// Events recorded by hand, with sequence numbers in call order. Times are
/// given in microseconds.
struct Recording {
    instance: u64,
    level: InstrumentationLevel,
    events: Vec<TimedObservation>,
}

impl Recording {
    fn new(instance: u64) -> Self {
        Self {
            instance,
            level: InstrumentationLevel::Trace,
            events: Vec::new(),
        }
    }

    fn next(&self) -> u64 {
        self.events.len() as u64
    }

    fn record(&mut self, at: u64, thread: u64, observation: Observation) -> u64 {
        let sequence = self.next();
        self.events.push(TimedObservation::new(
            at * 1000,
            sequence,
            thread,
            observation,
        ));
        sequence
    }

    fn begin(
        &mut self,
        at: u64,
        thread: u64,
        desc: &'static ScopeDesc,
        context: u64,
    ) -> ScopeToken {
        self.begin_with(at, thread, desc, context, Args::none())
    }

    fn begin_with(
        &mut self,
        at: u64,
        thread: u64,
        desc: &'static ScopeDesc,
        context: u64,
        args: Args,
    ) -> ScopeToken {
        let token = ScopeToken::new(self.next() + 1);
        self.record(
            at,
            thread,
            Observation::ScopeBegin {
                desc_id: desc.id(),
                name: desc.name(),
                location: desc.location(),
                context_id: ContextId::new(context),
                token,
                args,
            },
        );
        token
    }

    fn end(&mut self, at: u64, thread: u64, token: ScopeToken) {
        self.record(at, thread, Observation::ScopeEnd { token });
    }

    /// Records a scope with nothing nested in it, returning its id.
    fn scope(
        &mut self,
        thread: u64,
        desc: &'static ScopeDesc,
        context: u64,
        begin: u64,
        end: u64,
    ) -> SpanId {
        self.scope_with(thread, desc, context, begin, end, Args::none())
    }

    /// Records a scope with arguments and nothing nested in it, returning
    /// its id.
    fn scope_with(
        &mut self,
        thread: u64,
        desc: &'static ScopeDesc,
        context: u64,
        begin: u64,
        end: u64,
        args: Args,
    ) -> SpanId {
        let id = SpanId::new(self.next());
        let token = self.begin_with(begin, thread, desc, context, args);
        self.end(end, thread, token);
        id
    }

    fn marker(&mut self, at: u64, phase: MarkerPhase) -> u64 {
        self.numbered_marker(at, phase, None)
    }

    fn numbered_marker(&mut self, at: u64, phase: MarkerPhase, number: Option<u64>) -> u64 {
        self.record(
            at,
            MAIN,
            Observation::FrameMarker {
                kind: MarkerKind::Frame,
                phase,
                number,
                context_id: ContextId::new(0),
            },
        )
    }

    fn sample(&mut self, at: u64, value: u64) {
        self.record(
            at,
            MAIN,
            Observation::CounterSample {
                desc_id: UPLOADS.id(),
                name: UPLOADS.name(),
                unit: UPLOADS.unit(),
                context_id: ContextId::new(0),
                kind: CounterKind::Counter,
                value,
            },
        );
    }

    /// Records that `clock` read `ticks` nanosecond ticks at `at` µs.
    fn clock_sync(&mut self, at: u64, clock: &'static ClockDesc, ticks: u64) {
        self.record(
            at,
            MAIN,
            Observation::ClockSync {
                clock: ClockTicks::from(ClockReading::new(clock, ticks, TickPeriod::NANOSECOND)),
            },
        );
    }

    /// Reports finished GPU work at `at` µs, timed in `clock`'s ticks.
    fn timeline_span(&mut self, at: u64, span: &TimelineSpan) -> SpanId {
        let id = SpanId::new(self.next());
        self.record(
            at,
            MAIN,
            Observation::TimelineSpan {
                span: Box::new(TimelineWork::new(span)),
            },
        );
        id
    }

    /// A snapshot holding the events in `[since, next)` except `missing`.
    fn window(&self, since: u64, next: u64, missing: &[u64], captured_at: u64) -> OwnedSnapshot {
        let capture = CaptureSettings::new(self.level, 1024, 1024, 64).with_counter_samples(true);
        let metadata = SnapshotMetadata::new("test")
            .with_instance_id(self.instance)
            .with_producer_version("1.0.0")
            .with_capture(capture);
        let mut builder = Snapshot::builder(metadata)
            .captured_at_nanos(captured_at * 1000)
            .window(EventWindow::new(since, next, missing.len() as u64))
            .thread(ThreadSnapshot::new(MAIN, Some(String::from("main"))))
            .thread(ThreadSnapshot::new(WORKER, None))
            .context(ContextSnapshot::new(ContextId::new(0), Context::root()))
            .context(ContextSnapshot::new(
                ContextId::new(1),
                Context::root().with(&PASS, "main"),
            ))
            .context(ContextSnapshot::new(
                ContextId::new(2),
                Context::root().with(&VIEW, 3_u64),
            ));
        for event in &self.events {
            let sequence = event.sequence();
            if (since..next).contains(&sequence) && !missing.contains(&sequence) {
                builder = builder.event(event.clone());
            }
        }
        OwnedSnapshot::from(builder.build())
    }

    fn snapshot(&self, captured_at: u64) -> OwnedSnapshot {
        self.window(0, self.next(), &[], captured_at)
    }
}

#[test]
fn spans_nest_and_keep_their_identity() {
    let mut recording = Recording::new(7);
    let frame_begin = recording.marker(0, MarkerPhase::Begin);
    let update_id = SpanId::new(recording.next());
    let update = recording.begin(10, MAIN, &UPDATE, 0);
    let layout = recording.scope(MAIN, &LAYOUT, 0, 20, 50);
    let decode = recording.scope(WORKER, &DECODE, 0, 30, 70);
    let paint = recording.scope(MAIN, &PAINT, 0, 60, 80);
    recording.end(90, MAIN, update);
    recording.marker(100, MarkerPhase::End);
    let trace = Trace::from_snapshot(&recording.snapshot(100));

    let outer = trace.span(update_id).expect("the trace holds the span");
    assert_eq!(outer.name(), "update");
    assert_eq!(outer.duration(), us(80));
    assert_eq!(
        outer.self_time(),
        us(30),
        "80 less layout's 30 and paint's 20"
    );
    assert_eq!(outer.depth(), 0);
    let children: Vec<_> = trace.children(outer).iter().map(|span| span.id()).collect();
    assert_eq!(children, [layout, paint]);

    let inner = trace.span(layout).expect("layout is held");
    assert_eq!(inner.depth(), 1);
    assert_eq!(inner.parent(), Some(update_id));
    assert_eq!(trace.parent(inner).map(|span| span.id()), Some(update_id));
    assert_eq!(inner.id().sequence(), layout.sequence());
    assert_eq!(
        trace.span(decode).map(|span| span.parent()),
        Some(None),
        "nesting is per thread"
    );

    let main = trace.track(MAIN).expect("main has a track");
    assert_eq!(main.name(), Some("main"));
    assert_eq!(main.depth(), 2);
    let visible: Vec<_> = main
        .spans_in(1, range(55, 65))
        .map(|span| span.id())
        .collect();
    assert_eq!(visible, [paint]);

    let frame = trace.frame_at(us(25)).expect("a frame was in progress");
    assert_eq!(frame.id(), FrameId::new(frame_begin));
    assert_eq!(frame.duration(), us(100));
    assert!(trace.frame_at(us(150)).is_none());
}

#[test]
fn numbered_frames_pair_by_number_and_can_be_found_by_it() {
    let mut recording = Recording::new(7);
    // Two frames in flight at once, ending in the order they began: an
    // unnumbered end would close the later begin.
    let first = recording.numbered_marker(0, MarkerPhase::Begin, Some(1234));
    let second = recording.numbered_marker(10, MarkerPhase::Begin, Some(1235));
    recording.numbered_marker(20, MarkerPhase::End, Some(1234));
    recording.numbered_marker(30, MarkerPhase::End, Some(1235));
    let trace = Trace::from_snapshot(&recording.snapshot(40));

    let frame = trace.frame_numbered(1234).expect("frame 1234 completed");
    assert_eq!(frame.id(), FrameId::new(first));
    assert_eq!(frame.number(), Some(1234));
    assert_eq!(frame.range(), range(0, 20));
    let later = trace.frame_numbered(1235).expect("frame 1235 completed");
    assert_eq!(later.id(), FrameId::new(second));
    assert_eq!(later.range(), range(10, 30));
    assert!(trace.frame_numbered(1236).is_none());
}

#[test]
fn selection_decides_how_spans_crossing_an_edge_count() {
    let mut recording = Recording::new(7);
    let first = recording.marker(0, MarkerPhase::Begin);
    let task = recording.begin(90, MAIN, &TASK, 0);
    recording.scope(MAIN, &LAYOUT, 0, 95, 105);
    recording.marker(100, MarkerPhase::End);
    let second = recording.marker(100, MarkerPhase::Begin);
    recording.end(130, MAIN, task);
    recording.marker(200, MarkerPhase::End);
    let trace = Trace::from_snapshot(&recording.snapshot(200));
    let (first, second) = (FrameId::new(first), FrameId::new(second));

    let totals = |frames: &[FrameId], selection| {
        let table = trace
            .spans()
            .in_frames(frames)
            .selecting(selection)
            .group_by(GroupBy::Name)
            .table();
        let get = |name: &str| {
            table
                .row(name)
                .map(|row| (row.stats().total(), row.stats().self_time()))
        };
        (get("task"), get("layout"))
    };

    // Counted whole, in the frame each began in.
    assert_eq!(
        totals(&[first], Selection::StartsWithin),
        (Some((us(40), us(30))), Some((us(10), us(10))))
    );
    assert_eq!(totals(&[second], Selection::StartsWithin), (None, None));
    // Counted whole wherever they ran.
    assert_eq!(
        totals(&[second], Selection::Overlaps),
        (Some((us(40), us(30))), Some((us(10), us(10))))
    );
    // Only the time inside each frame, with self time clipped too.
    assert_eq!(
        totals(&[first], Selection::Inside),
        (Some((us(10), us(5))), Some((us(5), us(5))))
    );
    assert_eq!(
        totals(&[second], Selection::Inside),
        (Some((us(30), us(25))), Some((us(5), us(5))))
    );
    // Adjacent frames together count each span once, whole.
    assert_eq!(
        totals(&[first, second], Selection::Overlaps),
        (Some((us(40), us(30))), Some((us(10), us(10))))
    );
}

#[test]
fn spans_without_a_label_form_their_own_group() {
    let mut recording = Recording::new(7);
    recording.scope(MAIN, &LAYOUT, 1, 0, 10);
    recording.scope(MAIN, &PAINT, 2, 10, 30);
    recording.scope(MAIN, &DECODE, 0, 30, 35);
    let trace = Trace::from_snapshot(&recording.snapshot(40));

    let table = trace
        .spans()
        .group_by(GroupBy::Label("pass".into()))
        .table();
    let rows: Vec<_> = table
        .rows()
        .iter()
        .map(|row| (row.describe(), row.stats().total()))
        .collect();
    assert_eq!(
        rows,
        [
            (String::from("(no pass)"), us(25)),
            (String::from("pass=\"main\""), us(10)),
        ]
    );
    assert_eq!(table.total().total(), us(35), "no span is left out");
    assert_eq!(
        table.rows()[1].key(),
        [KeyPart::Label {
            key: "pass".into(),
            value: Some("main".into())
        }]
    );

    let labelled = trace.spans().with_label("pass", "main").table();
    assert_eq!(labelled.total().count(), 1);
    let unlabelled = trace.spans().without_label("pass").table();
    assert_eq!(unlabelled.total().count(), 2);
    let by_context = trace.spans().group_by(GroupBy::Context).table();
    assert!(by_context.row("view=3").is_some());
    assert!(by_context.row("root").is_some());
}

#[test]
fn spans_group_by_an_argument_and_those_without_it_form_their_own_group() {
    let mut recording = Recording::new(7);
    let nodes = |count: u64| Args::none().with("nodes", count);
    recording.scope_with(MAIN, &LAYOUT, 0, 0, 10, nodes(800));
    recording.scope_with(MAIN, &LAYOUT, 0, 10, 30, nodes(800));
    let storm = recording.scope_with(MAIN, &LAYOUT, 0, 30, 130, nodes(12_000));
    recording.scope_with(
        MAIN,
        &DECODE,
        0,
        130,
        132,
        Args::none().with("format", "png"),
    );
    recording.scope(MAIN, &PAINT, 0, 132, 137);
    // Still running, so counted only as open.
    recording.begin_with(140, MAIN, &LAYOUT, 0, nodes(12_000));
    let trace = Trace::from_snapshot(&recording.snapshot(150));

    let span = trace.span(storm).expect("the storm's layout is held");
    assert_eq!(span.arg("nodes"), Some(&OwnedLabelValue::Unsigned(12_000)));
    assert_eq!(span.arg("glyphs"), None);

    let table = trace
        .spans()
        .group_by(GroupBy::Name)
        .group_by(GroupBy::Arg("nodes".into()))
        .table();
    let rows: Vec<_> = table
        .rows()
        .iter()
        .map(|row| {
            (
                row.describe(),
                row.stats().count(),
                row.stats().total(),
                row.stats().time().max(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            (String::from("layout / nodes=12000"), 1, us(100), us(100)),
            (String::from("layout / nodes=800"), 2, us(30), us(20)),
            (String::from("paint / (no nodes)"), 1, us(5), us(5)),
            (String::from("decode / (no nodes)"), 1, us(2), us(2)),
        ]
    );
    assert_eq!(table.total().total(), us(137), "no span is left out");
    let storm_key = KeyPart::Arg {
        key: "nodes".into(),
        value: Some(12_000_u64.into()),
    };
    assert_eq!(table.rows()[0].key()[1], storm_key);
    assert_eq!(storm_key.semantic(), "nodes=12000");
    assert_eq!(table.rows()[2].key()[1].semantic(), "(no nodes)");

    // String values read as label values do: quoted.
    let by_format = trace
        .spans()
        .group_by(GroupBy::Arg("format".into()))
        .table();
    assert_eq!(
        by_format
            .row("format=\"png\"")
            .map(|row| row.stats().count()),
        Some(1)
    );

    let storms = trace.spans().with_arg("nodes", 12_000_u64);
    assert_eq!(storms.table().total().count(), 1);
    assert_eq!(storms.basis().open(), 1, "the open layout had 12000 nodes");
    let usual = trace.spans().with_arg("nodes", 800_u64);
    assert_eq!(usual.table().total().total(), us(30));
    assert_eq!(usual.basis().open(), 0);
    let without = trace.spans().without_arg("nodes").table();
    assert_eq!(without.total().count(), 2, "paint and decode");
    assert_eq!(
        trace
            .spans()
            .with_arg("nodes", 800_i64)
            .table()
            .total()
            .count(),
        0,
        "values match by kind as well as number"
    );
}

#[test]
fn an_answer_says_what_it_left_out_and_whether_anything_was_recorded() {
    let mut recording = Recording::new(7);
    recording.scope(MAIN, &LAYOUT, 0, 0, 10);
    recording.begin(20, MAIN, &PAINT, 0);
    recording.end(30, WORKER, ScopeToken::new(999));
    let trace = Trace::from_snapshot(&recording.snapshot(40));

    let table = trace.spans().table();
    let basis = table.basis();
    assert_eq!(basis.recorded(), Recorded::Yes);
    assert_eq!(basis.spans(), 1);
    assert_eq!(basis.open(), 1, "paint has not ended");
    assert_eq!(
        basis.unmatched_ends(),
        1,
        "an end whose begin the trace never saw"
    );
    assert!(!basis.is_complete());
    assert_eq!(trace.open_spans()[0].name(), "paint");
    let painting = trace.spans().named("paint").table();
    assert_eq!(painting.total().count(), 0);
    assert_eq!(
        painting.basis().open(),
        1,
        "zero completed, one still running"
    );

    // A producer capturing only counters records no spans: an empty answer
    // that says why.
    recording.level = InstrumentationLevel::Counters;
    let counters_only = Trace::from_snapshot(&recording.window(0, 0, &[], 40));
    let table = counters_only.spans().table();
    assert!(table.rows().is_empty());
    assert_eq!(
        table.basis().recorded(),
        Recorded::BelowLevel {
            level: InstrumentationLevel::Counters,
            needs: InstrumentationLevel::Scoped
        }
    );
    assert!(table.basis().to_string().contains("not recorded"));
}

#[test]
fn missing_events_become_gaps_and_interrupt_open_spans() {
    let mut recording = Recording::new(7);
    // Window 1: a frame with an upload, and a scope that stays open.
    recording.marker(0, MarkerPhase::Begin);
    recording.marker(10, MarkerPhase::End);
    recording.sample(10, 5);
    let long_id = SpanId::new(recording.next());
    let long = recording.begin(20, MAIN, &TASK, 0);
    let first_next = recording.next();
    // Window 2 misses the next two events.
    recording.scope(MAIN, &LAYOUT, 0, 30, 40);
    let after = recording.scope(MAIN, &PAINT, 0, 60, 70);
    recording.marker(75, MarkerPhase::Begin);
    recording.marker(80, MarkerPhase::End);
    recording.sample(80, 9);
    recording.end(100, MAIN, long);

    let mut trace = Trace::new();
    let first = trace
        .push(&recording.window(0, first_next, &[], 25))
        .expect("the first window is accepted");
    assert_eq!(first.continuity(), Continuity::First);
    let second = trace
        .push(&recording.window(
            first_next,
            recording.next(),
            &[first_next, first_next + 1],
            120,
        ))
        .expect("the second window is accepted");
    assert_eq!(second.continuity(), Continuity::Gap { missed: 2 });
    assert_eq!(trace.gaps().len(), 1);
    assert_eq!(trace.gaps()[0].missed(), 2);
    assert_eq!(trace.gaps()[0].range(), range(20, 60));

    let long = trace.span(long_id).expect("the pairing survives the gap");
    assert_eq!(long.duration(), us(80), "its duration is exact");
    assert!(long.interrupted(), "but layout, nested in it, went missing");
    let after = trace.span(after).expect("paint was recorded after the gap");
    assert!(!after.interrupted());
    assert_eq!(after.depth(), 0, "nesting starts over after a gap");

    let frames = trace.frames();
    assert_eq!(frames[0].total_change("uploads"), Some(5));
    assert_eq!(
        frames[1].total_change("uploads"),
        None,
        "uploads during the gap are unknown"
    );

    let basis = trace.spans().within(range(0, 200)).table().basis().clone();
    assert_eq!(basis.missed_events(), 2);
    assert_eq!(basis.interrupted(), 1);
    assert_eq!(basis.unobserved(), us(80), "the trace ends at 120");
    assert!(!basis.is_complete());
    let clean = trace.spans().within(range(60, 75)).table();
    assert!(clean.basis().gaps().is_empty());
}

#[test]
fn snapshots_may_overlap_but_must_come_from_one_instance() {
    let mut recording = Recording::new(7);
    recording.scope(MAIN, &LAYOUT, 0, 0, 10);
    let snapshot = recording.snapshot(20);
    let mut trace = Trace::from_snapshot(&snapshot);
    let again = trace.push(&snapshot).expect("the same instance");
    assert_eq!(again.continuity(), Continuity::Continuous);
    assert!(again.spans().is_empty(), "nothing new");
    assert_eq!(trace.spans().table().total().count(), 1);

    let other = Recording::new(8).snapshot(20);
    assert_eq!(
        trace.push(&other),
        Err(PushError::OtherInstance {
            expected: 7,
            found: 8
        })
    );
}

/// Records `normal` frames with one layout and one paint, then `slow`
/// frames with three layouts each as fast and one paint twice as slow.
fn frames_of_two_kinds(
    recording: &mut Recording,
    normal: u64,
    slow: u64,
) -> (Vec<FrameId>, Vec<FrameId>) {
    let mut normal_frames = Vec::new();
    let mut slow_frames = Vec::new();
    for index in 0..normal + slow {
        let start = index * 1000;
        let id = FrameId::new(recording.marker(start, MarkerPhase::Begin));
        if index < normal {
            recording.scope(MAIN, &LAYOUT, 0, start + 10, start + 20);
            recording.scope(MAIN, &PAINT, 0, start + 30, start + 50);
            normal_frames.push(id);
        } else {
            for call in 0..3 {
                recording.scope(
                    MAIN,
                    &LAYOUT,
                    0,
                    start + 10 + call * 20,
                    start + 20 + call * 20,
                );
            }
            recording.scope(MAIN, &PAINT, 0, start + 100, start + 140);
            slow_frames.push(id);
        }
        recording.marker(start + 500, MarkerPhase::End);
    }
    (normal_frames, slow_frames)
}

#[test]
fn a_comparison_separates_more_work_from_slower_work() {
    let mut recording = Recording::new(7);
    let (normal, slow) = frames_of_two_kinds(&mut recording, 4, 2);
    let trace = Trace::from_snapshot(&recording.snapshot(6000));
    let frames =
        |ids: &[FrameId]| Population::frames(&trace, ids.iter().filter_map(|id| trace.frame(*id)));

    let comparison = Comparison::between(&frames(&normal), &frames(&slow))
        .selecting(Selection::StartsWithin)
        .group_by(GroupBy::Name)
        .run();
    assert!(
        comparison.caveats().is_empty(),
        "{:?}",
        comparison.caveats()
    );
    assert_eq!(comparison.baseline().units(), 4);
    assert_eq!(comparison.candidate().units(), 2);

    let layout = comparison.row("layout").expect("layout ran in both");
    assert_eq!(layout.baseline().count_per_unit(), 1.0);
    assert_eq!(layout.candidate().count_per_unit(), 3.0);
    assert_eq!(layout.change_micros(), 20.0);
    assert_eq!(layout.volume_micros(), 20.0, "three calls instead of one");
    assert_eq!(layout.cost_micros(), 0.0, "each as fast");

    let paint = comparison.row("paint").expect("paint ran in both");
    assert_eq!(paint.change_micros(), 20.0);
    assert_eq!(paint.volume_micros(), 0.0, "one call either way");
    assert_eq!(paint.cost_micros(), 20.0, "each twice as slow");
    assert_eq!(paint.standard_error_micros(), Some(0.0));
}

#[test]
fn grouping_by_an_argument_turns_slower_work_into_different_work() {
    let mut recording = Recording::new(7);
    let mut normal = Vec::new();
    let mut slow = Vec::new();
    for index in 0..6 {
        let start = index * 1000;
        let id = FrameId::new(recording.marker(start, MarkerPhase::Begin));
        // Two of six frames lay out a hundred times the nodes, ten times
        // slower.
        let (nodes, length) = if index % 3 == 2 {
            slow.push(id);
            (12_000_u64, 100)
        } else {
            normal.push(id);
            (120_u64, 10)
        };
        recording.scope_with(
            MAIN,
            &LAYOUT,
            0,
            start + 10,
            start + 10 + length,
            Args::none().with("nodes", nodes),
        );
        recording.scope(MAIN, &PAINT, 0, start + 200, start + 220);
        recording.marker(start + 500, MarkerPhase::End);
    }
    let trace = Trace::from_snapshot(&recording.snapshot(6000));
    let frames =
        |ids: &[FrameId]| Population::frames(&trace, ids.iter().filter_map(|id| trace.frame(*id)));
    let (normal, slow) = (frames(&normal), frames(&slow));

    // By name, layout looks the same work done slower.
    let by_name = Comparison::between(&normal, &slow)
        .group_by(GroupBy::Name)
        .run();
    let layout = by_name.row("layout").expect("layout ran in both");
    assert_eq!(layout.change_micros(), 90.0);
    assert_eq!(layout.volume_micros(), 0.0, "one call either way");
    assert_eq!(layout.cost_micros(), 90.0);

    // By node count, it is different work: layouts of 12000 nodes, which
    // normal frames never ran, in place of layouts of 120.
    let by_nodes = Comparison::between(&normal, &slow)
        .group_by(GroupBy::Name)
        .group_by(GroupBy::Arg("nodes".into()))
        .run();
    let rows: Vec<_> = by_nodes
        .rows()
        .iter()
        .map(|row| {
            (
                row.describe(),
                row.change_micros(),
                row.volume_micros(),
                row.cost_micros(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            (String::from("layout / nodes=12000"), 100.0, 100.0, 0.0),
            (String::from("layout / nodes=120"), -10.0, -10.0, 0.0),
            (String::from("paint / (no nodes)"), 0.0, 0.0, 0.0),
        ]
    );
    let storm = by_nodes.row("layout / nodes=12000").expect("a row");
    assert_eq!(storm.baseline().count_per_unit(), 0.0);
    assert_eq!(storm.candidate().count_per_unit(), 1.0);
}

#[test]
fn self_time_puts_a_change_in_the_scope_that_did_the_work() {
    let mut recording = Recording::new(7);
    let mut frames = Vec::new();
    for (index, layout_length) in [10, 10, 40, 40].into_iter().enumerate() {
        let start = index as u64 * 1000;
        frames.push(FrameId::new(recording.marker(start, MarkerPhase::Begin)));
        let update = recording.begin(start, MAIN, &UPDATE, 0);
        recording.scope(MAIN, &LAYOUT, 0, start + 5, start + 5 + layout_length);
        recording.end(start + 55 + layout_length, MAIN, update);
        recording.marker(start + 500, MarkerPhase::End);
    }
    let trace = Trace::from_snapshot(&recording.snapshot(4000));
    let fast = Population::frames(&trace, frames[..2].iter().filter_map(|id| trace.frame(*id)));
    let slow = Population::frames(&trace, frames[2..].iter().filter_map(|id| trace.frame(*id)));
    let change = |metric| {
        let comparison = Comparison::between(&fast, &slow)
            .measuring(metric)
            .group_by(GroupBy::Name)
            .run();
        assert_eq!(comparison.metric(), metric);
        let get = |name| comparison.row(name).map(|row| row.change_micros());
        (get("update"), get("layout"))
    };
    // Layout grew by 30 µs. Update, around it, grew as much in total, but
    // did no more work of its own.
    assert_eq!(change(Metric::SelfTime), (Some(0.0), Some(30.0)));
    assert_eq!(change(Metric::Total), (Some(30.0), Some(30.0)));
}

#[test]
fn comparing_runs_matches_work_by_name_and_says_so() {
    let mut before = Recording::new(1);
    let mut after = Recording::new(2);
    frames_of_two_kinds(&mut before, 2, 0);
    frames_of_two_kinds(&mut after, 0, 2);
    let before = Trace::from_snapshot(&before.snapshot(2000));
    let after = Trace::from_snapshot(&after.snapshot(2000));

    let per_frame = Comparison::between(
        &Population::frames(&before, before.frames()),
        &Population::frames(&after, after.frames()),
    )
    .selecting(Selection::StartsWithin)
    .group_by(GroupBy::Name)
    .run();
    assert_eq!(per_frame.caveats(), [Caveat::DifferentInstances]);
    assert_eq!(
        per_frame.row("layout").map(|row| row.volume_micros()),
        Some(20.0)
    );

    let whole = Comparison::between(&Population::whole(&before), &Population::whole(&after))
        .selecting(Selection::StartsWithin)
        .group_by(GroupBy::Name)
        .run();
    assert!(whole.caveats().contains(&Caveat::FewUnits));
    assert_eq!(
        whole
            .row("layout")
            .and_then(|row| row.standard_error_micros()),
        None,
        "one run each says nothing about noise"
    );
}

#[test]
fn a_timeline_merges_spans_too_short_to_see() {
    let mut recording = Recording::new(7);
    for start in [0, 2, 4] {
        recording.scope(MAIN, &LAYOUT, 0, start, start + 1);
    }
    let long = recording.scope(MAIN, &PAINT, 0, 10, 30);
    recording.scope(MAIN, &LAYOUT, 0, 31, 32);
    let trace = Trace::from_snapshot(&recording.snapshot(40));
    let track = trace.track(MAIN).expect("main has a track");

    let zones = track.zones(0, range(0, 100), us(5));
    assert_eq!(zones.len(), 3);
    assert_eq!(
        zones[0],
        Zone::Merged {
            range: range(0, 5),
            count: 3
        }
    );
    assert!(matches!(zones[1], Zone::Span(span) if span.id() == long));
    assert_eq!(
        zones[2],
        Zone::Merged {
            range: range(31, 32),
            count: 1
        }
    );
    // Zoomed in, every span draws on its own.
    assert_eq!(track.zones(0, range(0, 6), us(1)).len(), 3);
}

#[test]
fn messages_resources_and_flows_lead_to_the_work_around_them() {
    let mut recording = Recording::new(7);
    let submit = SpanId::new(recording.next());
    let token = recording.begin(0, MAIN, &UPDATE, 0);
    recording.record(
        5,
        MAIN,
        Observation::Message {
            desc_id: LOG.id(),
            name: LOG.name(),
            context_id: ContextId::new(0),
            text: String::from("queued"),
            args: Args::none(),
        },
    );
    let flow = FlowId::new(42);
    recording.record(
        6,
        MAIN,
        Observation::Flow {
            flow_id: flow,
            phase: FlowPhase::Begin,
            context_id: ContextId::new(0),
        },
    );
    recording.record(
        7,
        MAIN,
        Observation::ResourceCreate {
            kind_id: TEXTURE.id(),
            kind_name: TEXTURE.name(),
            resource_id: ResourceId::new(1),
            context_id: ContextId::new(0),
            size: 100,
        },
    );
    recording.end(10, MAIN, token);
    let run = SpanId::new(recording.next());
    let token = recording.begin(20, WORKER, &DECODE, 0);
    recording.record(
        25,
        WORKER,
        Observation::Flow {
            flow_id: flow,
            phase: FlowPhase::End,
            context_id: ContextId::new(0),
        },
    );
    recording.record(
        26,
        WORKER,
        Observation::ResourceUpdate {
            kind_id: TEXTURE.id(),
            kind_name: TEXTURE.name(),
            resource_id: ResourceId::new(1),
            context_id: ContextId::new(0),
            size: 300,
        },
    );
    recording.end(30, WORKER, token);
    recording.record(
        40,
        MAIN,
        Observation::ResourceDrop {
            kind_id: TEXTURE.id(),
            kind_name: TEXTURE.name(),
            resource_id: ResourceId::new(1),
            context_id: ContextId::new(0),
        },
    );
    let trace = Trace::from_snapshot(&recording.snapshot(50));

    let message = &trace.messages_in(range(0, 10))[0];
    assert_eq!(message.text(), "queued");
    assert_eq!(message.span(), Some(submit));

    let flows: Vec<_> = trace.flows_through(submit).collect();
    assert_eq!(flows.len(), 1);
    assert!(flows[0].is_complete());
    let spans: Vec<_> = flows[0].steps().iter().map(|step| step.span()).collect();
    assert_eq!(
        spans,
        [Some(submit), Some(run)],
        "from the submit to the decode"
    );

    let texture = &trace.resources()[0];
    assert_eq!(texture.kind_name(), "texture");
    assert_eq!(texture.size_at(us(10)), Some(100));
    assert_eq!(texture.size_at(us(30)), Some(300));
    assert_eq!(texture.size_at(us(45)), None, "dropped at 40");
    assert_eq!(trace.resources_alive_at(us(30)).count(), 1);
    assert_eq!(trace.resources_alive_at(us(45)).count(), 0);
}

#[test]
fn counter_series_plot_samples_over_time() {
    let mut recording = Recording::new(7);
    for frame in 0..3 {
        recording.marker(frame * 10, MarkerPhase::Begin);
        recording.marker(frame * 10 + 5, MarkerPhase::End);
        recording.sample(frame * 10 + 5, (frame + 1) * 100);
    }
    let trace = Trace::from_snapshot(&recording.snapshot(40));
    let series = trace.counter_series().next().expect("uploads were sampled");
    assert_eq!(series.name(), "uploads");
    let changes: Vec<_> = series
        .samples()
        .iter()
        .map(|sample| sample.change())
        .collect();
    assert_eq!(changes, [Some(100), Some(100), Some(100)]);
    assert_eq!(series.samples_in(range(10, 20)).len(), 1);
    assert_eq!(
        trace.recorded(footfall_analysis::Signal::CounterSamples),
        Recorded::Yes
    );
}

#[test]
fn forgetting_bounds_a_live_trace_and_shows_in_its_answers() {
    let mut recording = Recording::new(7);
    let old = recording.scope(MAIN, &LAYOUT, 0, 0, 10);
    let recent = recording.scope(MAIN, &LAYOUT, 0, 60, 70);
    let mut trace = Trace::from_snapshot(&recording.snapshot(100));
    trace.forget_before(us(50));
    assert!(trace.span(old).is_none());
    assert!(trace.span(recent).is_some());
    assert_eq!(trace.observed(), range(50, 100));
    let basis = trace.spans().within(range(0, 100)).basis();
    assert_eq!(basis.spans(), 1);
    assert_eq!(basis.unobserved(), us(50), "the forgotten half");
}

#[test]
fn instances_show_typical_and_worst_examples() {
    let mut recording = Recording::new(7);
    let mut ids = Vec::new();
    let mut start = 0;
    for length in [1, 2, 3, 4, 100] {
        ids.push(recording.scope(MAIN, &LAYOUT, 0, start, start + length));
        start += length + 1;
    }
    let trace = Trace::from_snapshot(&recording.snapshot(1000));
    let instances = trace.spans().named("layout").instances(1);
    assert_eq!(instances.worst()[0].span().id(), ids[4]);
    assert_eq!(instances.typical()[0].span().id(), ids[2], "the median");
    assert_eq!(instances.stats().count(), 5);
}

#[test]
fn duration_stats_use_nearest_rank_percentiles() {
    let stats = DurationStats::of((1..=100).map(us));
    assert_eq!(stats.count(), 100);
    assert_eq!(stats.median(), us(50));
    assert_eq!(stats.p90(), us(90));
    assert_eq!(stats.p99(), us(99));
    assert_eq!(stats.mean(), Duration::from_nanos(50_500));
    assert_eq!(DurationStats::of([]), DurationStats::default());
}

static GRAPHICS: TimelineDesc = TimelineDesc::gpu("graphics");
static GPU_CLOCK: ClockDesc = ClockDesc::new("gpu");
static LOST_CLOCK: ClockDesc = ClockDesc::new("lost");
static GPU_FRAME: ScopeDesc = ScopeDesc::new("gpu_frame");
static SHADOWS: ScopeDesc = ScopeDesc::new("shadows");
static OPAQUE: ScopeDesc = ScopeDesc::new("opaque");

fn gpu(scope: &'static ScopeDesc, begin: u64, end: u64) -> TimelineSpan {
    TimelineSpan::new(
        &GRAPHICS,
        scope,
        footfall_core::ContextHandle::ROOT,
        &GPU_CLOCK,
        TickPeriod::NANOSECOND,
        begin,
        end,
    )
    .in_frame(1234)
}

#[test]
fn gpu_work_is_placed_on_its_own_timeline_through_its_clock() {
    let mut recording = Recording::new(7);
    // The GPU clock read 5 ms of ticks 1,000 µs into the runtime.
    recording.clock_sync(1_000, &GPU_CLOCK, 5_000_000);
    // The CPU submits frame 1234's GPU work under flow 1234.
    let submit = SpanId::new(recording.next());
    let token = recording.begin(1_040, MAIN, &UPDATE, 0);
    recording.record(
        1_050,
        MAIN,
        Observation::Flow {
            flow_id: FlowId::new(1234),
            phase: FlowPhase::Begin,
            context_id: ContextId::new(0),
        },
    );
    recording.end(1_060, MAIN, token);
    // Read back later: the whole frame, then the passes inside it.
    let frame = recording.timeline_span(
        2_000,
        &gpu(&GPU_FRAME, 5_100_000, 5_600_000).after_flow(FlowId::new(1234)),
    );
    let shadows = recording.timeline_span(2_000, &gpu(&SHADOWS, 5_150_000, 5_250_000));
    recording.timeline_span(2_000, &gpu(&OPAQUE, 5_300_000, 5_550_000));
    let trace = Trace::from_snapshot(&recording.snapshot(3_000));

    let graphics = trace.timeline_named("graphics").expect("the GPU timeline");
    assert!(matches!(
        graphics.kind(),
        footfall_analysis::TrackKind::Timeline {
            kind: footfall_core::TimelineKind::Gpu,
            ..
        }
    ));
    let gpu_frame = trace.span(frame).expect("placed");
    assert_eq!(gpu_frame.range(), range(1_100, 1_600));
    assert_eq!(gpu_frame.depth(), 0);
    assert_eq!(
        gpu_frame.self_time(),
        us(150),
        "500 less the passes' 100 and 250"
    );
    assert_eq!(gpu_frame.frame_number(), Some(1234));
    assert_eq!(gpu_frame.timeline(), graphics.timeline_id());
    let placement = gpu_frame.placement().expect("timed by another clock");
    assert!(!placement.between_syncs(), "one sync, so extended from it");
    assert_eq!(placement.distance(), us(600));
    let pass = trace.span(shadows).expect("placed");
    assert_eq!((pass.depth(), pass.parent()), (1, Some(frame)));
    assert_eq!(trace.children(gpu_frame).len(), 2);

    // The flow leads from the CPU submit to the GPU work.
    let flows: Vec<_> = trace.flows_through(frame).collect();
    let spans: Vec<_> = flows[0].steps().iter().map(|step| step.span()).collect();
    assert_eq!(spans, [Some(submit), Some(frame)]);

    // Queries cover timelines, or leave them out.
    let gpu_only = trace.spans().on_timelines().group_by(GroupBy::Name).table();
    assert_eq!(gpu_only.rows().len(), 3);
    assert_eq!(gpu_only.basis().converted(), 3);
    assert_eq!(gpu_only.basis().farthest_conversion(), us(600));
    assert!(
        trace
            .spans()
            .on_threads()
            .named("shadows")
            .table()
            .rows()
            .is_empty()
    );
    let by_track = trace.spans().group_by(GroupBy::Thread).table();
    assert!(by_track.row("graphics").is_some());
    assert!(by_track.row("main").is_some());
}

#[test]
fn gpu_work_without_a_clock_reading_waits_to_be_placed() {
    let mut recording = Recording::new(7);
    let lost = recording.timeline_span(
        10,
        &TimelineSpan::new(
            &GRAPHICS,
            &SHADOWS,
            footfall_core::ContextHandle::ROOT,
            &LOST_CLOCK,
            TickPeriod::NANOSECOND,
            9_000,
            9_500,
        ),
    );
    let first_next = recording.next();
    recording.clock_sync(20, &LOST_CLOCK, 10_000);

    let mut trace = Trace::new();
    trace
        .push(&recording.window(0, first_next, &[], 15))
        .expect("one instance");
    assert_eq!(trace.unplaced_spans(), 1);
    let basis = trace.spans().table().basis().clone();
    assert_eq!(basis.unplaced(), 1);
    assert!(!basis.is_complete());
    assert_eq!(trace.spans().on_threads().table().basis().unplaced(), 0);

    // The clock's first reading arrives with the next window.
    let pushed = trace
        .push(&recording.window(first_next, recording.next(), &[], 30))
        .expect("one instance");
    assert_eq!(pushed.spans(), [lost]);
    assert_eq!(trace.unplaced_spans(), 0);
    assert_eq!(
        trace.span(lost).map(|span| span.range()),
        Some(TimeRange::new(us(19), Duration::from_nanos(19_500)))
    );
}

#[test]
fn a_frames_gpu_work_counts_toward_that_frame_wherever_it_ran() {
    let mut recording = Recording::new(7);
    recording.clock_sync(0, &GPU_CLOCK, 0);
    // Frame 1234's CPU work, then frame 1235's, while 1234's GPU work runs.
    let first = FrameId::new(recording.numbered_marker(0, MarkerPhase::Begin, Some(1234)));
    recording.numbered_marker(100, MarkerPhase::End, Some(1234));
    let second = FrameId::new(recording.numbered_marker(100, MarkerPhase::Begin, Some(1235)));
    recording.numbered_marker(200, MarkerPhase::End, Some(1235));
    recording.timeline_span(250, &gpu(&OPAQUE, 120_000, 180_000));
    let trace = Trace::from_snapshot(&recording.snapshot(300));

    let gpu_time = |frame| {
        trace
            .spans()
            .in_frames(&[frame])
            .selecting(Selection::Inside)
            .on_timelines()
            .table()
            .total()
            .total()
    };
    assert_eq!(gpu_time(first), us(60), "frame 1234's opaque pass, whole");
    assert_eq!(gpu_time(second), us(0), "though it ran during frame 1235");
}

static HOST: ClockDesc = ClockDesc::new("host");

impl Recording {
    /// Records a frame point at `at` µs whose time is `time` µs of the host
    /// clock, or no time.
    fn frame_point(
        &mut self,
        at: u64,
        number: u64,
        kind: footfall_core::FramePointKind,
        clock: &'static ClockDesc,
        time: Option<u64>,
    ) {
        self.record(
            at,
            MAIN,
            Observation::FramePoint {
                number,
                kind,
                time: time.map(|micros| {
                    ClockTicks::from(ClockReading::new(
                        clock,
                        micros * 1000,
                        TickPeriod::NANOSECOND,
                    ))
                }),
            },
        );
    }

    fn numbered_frame(&mut self, number: u64, begin: u64, end: u64) -> FrameId {
        let id = FrameId::new(self.numbered_marker(begin, MarkerPhase::Begin, Some(number)));
        self.numbered_marker(end, MarkerPhase::End, Some(number));
        id
    }
}

#[test]
fn pacing_says_which_frames_were_shown_on_time_and_why_not() {
    use footfall_analysis::{LateCause, Outcome};
    use footfall_core::DropReason;
    use footfall_core::FramePointKind::{Deadline, Dropped, Presented, Target};
    let mut recording = Recording::new(7);
    // The host clock reads in step with the runtime's: 0 at 0.
    recording.clock_sync(0, &HOST, 0);
    let first = recording.numbered_frame(1, 0, 5_000);
    recording.frame_point(5_000, 1, Deadline, &HOST, Some(12_500));
    recording.frame_point(5_000, 1, Target, &HOST, Some(16_667));
    let second = recording.numbered_frame(2, 16_667, 40_000);
    recording.frame_point(40_000, 2, Target, &HOST, Some(33_333));
    let third = recording.numbered_frame(3, 40_000, 45_000);
    recording.frame_point(45_000, 1, Presented, &HOST, Some(16_667));
    recording.frame_point(45_000, 2, Presented, &HOST, Some(50_000));
    recording.frame_point(46_000, 3, Dropped(DropReason::MissedDeadline), &HOST, None);
    let fourth = recording.numbered_frame(4, 50_000, 64_000);
    recording.frame_point(64_000, 4, Deadline, &HOST, Some(62_500));
    recording.numbered_frame(5, 66_667, 70_000);
    recording.frame_point(70_000, 5, Target, &HOST, Some(83_333));
    // A point whose clock was never synced.
    recording.frame_point(70_000, 5, Presented, &LOST_CLOCK, Some(83_333));
    let trace = Trace::from_snapshot(&recording.snapshot(90_000));

    let pacing = trace.pacing(us(500));
    let outcomes: Vec<_> = pacing
        .frames()
        .iter()
        .map(|frame| frame.outcome())
        .collect();
    assert_eq!(
        outcomes,
        [
            Outcome::OnTime,
            Outcome::Late {
                by: us(16_667),
                cause: LateCause::Unknown,
            },
            Outcome::Dropped {
                reason: DropReason::MissedDeadline,
            },
            Outcome::SubmittedLate { by: us(1_500) },
            Outcome::Unknown,
        ]
    );
    let counts = pacing.counts();
    assert_eq!((counts.on_time, counts.late, counts.total()), (1, 1, 5));
    assert_eq!(pacing.frame_ids(Outcome::missed), [second, third, fourth]);
    assert_eq!(
        pacing.frame(1).and_then(|frame| frame.latency()),
        Some(us(16_667))
    );
    assert_eq!(
        pacing.frame(1).map(|frame| frame.frame()),
        Some(Some(first))
    );
    assert_eq!(
        pacing.intervals().median(),
        us(33_333),
        "frame 1 to frame 2"
    );
    assert_eq!(pacing.unplaced_points(), 1);
}

#[test]
fn late_frames_say_what_made_them_late() {
    use footfall_analysis::{LateCause, Outcome};
    use footfall_core::DropReason;
    use footfall_core::FramePointKind::{
        Deadline, Dropped, Expected, Predicted, Presented, Target,
    };
    let mut recording = Recording::new(7);
    recording.clock_sync(0, &HOST, 0);
    recording.clock_sync(0, &GPU_CLOCK, 0);
    // Each frame: its work, deadline, target, and when it was shown, in µs.
    let frame = |recording: &mut Recording, number, work: (u64, u64), times: [Option<u64>; 3]| {
        recording.numbered_frame(number, work.0, work.1);
        let [deadline, target, presented] = times;
        for (kind, time) in [
            (Deadline, deadline),
            (Target, target),
            (Presented, presented),
        ] {
            if time.is_some() {
                recording.frame_point(work.1, number, kind, &HOST, time);
            }
        }
    };
    let gpu_for =
        |number, begin: u64, end: u64| gpu(&OPAQUE, begin * 1000, end * 1000).in_frame(number);
    // 1: on time.
    frame(
        &mut recording,
        1,
        (0, 5_000),
        [Some(12_500), Some(16_667), Some(16_667)],
    );
    recording.frame_point(0, 1, Predicted, &HOST, Some(16_667));
    recording.frame_point(5_000, 1, Expected, &HOST, Some(16_667));
    // 2: its work ran past its deadline.
    frame(
        &mut recording,
        2,
        (16_667, 30_000),
        [Some(29_167), Some(33_333), Some(50_000)],
    );
    recording.frame_point(16_667, 2, Predicted, &HOST, Some(33_333));
    // 3: superseded by newer work after it was built.
    recording.numbered_frame(3, 33_333, 38_000);
    recording.frame_point(38_000, 3, Dropped(DropReason::Superseded), &HOST, None);
    // 4: submitted in time, but its GPU work ran past its target.
    frame(
        &mut recording,
        4,
        (50_000, 55_000),
        [Some(62_500), Some(66_667), Some(83_333)],
    );
    recording.timeline_span(72_000, &gpu_for(4, 56_000, 70_000));
    recording.frame_point(50_000, 4, Predicted, &HOST, Some(83_333));
    // 5: in time, but frame 4 still held its slot.
    frame(
        &mut recording,
        5,
        (66_667, 70_000),
        [Some(79_167), Some(83_333), Some(100_000)],
    );
    // 6: in time, rendered in time, nothing ahead of it: the display.
    frame(
        &mut recording,
        6,
        (100_000, 105_000),
        [Some(112_500), Some(116_667), Some(133_333)],
    );
    recording.timeline_span(112_000, &gpu_for(6, 106_000, 110_000));
    // 7: in time with nothing ahead, but no GPU work to tell.
    frame(
        &mut recording,
        7,
        (133_333, 137_000),
        [Some(145_833), Some(150_000), Some(166_667)],
    );
    // 8: replaced on its way to the display.
    recording.frame_point(170_000, 8, Dropped(DropReason::Replaced), &HOST, None);
    let trace = Trace::from_snapshot(&recording.snapshot(180_000));

    let pacing = trace.pacing(us(500));
    let outcomes: Vec<_> = pacing
        .frames()
        .iter()
        .map(|frame| frame.outcome())
        .collect();
    let late = |cause| Outcome::Late {
        by: us(16_667),
        cause,
    };
    assert_eq!(
        outcomes,
        [
            Outcome::OnTime,
            late(LateCause::MissedDeadline),
            Outcome::Dropped {
                reason: DropReason::Superseded
            },
            Outcome::Late {
                by: us(16_666),
                cause: LateCause::Gpu,
            },
            late(LateCause::Backlog),
            Outcome::Late {
                by: us(16_666),
                cause: LateCause::Display,
            },
            late(LateCause::AfterSubmit),
            Outcome::Dropped {
                reason: DropReason::Replaced
            },
        ]
    );
    let counts = pacing.counts();
    assert_eq!((counts.late, counts.skipped, counts.dropped), (5, 2, 0));
    assert_eq!(counts.missed(), 5);
    assert_eq!(counts.late_causes.gpu, 1);
    assert_eq!(counts.total(), 8);
    assert!(
        !Outcome::Dropped {
            reason: DropReason::Superseded
        }
        .missed()
    );

    let predicted = pacing.prediction_error(Predicted);
    assert_eq!(
        (
            predicted.off.count(),
            predicted.within(),
            predicted.late.count()
        ),
        (3, 2, 1)
    );
    assert_eq!(
        predicted.late.max(),
        us(16_667),
        "frame 2 was predicted for 33.3 ms"
    );
    let expected = pacing.prediction_error(Expected);
    assert_eq!((expected.off.count(), expected.off.max()), (1, us(0)));

    let wasted = pacing.wasted();
    assert_eq!(
        (wasted.frames, wasted.cpu, wasted.gpu),
        (2, us(4_667), us(0))
    );
}
