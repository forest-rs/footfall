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
    FrameNumber, FrameStream, InstrumentationLevel, LabelKey, MarkerKind, MarkerPhase, ResourceId,
    ResourceKindDesc, ScopeDesc, ScopeToken, TickPeriod, TimelineDesc, TimelineSpan,
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
                number: number.map(FrameNumber::from),
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
    assert_eq!(frame.number(), Some(1234.into()));
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
    assert_eq!(gpu_frame.frame_number(), Some(1234.into()));
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

static COMPUTE: TimelineDesc = TimelineDesc::gpu("compute");

#[test]
fn gpu_work_nests_whatever_order_it_is_read_back_in() {
    let mut recording = Recording::new(7);
    recording.clock_sync(0, &GPU_CLOCK, 0);
    recording.numbered_marker(0, MarkerPhase::Begin, Some(1234));
    recording.numbered_marker(100, MarkerPhase::End, Some(1234));
    // The passes are read back as they finish, before the frame around
    // them, which comes in the next window.
    let shadows = recording.timeline_span(700, &gpu(&SHADOWS, 150_000, 250_000));
    let opaque = recording.timeline_span(700, &gpu(&OPAQUE, 300_000, 550_000));
    let first_next = recording.next();
    let frame = recording.timeline_span(800, &gpu(&GPU_FRAME, 100_000, 600_000));
    // Compute work for the same frame, alongside the graphics work.
    recording.timeline_span(
        800,
        &TimelineSpan::new(
            &COMPUTE,
            &OPAQUE,
            footfall_core::ContextHandle::ROOT,
            &GPU_CLOCK,
            TickPeriod::NANOSECOND,
            200_000,
            300_000,
        )
        .in_frame(1234),
    );

    let mut trace = Trace::new();
    trace
        .push(&recording.window(0, first_next, &[], 750))
        .expect("one instance");
    assert_eq!(
        trace.span(shadows).map(|span| span.depth()),
        Some(0),
        "nothing around it yet"
    );
    trace
        .push(&recording.window(first_next, recording.next(), &[], 900))
        .expect("one instance");

    let gpu_frame = trace.span(frame).expect("placed");
    assert_eq!((gpu_frame.depth(), gpu_frame.parent()), (0, None));
    assert_eq!(
        gpu_frame.self_time(),
        us(150),
        "500 less the passes' 100 and 250"
    );
    for pass in [shadows, opaque] {
        let pass = trace.span(pass).expect("placed");
        assert_eq!((pass.depth(), pass.parent()), (1, Some(frame)));
    }
    let graphics = trace.timeline_named("graphics").expect("the GPU timeline");
    assert_eq!(graphics.depth(), 2);
    assert_eq!(trace.children(gpu_frame).len(), 2);

    // The same work in one snapshot nests the same way.
    let whole = Trace::from_snapshot(&recording.snapshot(900));
    for id in [frame, shadows, opaque] {
        let nesting = |trace: &Trace| {
            trace
                .span(id)
                .map(|span| (span.depth(), span.parent(), span.self_time()))
        };
        assert_eq!(nesting(&trace), nesting(&whole));
    }

    // The GPU was busy with the frame for 500 µs on the graphics queue,
    // counting the passes inside once, and 100 µs on the compute queue.
    let pacing = trace.pacing(us(1_000));
    let frame = &pacing.frames()[0];
    assert_eq!(frame.gpu_busy(), Some(us(600)));
    assert_eq!(frame.gpu(), Some(range(100, 600)));
}

