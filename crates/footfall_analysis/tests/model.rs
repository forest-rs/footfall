// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The trace model on hand-built snapshots, with exact times: nesting and
//! identity, how spans count toward ranges, grouping, what answers cover,
//! gaps, comparisons, and what a timeline draws.

use core::time::Duration;
use footfall_analysis::{
    Caveat, Comparison, Continuity, DurationStats, FrameId, GroupBy, KeyPart, Metric, Population,
    PushError, Recorded, Selection, SpanId, TimeRange, Trace, Zone,
};
use footfall_core::{
    Args, Context, ContextId, CounterDesc, EventDesc, FlowId, FlowPhase, InstrumentationLevel,
    LabelKey, MarkerKind, MarkerPhase, ResourceId, ResourceKindDesc, ScopeDesc, ScopeToken,
};
use footfall_runtime::{
    CaptureSettings, ContextSnapshot, CounterKind, EventWindow, Observation, Snapshot,
    SnapshotMetadata, ThreadSnapshot, TimedObservation,
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

/// Events recorded by hand, with sequence numbers in call order.
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
        self.events
            .push(TimedObservation::new(at, sequence, thread, observation));
        sequence
    }

    fn begin(
        &mut self,
        at: u64,
        thread: u64,
        desc: &'static ScopeDesc,
        context: u64,
    ) -> ScopeToken {
        let token = ScopeToken::new(self.next() + 1);
        self.record(
            at,
            thread,
            Observation::ScopeBegin {
                desc_id: desc.id(),
                name: desc.name(),
                context_id: ContextId::new(context),
                token,
                args: Args::none(),
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
        let id = SpanId::new(self.next());
        let token = self.begin(begin, thread, desc, context);
        self.end(end, thread, token);
        id
    }

    fn marker(&mut self, at: u64, phase: MarkerPhase) -> u64 {
        self.record(
            at,
            MAIN,
            Observation::FrameMarker {
                kind: MarkerKind::Frame,
                phase,
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

    /// A snapshot holding the events in `[since, next)` except `missing`.
    fn window(&self, since: u64, next: u64, missing: &[u64], captured_at: u64) -> OwnedSnapshot {
        let capture = CaptureSettings::new(self.level, 1024, 1024, 64).with_counter_samples(true);
        let metadata = SnapshotMetadata::new("test")
            .with_instance_id(self.instance)
            .with_producer_version("1.0.0")
            .with_capture(capture);
        let mut builder = Snapshot::builder(metadata)
            .captured_at_micros(captured_at)
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