#[test]
fn timeline_spans_place_the_same_however_the_windows_split() {
    let mut recording = Recording::new(7);
    recording.clock_sync(0, &GPU_CLOCK, 0);
    recording.numbered_marker(0, MarkerPhase::Begin, Some(1234));
    recording.numbered_marker(100, MarkerPhase::End, Some(1234));
    // Read back before the GPU clock's next reading, so placed at its
    // nominal rate until that reading arrives.
    let frame = recording.timeline_span(
        500,
        &gpu(&GPU_FRAME, 100_000, 600_000).after_flow(FlowId::new(1234)),
    );
    let shadows = recording.timeline_span(500, &gpu(&SHADOWS, 150_000, 250_000));
    let before_reading = recording.next();
    // The GPU clock ran 10% fast: its 1,100,000 ticks were 1,000 µs.
    recording.clock_sync(1_000, &GPU_CLOCK, 1_100_000);
    let opaque = recording.timeline_span(1_200, &gpu(&OPAQUE, 300_000, 550_000));
    let next_frame = recording.timeline_span(1_300, &gpu(&OPAQUE, 1_150_000, 1_200_000));
    recording.clock_sync(2_000, &GPU_CLOCK, 2_200_000);
    let next = recording.next();
    let whole = Trace::from_snapshot(&recording.snapshot(2_500));
    assert_eq!(
        whole.span(frame).map(|span| span.range()),
        Some(TimeRange::new(
            Duration::from_nanos(90_909),
            Duration::from_nanos(545_454)
        )),
        "placed between the readings, following the drift"
    );

    let shape = |trace: &Trace| -> Vec<_> {
        let spans = [frame, shadows, opaque, next_frame].map(|id| {
            trace.span(id).map(|span| {
                (
                    span.range(),
                    span.depth(),
                    span.parent(),
                    span.self_time(),
                    span.placement().map(|placement| placement.between_syncs()),
                )
            })
        });
        let flow: Vec<_> = trace
            .flows_through(frame)
            .flat_map(|flow| flow.steps().iter().map(|step| (step.at(), step.span())))
            .collect();
        let pacing = trace.pacing(us(1_000));
        let gpu = pacing
            .frame(1234)
            .map(|frame| (frame.gpu(), frame.gpu_busy()));
        vec![format!("{spans:?} {flow:?} {gpu:?}")]
    };
    for split in 1..next {
        let mut trace = Trace::new();
        trace
            .push(&recording.window(0, split, &[], 2_500))
            .expect("one instance");
        let second = trace
            .push(&recording.window(split, next, &[], 2_500))
            .expect("one instance");
        assert_eq!(shape(&trace), shape(&whole), "split before event {split}");
        if split == before_reading {
            assert_eq!(second.moved_spans(), [frame, shadows]);
        }
    }
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
        number: impl Into<FrameNumber>,
        kind: footfall_core::FramePointKind,
        clock: &'static ClockDesc,
        time: Option<u64>,
    ) {
        self.frame_point_with(
            at,
            number,
            kind,
            clock,
            time,
            footfall_core::TimingBasis::Unknown,
            Args::none(),
        );
    }

    /// Records a frame point as [`frame_point`](Self::frame_point) does,
    /// with a timing basis and args.
    #[expect(clippy::too_many_arguments, reason = "a test helper")]
    fn frame_point_with(
        &mut self,
        at: u64,
        number: impl Into<FrameNumber>,
        kind: footfall_core::FramePointKind,
        clock: &'static ClockDesc,
        time: Option<u64>,
        basis: footfall_core::TimingBasis,
        args: Args,
    ) {
        self.record(
            at,
            MAIN,
            Observation::FramePoint {
                number: number.into(),
                kind,
                time: time.map(|micros| {
                    ClockTicks::from(ClockReading::new(
                        clock,
                        micros * 1000,
                        TickPeriod::NANOSECOND,
                    ))
                }),
                basis,
                args,
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
fn presentation_intervals_run_across_drops_and_break_where_unknown() {
    use footfall_analysis::Outcome;
    use footfall_core::DropReason;
    use footfall_core::FramePointKind::{Dropped, Presented, Target};
    let mut recording = Recording::new(7);
    recording.clock_sync(0, &HOST, 0);
    let shown = |recording: &mut Recording, number, at| {
        recording.frame_point(at, number, Presented, &HOST, Some(at));
    };
    shown(&mut recording, 1, 16_667);
    // Frame 2 was superseded, so frame 1 stayed up for two refreshes.
    recording.frame_point(20_000, 2, Dropped(DropReason::Superseded), &HOST, None);
    shown(&mut recording, 3, 50_000);
    shown(&mut recording, 4, 66_667);
    // Nothing was reported for frame 5: what was on screen is not known.
    recording.numbered_frame(5, 70_000, 75_000);
    shown(&mut recording, 6, 100_000);
    shown(&mut recording, 7, 116_667);
    // Frame 8 is missing from the trace altogether.
    shown(&mut recording, 9, 150_000);
    shown(&mut recording, 10, 166_667);
    // Frame 11 was meant for 200 ms and shown a refresh early.
    recording.frame_point(170_000, 11, Target, &HOST, Some(200_000));
    shown(&mut recording, 11, 183_333);
    // Frames 12 and 13 were still in flight when the trace ended.
    recording.numbered_frame(12, 185_000, 190_000);
    recording.numbered_frame(13, 200_000, 205_000);
    let trace = Trace::from_snapshot(&recording.snapshot(250_000));

    let pacing = trace.pacing(us(1_000));
    let intervals = pacing.intervals();
    assert_eq!(
        (intervals.count(), intervals.median(), intervals.max()),
        (5, us(16_667), us(33_333)),
        "1 to 3 across the drop, 3 to 4, 6 to 7, 9 to 10, and 10 to 11"
    );
    assert_eq!(
        pacing.interval_breaks(),
        2,
        "at frame 5 and at frame 8, not at the frames still in flight"
    );
    assert_eq!(pacing.stutters(), 1);
    assert_eq!(
        pacing.frame(11).map(|frame| frame.outcome()),
        Some(Outcome::Early { by: us(16_667) })
    );
    assert!(!Outcome::Early { by: us(1) }.missed());
    assert_eq!(pacing.counts().early, 1);
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

    // Each frame against its own deadline, whatever became of it.
    let deadlines = pacing.deadlines();
    assert_eq!(
        (deadlines.met, deadlines.missed, deadlines.judged()),
        (5, 1, 6),
        "frames 3 and 8 have no deadline"
    );
    let judged = |number: u64| {
        let frame = pacing.frame(number).expect("a frame");
        (frame.met_deadline(), frame.submitted_late())
    };
    assert_eq!(judged(2), (Some(false), Some(us(833))));
    assert_eq!(judged(4), (Some(true), None), "late, but not its work");
    assert_eq!(judged(3), (None, None));

    // The evidence behind each cause, and how strongly the trace supports it.
    use footfall_analysis::Support;
    let why = |number: u64| {
        *pacing
            .frame(number)
            .and_then(|frame| frame.why())
            .expect("a late frame")
    };
    assert!(pacing.frame(1).and_then(|frame| frame.why()).is_none());
    let deadline = why(2);
    assert_eq!(deadline.support, Support::Observed);
    assert_eq!(deadline.submitted_late, Some(us(833)));
    assert!(!deadline.submit_reported, "judged by its end marker");
    assert_eq!(
        deadline.submitted,
        footfall_core::TimingBasis::Observed,
        "the runtime recorded the end marker itself"
    );
    let gpu = why(4);
    assert_eq!(
        (gpu.support, gpu.gpu_late, gpu.submitted_late),
        (Support::Observed, Some(us(3_333)), None)
    );
    let backlog = why(5);
    assert_eq!(
        (backlog.support, backlog.behind_previous),
        (Support::Inferred, true),
        "the queue itself was not observed"
    );
    let display = why(6);
    assert_eq!(
        (display.support, display.gpu_recorded, display.gpu_late),
        (Support::Unresolved, true, None),
        "the compositor was not observed"
    );
    assert_eq!(
        (why(7).support, why(7).gpu_recorded),
        (Support::Unresolved, false)
    );
    let support = counts.late_support;
    assert_eq!(
        (support.observed, support.inferred, support.unresolved),
        (2, 1, 2)
    );
    assert_eq!(
        support.observed_unstated, 2,
        "both rest on presentation times recorded without a basis"
    );
    assert!(why(2).unstated_basis() && why(4).unstated_basis());
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
        (
            wasted.frames,
            wasted.cpu_frames,
            wasted.cpu,
            wasted.gpu_frames,
            wasted.gpu
        ),
        (2, 1, us(4_667), 0, us(0)),
        "only frame 3 has markers, and neither has GPU work"
    );
}

#[test]
fn forgetting_covers_frame_points_waiting_work_and_old_clock_readings() {
    use footfall_core::FramePointKind::{Presented, Target};
    let mut recording = Recording::new(7);
    recording.clock_sync(0, &HOST, 0);
    recording.clock_sync(10, &HOST, 10_000);
    // Frame 1 is over by 50 µs: its work and its feedback.
    recording.numbered_frame(1, 0, 5);
    recording.frame_point(5, 1, Target, &HOST, Some(16));
    recording.frame_point(20, 1, Presented, &HOST, Some(16));
    // Frame 2's work is over by then, but its feedback comes after.
    recording.numbered_frame(2, 16, 30);
    recording.frame_point(30, 2, Target, &HOST, Some(33));
    recording.frame_point(60, 2, Presented, &HOST, Some(34));
    // Work waiting for a clock that never syncs.
    recording.timeline_span(
        10,
        &TimelineSpan::new(
            &GRAPHICS,
            &SHADOWS,
            footfall_core::ContextHandle::ROOT,
            &LOST_CLOCK,
            TickPeriod::NANOSECOND,
            0,
            1_000,
        ),
    );
    recording.clock_sync(70, &HOST, 70_000);
    let mut trace = Trace::from_snapshot(&recording.snapshot(100));
    assert_eq!(trace.unplaced_spans(), 1);

    trace.forget_before(us(50));
    let pacing = trace.pacing(us(1));
    let numbers: Vec<_> = pacing.frames().iter().map(|frame| frame.number()).collect();
    assert_eq!(
        numbers,
        [FrameNumber::from(2)],
        "frame 1 is forgotten; frame 2 keeps its target"
    );
    assert_eq!(
        pacing
            .frame(2)
            .map(|frame| (frame.target(), frame.presented())),
        Some((Some(us(33)), Some(us(34))))
    );
    assert_eq!(trace.unplaced_spans(), 0);
    let host = trace.clocks().find("host").expect("still known");
    assert_eq!(
        trace.clocks().clock(host).map(|clock| clock.syncs()),
        Some(2),
        "the sync at 10 µs, the last before, and the one at 70 µs"
    );
}

#[test]
fn frame_streams_keep_each_windows_frames_apart() {
    use footfall_core::FramePointKind::{Expected, Presented, Target};
    let (main, tools) = (FrameStream::new(1), FrameStream::new(2));
    let mut recording = Recording::new(7);
    recording.clock_sync(0, &HOST, 0);
    // Both windows number their frames from 1: the main window at 60 Hz,
    // the tools window at 30 Hz, each shown on time. The main window shows
    // more frames, so pooled together its intervals would be the median.
    let mut ids = Vec::new();
    for (stream, period, start, frames) in [(main, 16_667, 0, 5), (tools, 33_333, 100_000, 3)] {
        for number in 1..=frames {
            let frame = stream.frame(number);
            let begin = start + (number - 1) * period;
            let mark = |recording: &mut Recording, at, phase| {
                recording.record(
                    at,
                    MAIN,
                    Observation::FrameMarker {
                        kind: MarkerKind::Frame,
                        phase,
                        number: Some(frame),
                        context_id: ContextId::new(0),
                    },
                )
            };
            ids.push(mark(&mut recording, begin, MarkerPhase::Begin));
            mark(&mut recording, begin + 1_000, MarkerPhase::End);
            let shown = begin + period;
            recording.frame_point(begin + 1_000, frame, Target, &HOST, Some(shown));
            recording.frame_point(shown, frame, Presented, &HOST, Some(shown));
        }
    }
    // The tools window also reported a time on a clock never synced.
    recording.frame_point(200_000, tools.frame(3), Expected, &LOST_CLOCK, Some(1));
    let trace = Trace::from_snapshot(&recording.snapshot(300_000));

    assert_eq!(
        trace.frame_numbered(main.frame(1)).map(|frame| frame.id()),
        Some(FrameId::new(ids[0]))
    );
    assert_eq!(
        trace.frame_numbered(tools.frame(1)).map(|frame| frame.id()),
        Some(FrameId::new(ids[5]))
    );
    assert!(
        trace.frame_numbered(1).is_none(),
        "nothing in the default stream"
    );

    let pacing = trace.pacing(us(1_000));
    assert_eq!(pacing.frames().len(), 8);
    assert_eq!(pacing.streams(), [main, tools]);
    assert_eq!(pacing.counts().on_time, 8);
    assert_eq!(
        pacing
            .frame(tools.frame(2))
            .and_then(|frame| frame.target()),
        Some(us(166_666))
    );
    assert_eq!(pacing.interval_breaks(), 0, "a new stream is not a break");
    let main_only = pacing.in_stream(main);
    assert_eq!(
        (
            main_only.intervals().count(),
            main_only.intervals().median()
        ),
        (4, us(16_667))
    );
    let tools_only = pacing.in_stream(tools);
    assert_eq!(
        (
            tools_only.intervals().count(),
            tools_only.intervals().median()
        ),
        (2, us(33_333))
    );
    assert_eq!(
        pacing.stutters(),
        0,
        "each window is steady at its own rate, though 33 ms is twice the pooled median"
    );
    assert_eq!(pacing.intervals().median(), us(16_667));
    assert_eq!(
        (
            pacing.unplaced_points(),
            main_only.unplaced_points(),
            tools_only.unplaced_points()
        ),
        (1, 0, 1),
        "narrowing to a stream keeps what its frames left out"
    );
    assert_eq!(
        pacing
            .frame(tools.frame(3))
            .map(|frame| frame.unplaced_points()),
        Some(1)
    );
    assert_eq!(main.frame(1).to_string(), "1:1");
    assert_eq!(FrameNumber::from(1).to_string(), "1");
}

#[test]
fn frame_points_say_how_their_times_were_known_and_what_the_scheduler_knew() {
    use footfall_analysis::Outcome;
    use footfall_core::FramePointKind::{Deadline, Dropped, Presented, Sampled, Submitted, Target};
    use footfall_core::{DropReason, TimingBasis};
    let mut recording = Recording::new(7);
    recording.clock_sync(0, &HOST, 0);
    // Frame 1: its CPU work ended before the deadline, but it was submitted
    // after it; the display observed when it was shown, late.
    recording.numbered_frame(1, 0, 4_000);
    recording.frame_point_with(
        4_000,
        1,
        Deadline,
        &HOST,
        Some(5_000),
        TimingBasis::Unknown,
        Args::none()
            .with("pipeline_depth", 2_u32)
            .with("safety_margin_ns", 500_000_u64),
    );
    recording.frame_point(4_000, 1, Target, &HOST, Some(16_667));
    recording.frame_point(4_000, 1, Sampled, &HOST, Some(16_667));
    recording.frame_point_with(
        6_000,
        1,
        Submitted,
        &HOST,
        Some(6_000),
        TimingBasis::Observed,
        Args::none(),
    );
    recording.frame_point_with(
        40_000,
        1,
        Presented,
        &HOST,
        Some(33_333),
        TimingBasis::Observed,
        Args::none(),
    );
    // Frame 2: shown at a time assumed from the refresh rate.
    recording.numbered_frame(2, 16_667, 20_000);
    recording.frame_point(20_000, 2, Target, &HOST, Some(50_000));
    recording.frame_point_with(
        60_000,
        2,
        Presented,
        &HOST,
        Some(50_000),
        TimingBasis::Estimated,
        Args::none(),
    );
    // Frame 3: the platform did not present it and did not say why.
    recording.numbered_frame(3, 33_333, 36_000);
    recording.frame_point(70_000, 3, Dropped(DropReason::NotPresented), &HOST, None);
    // Frame 4: its work ran past its deadline, and it was shown late, at a
    // time assumed from the refresh rate.
    recording.numbered_frame(4, 50_000, 64_000);
    recording.frame_point(64_000, 4, Deadline, &HOST, Some(62_500));
    recording.frame_point(64_000, 4, Target, &HOST, Some(66_667));
    recording.frame_point_with(
        90_000,
        4,
        Presented,
        &HOST,
        Some(83_333),
        TimingBasis::Estimated,
        Args::none(),
    );
    let trace = Trace::from_snapshot(&recording.snapshot(100_000));

    let pacing = trace.pacing(us(1_000));
    let first = pacing.frame(1).expect("frame 1");
    assert_eq!(first.submitted(), Some(us(6_000)));
    assert_eq!(first.sampled(), Some(us(16_667)));
    assert_eq!(
        first.outcome(),
        Outcome::Late {
            by: us(16_666),
            cause: footfall_analysis::LateCause::MissedDeadline,
        },
        "submitted after the deadline, though its work ended before it"
    );
    assert_eq!(first.basis(Presented), TimingBasis::Observed);
    assert_eq!(first.basis(Target), TimingBasis::Unknown);
    let plan: Vec<_> = first
        .args(Deadline)
        .iter()
        .map(|arg| arg.key().to_owned())
        .collect();
    assert_eq!(plan, ["pipeline_depth", "safety_margin_ns"]);
    assert!(first.args(Presented).is_empty());
    assert_eq!(
        pacing.frame(2).map(|frame| frame.basis(Presented)),
        Some(TimingBasis::Estimated)
    );
    assert!(
        pacing
            .frame(3)
            .is_some_and(|frame| frame.outcome().missed()),
        "not presented without a reason is a miss"
    );
    let basis = pacing.presentation_basis();
    assert_eq!((basis.observed, basis.estimated, basis.unknown), (1, 2, 0));
    assert_eq!(
        first.why().map(|why| why.support),
        Some(footfall_analysis::Support::Observed)
    );
    assert!(first.why().is_some_and(|why| why.submit_reported));
    assert_eq!(
        pacing
            .frame(4)
            .and_then(|frame| frame.why())
            .map(|why| (why.cause, why.support)),
        Some((
            footfall_analysis::LateCause::MissedDeadline,
            footfall_analysis::Support::Inferred
        )),
        "how late rests on an estimated presentation"
    );
}

#[test]
fn a_missed_deadline_shown_by_an_estimated_submission_is_inferred() {
    use footfall_analysis::{LateCause, Support};
    use footfall_core::FramePointKind::{Deadline, Presented, Submitted, Target};
    use footfall_core::TimingBasis::{Estimated, Observed};
    let mut recording = Recording::new(7);
    recording.clock_sync(0, &HOST, 0);
    // Two frames, each submitted 2 ms after its deadline and shown a
    // refresh late; the first frame's submission time was estimated.
    for (number, submitted) in [(1_u64, Estimated), (2, Observed)] {
        let start = (number - 1) * 16_667;
        recording.numbered_frame(number, start, start + 5_000);
        let at = start + 5_000;
        let point = |recording: &mut Recording, kind, time, basis| {
            recording.frame_point_with(at, number, kind, &HOST, Some(time), basis, Args::none());
        };
        point(&mut recording, Deadline, start + 8_000, Observed);
        point(&mut recording, Submitted, start + 10_000, submitted);
        point(&mut recording, Target, start + 16_667, Observed);
        point(&mut recording, Presented, start + 33_333, Observed);
    }
    let trace = Trace::from_snapshot(&recording.snapshot(60_000));
    let pacing = trace.pacing(us(500));
    let why = |number: u64| {
        *pacing
            .frame(number)
            .and_then(|frame| frame.why())
            .expect("a late frame")
    };
    let (estimated, observed) = (why(1), why(2));
    assert_eq!(
        (estimated.cause, estimated.support, estimated.submitted),
        (LateCause::MissedDeadline, Support::Inferred, Estimated),
        "the miss rests on an estimated submission"
    );
    assert_eq!(
        (observed.cause, observed.support, observed.submitted),
        (LateCause::MissedDeadline, Support::Observed, Observed)
    );
    assert_eq!(estimated.submitted_late, Some(us(2_000)));
    assert!(
        !observed.unstated_basis(),
        "every time it rests on was stated"
    );
    assert_eq!(pacing.counts().late_support.observed_unstated, 0);
}

impl Recording {
    /// Records, at `at` µs, a frame opportunity of `stream` at `time` µs of
    /// the host clock, with an interval in µs.
    fn opportunity(&mut self, at: u64, stream: FrameStream, time: u64, interval: Option<u64>) {
        self.record(
            at,
            MAIN,
            Observation::FrameOpportunity {
                stream,
                time: ClockTicks::from(ClockReading::new(
                    &HOST,
                    time * 1000,
                    TickPeriod::NANOSECOND,
                )),
                interval: interval.map(|micros| micros * 1000),
                args: Args::none(),
            },
        );
    }

    /// Records a frame scheduled at `scheduled` µs, begun at `begin`, ended
    /// at `end`, with its deadline, target, and presentation.
    #[expect(clippy::too_many_arguments, reason = "a test helper")]
    fn scheduled_frame(
        &mut self,
        number: u64,
        begins_at: footfall_core::BeginsAt,
        scheduled: u64,
        begin: u64,
        end: u64,
        deadline: u64,
        target: u64,
        presented: u64,
    ) {
        use footfall_core::FramePointKind::{Deadline, Presented, Scheduled, Target};
        self.numbered_frame(number, begin, end);
        self.frame_point(end, number, Scheduled(begins_at), &HOST, Some(scheduled));
        self.frame_point(end, number, Deadline, &HOST, Some(deadline));
        self.frame_point(end, number, Target, &HOST, Some(target));
        self.frame_point(end, number, Presented, &HOST, Some(presented));
    }
}

#[test]
fn late_frames_say_whether_the_platform_or_the_program_began_them_late() {
    use footfall_analysis::{LateCause, Outcome, Support};
    use footfall_core::BeginsAt::{Opportunity, Time};
    let mut recording = Recording::new(9);
    recording.clock_sync(0, &HOST, 0);
    // A display link every 10 ms, which skips the one at 20 ms.
    for time in [0, 10_000, 30_000, 40_000, 50_000, 60_000] {
        recording.opportunity(time, FrameStream::DEFAULT, time, Some(10_000));
    }
    // Begun at the opportunity it waited for, and shown on time.
    recording.scheduled_frame(
        1,
        Opportunity,
        5_000,
        10_000,
        12_000,
        18_000,
        20_000,
        20_000,
    );
    // Meant for the opportunity at 20 ms, which never came: begun at the
    // next, and shown a refresh late, though submitted by its deadline.
    recording.scheduled_frame(
        2,
        Opportunity,
        15_000,
        30_000,
        32_000,
        38_000,
        30_000,
        40_000,
    );
    // Its opportunity came at 40 ms, but the program began it at the next.
    recording.scheduled_frame(
        3,
        Opportunity,
        35_000,
        50_000,
        52_000,
        58_000,
        50_000,
        60_000,
    );
    // Woken by a timer 3 ms after it was scheduled to begin.
    recording.scheduled_frame(4, Time, 61_000, 64_000, 66_000, 68_000, 70_000, 80_000);
    let trace = Trace::from_snapshot(&recording.snapshot(90_000));
    let pacing = trace.pacing(us(500));

    let on_time = pacing.frame(1).expect("frame 1");
    assert_eq!(on_time.outcome(), Outcome::OnTime);
    assert!(on_time.begin_judged());
    assert_eq!(
        (on_time.began_late(), on_time.opportunity_late()),
        (None, None)
    );
    assert_eq!(on_time.scheduled(), Some(us(5_000)));
    assert_eq!(on_time.begins_at(), Some(Opportunity));

    let skipped = pacing.frame(2).expect("frame 2");
    assert_eq!(
        skipped.outcome(),
        Outcome::Late {
            by: us(10_000),
            cause: LateCause::SkippedOpportunity,
        }
    );
    let why = skipped.why().expect("a late frame says why");
    assert_eq!(why.support, Support::Observed);
    assert_eq!(why.opportunity_late, Some(us(10_000)));
    assert_eq!(why.began_late, None, "it began at the opportunity it got");
    assert_eq!(why.submitted_late, None);
    assert_eq!(
        skipped.outcome().to_string(),
        "late by 10.00 ms: the platform skipped its opportunity"
    );
    assert_eq!(
        why.to_string(),
        "the platform skipped its opportunity, observed; its opportunity came 10.00 ms late"
    );

    let busy = pacing.frame(3).expect("frame 3");
    assert_eq!(
        busy.outcome(),
        Outcome::Late {
            by: us(10_000),
            cause: LateCause::BeganLate,
        }
    );
    assert_eq!(busy.began_late(), Some(us(10_000)));
    assert_eq!(busy.opportunity_late(), None);

    let woke_late = pacing.frame(4).expect("frame 4");
    assert_eq!(woke_late.began_late(), Some(us(3_000)));
    assert_eq!(
        woke_late.why().map(ToString::to_string).as_deref(),
        Some("began late, observed; began 3.00 ms late")
    );
    assert!(matches!(
        woke_late.outcome(),
        Outcome::Late {
            cause: LateCause::BeganLate,
            ..
        }
    ));

    let counts = pacing.counts();
    assert_eq!(
        (
            counts.late_causes.skipped_opportunity,
            counts.late_causes.began_late
        ),
        (1, 2)
    );
    let opportunities = pacing.opportunities();
    assert_eq!(
        (
            opportunities.recorded,
            opportunities.skipped,
            opportunities.unplaced
        ),
        (6, 1, 0)
    );
    assert_eq!(pacing.opportunity_intervals().max(), us(20_000));
    assert_eq!(pacing.begin_lateness().count(), 2);
}

#[test]
fn a_frame_that_expired_waiting_says_its_opportunity_never_came() {
    use footfall_core::BeginsAt::Opportunity;
    use footfall_core::DropReason;
    use footfall_core::FramePointKind::{Dropped, Scheduled};
    let mut recording = Recording::new(11);
    recording.clock_sync(0, &HOST, 0);
    for time in [0, 10_000, 30_000] {
        recording.opportunity(time, FrameStream::DEFAULT, time, Some(10_000));
    }
    // Held for the opportunity at 20 ms, which never came, it was dropped at
    // the next without beginning.
    recording.frame_point(30_000, 1, Scheduled(Opportunity), &HOST, Some(15_000));
    recording.frame_point(30_000, 1, Dropped(DropReason::MissedDeadline), &HOST, None);
    let trace = Trace::from_snapshot(&recording.snapshot(40_000));
    let frame = trace.pacing(us(500)).frame(1).cloned().expect("the frame");
    assert!(matches!(
        frame.outcome(),
        footfall_analysis::Outcome::Dropped { .. }
    ));
    assert_eq!(frame.opportunity_late(), Some(us(10_000)));
    assert!(!frame.begin_judged(), "it never began");
    assert_eq!(frame.began_late(), None);
}

#[test]
fn opportunities_are_counted_per_stream_and_judged_only_where_recorded() {
    use footfall_analysis::LateCause;
    use footfall_core::BeginsAt::Opportunity;
    let mut recording = Recording::new(10);
    recording.clock_sync(0, &HOST, 0);
    let window = FrameStream::new(1);
    let inspector = FrameStream::new(2);
    // Without intervals, the median spacing says when the next was due.
    for time in [0, 10_000, 20_000, 40_000] {
        recording.opportunity(time, window, time, None);
    }
    // Events lost between these two leave the wait between them unjudged.
    recording.opportunity(41_000, inspector, 41_000, Some(10_000));
    let lost = recording.marker(50_000, MarkerPhase::Begin);
    recording.opportunity(71_000, inspector, 71_000, Some(10_000));
    // A frame of a third stream, which recorded no opportunities, cannot be
    // judged on when it began.
    let unrecorded = FrameStream::new(3).frame(1);
    recording.record(
        1_000,
        MAIN,
        Observation::FrameMarker {
            kind: MarkerKind::Frame,
            phase: MarkerPhase::Begin,
            number: Some(unrecorded),
            context_id: ContextId::new(0),
        },
    );
    recording.record(
        3_000,
        MAIN,
        Observation::FrameMarker {
            kind: MarkerKind::Frame,
            phase: MarkerPhase::End,
            number: Some(unrecorded),
            context_id: ContextId::new(0),
        },
    );
    use footfall_core::FramePointKind::{Deadline, Presented, Scheduled, Target};
    recording.frame_point(3_000, unrecorded, Scheduled(Opportunity), &HOST, Some(0));
    recording.frame_point(3_000, unrecorded, Deadline, &HOST, Some(8_000));
    recording.frame_point(3_000, unrecorded, Target, &HOST, Some(10_000));
    recording.frame_point(3_000, unrecorded, Presented, &HOST, Some(20_000));
    let snapshot = recording.window(0, recording.next(), &[lost], 90_000);
    let trace = Trace::from_snapshot(&snapshot);
    let pacing = trace.pacing(us(500));

    let opportunities = pacing.opportunities();
    assert_eq!((opportunities.recorded, opportunities.skipped), (6, 1));
    let window_only = pacing.in_stream(window).opportunities();
    assert_eq!((window_only.recorded, window_only.skipped), (4, 1));
    assert_eq!(pacing.in_stream(inspector).opportunities().skipped, 0);

    let frame = pacing.frame(unrecorded).expect("the frame");
    assert!(!frame.begin_judged());
    assert_eq!(
        frame.why().map(ToString::to_string).as_deref(),
        Some("late after submit, no GPU work to tell, unresolved; when it began was not judged")
    );
    assert_eq!(
        footfall_analysis::Outcome::Dropped {
            reason: footfall_core::DropReason::MissedDeadline
        }
        .to_string(),
        "dropped: missed its deadline"
    );
    assert_eq!(
        footfall_analysis::Outcome::Early { by: us(850) }.to_string(),
        "early by 850 µs"
    );
    let why = frame.why().expect("a late frame says why");
    assert!(!why.begin_judged);
    assert_eq!(why.cause, LateCause::AfterSubmit);
}

#[test]
fn steady_latency_is_told_apart_from_hitches() {
    use footfall_analysis::{Cadence, Outcome};
    use footfall_core::DropReason;
    use footfall_core::FramePointKind::{Deadline, Dropped, Presented, Target};
    let mut recording = Recording::new(12);
    recording.clock_sync(0, &HOST, 0);
    let refresh = 16_667;
    // Shown this many refreshes after their targets: frame 2 falls one
    // behind, 3 to 5 stay one behind, and 6 catches up. Frame 7 is dropped
    // as superseded, 8 is shown on time, 9 says nothing, and 10 follows.
    let shown_late = [0, 1, 1, 1, 1, 0, 0, 0, 0, 0];
    for number in 1..=10_u64 {
        let target = number * refresh;
        let start = target - refresh;
        recording.numbered_frame(number, start, start + 3_000);
        let at = start + 3_000;
        recording.frame_point(at, number, Deadline, &HOST, Some(target - 4_000));
        recording.frame_point(at, number, Target, &HOST, Some(target));
        match number {
            7 => recording.frame_point(at, number, Dropped(DropReason::Superseded), &HOST, None),
            9 => {}
            _ => {
                let late = shown_late[usize::try_from(number - 1).unwrap()] * refresh;
                recording.frame_point(at, number, Presented, &HOST, Some(target + late));
            }
        }
    }
    let trace = Trace::from_snapshot(&recording.snapshot(200_000));
    let pacing = trace.pacing(us(500));
    let cadence = |number: u64| pacing.frame(number).and_then(|frame| frame.cadence());

    assert_eq!(cadence(1), None, "the first shown frame has none before it");
    assert_eq!(cadence(2), Some(Cadence::Stalled { by: us(refresh) }));
    for number in 3..=5 {
        assert_eq!(cadence(number), Some(Cadence::Steady), "frame {number}");
        assert!(matches!(
            pacing.frame(number).unwrap().outcome(),
            Outcome::Late { .. }
        ));
    }
    assert_eq!(cadence(6), Some(Cadence::Hurried { by: us(refresh) }));
    assert_eq!(cadence(7), None, "never shown");
    assert_eq!(
        cadence(8),
        Some(Cadence::Steady),
        "the superseded frame left frame 6 on screen as long as their targets were apart"
    );
    assert_eq!(cadence(9), None);
    assert_eq!(
        cadence(10),
        None,
        "what was on screen before it is not known"
    );
    assert_eq!(
        Cadence::Stalled { by: us(refresh) }.to_string(),
        "stalled by 16.67 ms"
    );

    let counts = pacing.cadence();
    assert_eq!(
        (
            counts.steady,
            counts.late_but_steady,
            counts.stalled,
            counts.hurried
        ),
        (4, 3, 1, 1)
    );
    assert_eq!((counts.judged(), counts.hitches()), (6, 2));
}

#[test]
fn late_frames_say_whether_they_missed_refreshes_or_their_target() {
    use footfall_analysis::{LateCause, Outcome, Party, Support};
    use footfall_core::FramePointKind::{Deadline, Presented, Target};
    let mut recording = Recording::new(13);
    recording.clock_sync(0, &HOST, 0);
    // A display refreshing every 10 ms, which says so.
    for time in (0..=60_000).step_by(10_000) {
        recording.opportunity(time, FrameStream::DEFAULT, time, Some(10_000));
    }
    // Both submitted in time: one shown a refresh late, one 4 ms late.
    for (number, presented) in [(1_u64, 30_000), (2, 44_000)] {
        let target = number * 20_000;
        recording.numbered_frame(number, target - 10_000, target - 8_000);
        let at = target - 8_000;
        recording.frame_point(at, number, Deadline, &HOST, Some(target - 2_000));
        recording.frame_point(at, number, Target, &HOST, Some(target));
        recording.frame_point(at, number, Presented, &HOST, Some(presented));
    }
    let trace = Trace::from_snapshot(&recording.snapshot(70_000));
    let pacing = trace.pacing(us(500));

    let missed_one = pacing.frame(1).and_then(|frame| frame.why()).unwrap();
    assert_eq!(missed_one.whole_refreshes, Some(true));
    assert_eq!(missed_one.cause, LateCause::AfterSubmit);

    let between = pacing.frame(2).unwrap();
    assert_eq!(
        between.outcome(),
        Outcome::Late {
            by: us(4_000),
            cause: LateCause::Mispredicted,
        }
    );
    let why = between.why().unwrap();
    assert_eq!(
        (why.whole_refreshes, why.support),
        (Some(false), Support::Inferred)
    );
    assert_eq!(why.cause.party(), Party::Platform);
    assert_eq!(
        why.to_string(),
        "its target was mispredicted, inferred; shown between refreshes; when it began was not judged"
    );
    assert_eq!(
        between.outcome().to_string(),
        "late by 4.00 ms: its target was mispredicted"
    );
    assert_eq!(pacing.counts().late_causes.mispredicted, 1);
    assert_eq!(LateCause::MissedDeadline.party(), Party::Program);
    assert_eq!(LateCause::Backlog.party(), Party::Queue);
}

#[test]
fn whole_refreshes_are_not_judged_without_a_stated_interval() {
    use footfall_analysis::LateCause;
    use footfall_core::FramePointKind::{Deadline, Presented, Target};
    let mut recording = Recording::new(14);
    recording.clock_sync(0, &HOST, 0);
    for time in (0..=40_000).step_by(10_000) {
        recording.opportunity(time, FrameStream::DEFAULT, time, None);
    }
    recording.numbered_frame(1, 10_000, 12_000);
    recording.frame_point(12_000, 1, Deadline, &HOST, Some(18_000));
    recording.frame_point(12_000, 1, Target, &HOST, Some(20_000));
    recording.frame_point(12_000, 1, Presented, &HOST, Some(24_000));
    let trace = Trace::from_snapshot(&recording.snapshot(50_000));
    let why = *trace.pacing(us(500)).frame(1).unwrap().why().unwrap();
    assert_eq!(why.whole_refreshes, None);
    assert_eq!(why.cause, LateCause::AfterSubmit);
}

#[test]
fn a_lasting_change_in_latency_is_a_step() {
    use footfall_core::FramePointKind::Presented;
    let mut recording = Recording::new(15);
    recording.clock_sync(0, &HOST, 0);
    let refresh = 8_333;
    for time in (0..60).map(|n| n * refresh) {
        recording.opportunity(time, FrameStream::DEFAULT, time, Some(refresh));
    }
    // Ten frames shown three refreshes after they began, one outlier five
    // refreshes after, then eight four refreshes after, as if a frame were
    // queued, then ten back at three.
    let refreshes: Vec<u64> = core::iter::repeat_n(3, 10)
        .chain([5])
        .chain(core::iter::repeat_n(3, 4))
        .chain(core::iter::repeat_n(4, 8))
        .chain(core::iter::repeat_n(3, 10))
        .collect();
    for (number, late) in (1_u64..).zip(&refreshes) {
        let begin = number * refresh;
        recording.numbered_frame(number, begin, begin + 1_000);
        recording.frame_point(
            begin + 1_000,
            number,
            Presented,
            &HOST,
            Some(begin + late * refresh),
        );
    }
    let trace = Trace::from_snapshot(&recording.snapshot(1_000_000));
    let steps = trace.pacing(us(500)).latency_steps();
    let three = us(3 * refresh);
    let four = us(4 * refresh);
    assert_eq!(steps.len(), 2, "{steps:?}");
    assert_eq!(
        (
            steps[0].at.number(),
            steps[0].from,
            steps[0].to,
            steps[0].rose()
        ),
        (16, three, four, true),
        "the outlier at frame 11 is not a step"
    );
    assert_eq!(
        (
            steps[1].at.number(),
            steps[1].from,
            steps[1].to,
            steps[1].rose()
        ),
        (24, four, three, false)
    );
    assert_eq!(
        steps[0].to_string(),
        "latency rose from 25.00 ms to 33.33 ms at frame 16"
    );
}

#[test]
fn a_frame_late_for_want_of_a_buffer_blames_the_queue() {
    use footfall_analysis::{LateCause, Party, Support};
    use footfall_core::FramePointKind::{
        BufferAcquired, BufferRequested, Deadline, Presented, Target,
    };
    let mut recording = Recording::new(16);
    recording.clock_sync(0, &HOST, 0);
    // Frame 1 waits 3 ms for a buffer and is submitted 2 ms late; frame 2
    // waits 1 ms and is submitted 4 ms late. Both are shown a refresh late.
    for (number, wait, late) in [(1_u64, 3_000, 2_000), (2, 1_000, 4_000)] {
        let start = number * 20_000;
        let deadline = start + 8_000;
        recording.numbered_frame(number, start, deadline + late);
        let at = deadline + late;
        recording.frame_point(at, number, BufferRequested, &HOST, Some(start + 1_000));
        recording.frame_point(
            at,
            number,
            BufferAcquired,
            &HOST,
            Some(start + 1_000 + wait),
        );
        recording.frame_point(at, number, Deadline, &HOST, Some(deadline));
        recording.frame_point(at, number, Target, &HOST, Some(start + 10_000));
        recording.frame_point(at, number, Presented, &HOST, Some(start + 20_000));
    }
    let trace = Trace::from_snapshot(&recording.snapshot(100_000));
    let pacing = trace.pacing(us(500));

    let waited = pacing.frame(1).unwrap();
    assert_eq!(waited.buffer_wait(), Some(us(3_000)));
    let why = waited.why().unwrap();
    assert_eq!(
        (why.cause, why.support, why.cause.party()),
        (LateCause::WaitedForBuffer, Support::Observed, Party::Queue)
    );
    assert_eq!(
        why.to_string(),
        "waited for a buffer, observed; submitted 2.00 ms after its deadline; waited 3.00 ms for a buffer; when it began was not judged"
    );

    let slow = pacing.frame(2).unwrap().why().unwrap();
    assert_eq!(
        slow.cause,
        LateCause::MissedDeadline,
        "it was late by more than it waited"
    );
    assert_eq!(slow.buffer_wait, Some(us(1_000)));

    let counts = pacing.counts().late_causes;
    assert_eq!((counts.waited_for_buffer, counts.missed_deadline), (1, 1));
    let waits = pacing.buffer_waits();
    assert_eq!((waits.count(), waits.max()), (2, us(3_000)));
}

#[test]
fn misses_and_hitches_are_partial_or_full_against_the_refresh() {
    use footfall_analysis::Severity;
    use footfall_core::DropReason;
    use footfall_core::FramePointKind::{Deadline, Dropped, Presented, Target};
    let mut recording = Recording::new(17);
    recording.clock_sync(0, &HOST, 0);
    let refresh = 10_000;
    for time in (0..12).map(|n| n * refresh) {
        recording.opportunity(time, FrameStream::DEFAULT, time, Some(refresh));
    }
    // How late each frame is shown, in µs: 2 stalls by a refresh, 3 stays as
    // late, 4 catches up, 5 stalls by 4 ms, 6 catches up, 7 is dropped for a
    // miss, and 8 follows on time.
    let shown_late = [0, refresh, refresh, 0, 4_000, 0, 0, 0];
    for (number, late) in (1_u64..).zip(shown_late) {
        let target = number * refresh;
        recording.numbered_frame(number, target - refresh, target - refresh + 2_000);
        let at = target - refresh + 2_000;
        recording.frame_point(at, number, Deadline, &HOST, Some(target - 4_000));
        recording.frame_point(at, number, Target, &HOST, Some(target));
        if number == 7 {
            recording.frame_point(at, number, Dropped(DropReason::MissedDeadline), &HOST, None);
        } else {
            recording.frame_point(at, number, Presented, &HOST, Some(target + late));
        }
    }
    let trace = Trace::from_snapshot(&recording.snapshot(200_000));
    let pacing = trace.pacing(us(500));
    let severity = |number: u64| pacing.frame(number).unwrap().severity();
    assert_eq!(severity(1), None);
    assert_eq!(severity(2), Some(Severity::Full), "stalled a refresh");
    assert_eq!(severity(3), None, "late as steadily as frame 2: latency");
    assert_eq!(severity(4), Some(Severity::Full), "hurried a refresh");
    assert_eq!(severity(5), Some(Severity::Partial), "stalled 4 ms");
    assert_eq!(severity(6), Some(Severity::Partial), "hurried 4 ms");
    assert_eq!(severity(7), Some(Severity::Full), "dropped for a miss");
    let counts = pacing.severity();
    assert_eq!((counts.partial, counts.full), (2, 3));
    assert_eq!(Severity::Partial.to_string(), "partial");
}
