// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A runtime instance's observations, indexed for querying and drawing.

use crate::basis::{Gap, Recorded, Signal};
use crate::frame::{Frame, FrameCounter, FrameId};
use crate::query::SpanQuery;
use crate::signal::{CounterSample, CounterSeries, Flow, FlowStep, Message, Resource};
use crate::span::{OpenSpan, Placement, Span, SpanId, Track, TrackKind};
use crate::time::{TimeRange, nanos};
use core::fmt;
use core::time::Duration;
use footfall_core::{
    ClockId, ContextId, CounterDescId, FlowId, FlowPhase, FrameNumber, FramePointKind, FrameStream,
    InstrumentationLevel, MarkerKind, MarkerPhase, ResourceId, ResourceKindId, ScopeDescId,
    ScopeToken, TimelineId, TimingBasis,
};
use footfall_runtime::{CounterKind, Drops, SnapshotMetadata};
use footfall_session::{
    Clocks, OwnedArgEntry, OwnedClockTicks, OwnedContextSnapshot, OwnedCounterSnapshot,
    OwnedHistogramSnapshot, OwnedLabelValue, OwnedObservation, OwnedSnapshot, OwnedSourceLocation,
    OwnedTimelineWork, SnapshotKind, capture::CaptureReader,
};
use hashbrown::{HashMap, HashSet};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Everything one runtime instance recorded, as far as its snapshots show,
/// indexed for the questions an investigation or a timeline asks.
///
/// Build one from a snapshot with [`from_snapshot`](Self::from_snapshot),
/// or [`push`](Self::push) each window of a subscription or capture file in
/// turn. A trace pairs scope begins and ends into [`Span`]s on per-thread
/// [`Track`]s, markers into [`Frame`]s, and keeps counter samples, messages,
/// resource lifetimes, and flows. It also keeps track of what it did not
/// see: [`gaps`](Self::gaps) where events are missing, spans still
/// [open](Self::open_spans), and whether each kind of observation was
/// [recorded](Self::recorded) at all.
///
/// [`spans`](Self::spans) starts a query; every answer carries a
/// [`Basis`](crate::Basis) saying what it covers.
///
/// A trace holds one runtime instance, since ids and sequence numbers mean
/// nothing across instances; a snapshot from another instance is refused,
/// and a consumer that follows a restarting producer starts a new trace. A
/// trace that follows a live session grows without bound unless the
/// consumer calls [`forget_before`](Self::forget_before).
#[derive(Clone, Debug, Default)]
pub struct Trace {
    instance_id: Option<u64>,
    metadata: Option<SnapshotMetadata>,
    drops: Drops,
    captured_at: Duration,
    next_sequence: u64,
    last_event_at: Duration,
    gaps: Vec<Gap>,
    forgotten_before: Duration,
    names: HashSet<Arc<str>>,
    tracks: BTreeMap<u64, Track>,
    timelines: BTreeMap<TimelineId, Track>,
    /// Frame points, by the frame's number: the kind, the time read from
    /// another clock, and when the point was recorded.
    frame_points: BTreeMap<FrameNumber, Vec<HeldPoint>>,
    /// Frame opportunities, by the frame stream they were for, in the
    /// order they were recorded.
    frame_opportunities: BTreeMap<FrameStream, Vec<HeldOpportunity>>,
    /// Frame plans, by the frame's number: the latest recorded, and when.
    frame_plans: BTreeMap<FrameNumber, (crate::pacing::Plan, Duration)>,
    /// Placed timeline spans reported with a frame number, by that number.
    frame_spans: HashMap<FrameNumber, Vec<SpanId>>,
    /// Timeline spans whose clock has no path to the runtime's yet, with
    /// their event's sequence number, reporting thread, and time.
    unplaced: Vec<(u64, u64, Duration, Box<OwnedTimelineWork>)>,
    /// Placed timeline spans whose times lie past their clock's last
    /// reading, to convert again when more readings arrive; as `unplaced`.
    provisional: Vec<(u64, u64, Duration, Box<OwnedTimelineWork>)>,
    /// The clocks with readings since timeline spans were last placed.
    recalibrated: HashSet<ClockId>,
    contexts: BTreeMap<ContextId, OwnedContextSnapshot>,
    span_index: HashMap<SpanId, (Lane, u32, Duration)>,
    begun: HashMap<ScopeToken, Begun>,
    stacks: HashMap<u64, Vec<ScopeToken>>,
    unmatched_ends: Vec<Duration>,
    marks: [Vec<Frame>; 3],
    open_marks: [Vec<OpenMark>; 3],
    /// Completed marks with a program number, by kind and number.
    numbered: HashMap<(usize, FrameNumber), FrameId>,
    series: BTreeMap<(CounterDescId, ContextId), CounterSeries>,
    /// Whether every event since the runtime began has been seen without a
    /// gap, so a counter's first sample is its whole change.
    complete_history: bool,
    counters: Vec<OwnedCounterSnapshot>,
    histograms: Vec<OwnedHistogramSnapshot>,
    messages: Vec<Message>,
    resources: Vec<Resource>,
    live_resources: HashMap<(ResourceKindId, ResourceId), usize>,
    flows: BTreeMap<FlowId, Flow>,
    clocks: Clocks,
}

/// A scope begun and not yet ended.
#[derive(Clone, Debug)]
struct Begun {
    id: SpanId,
    desc_id: ScopeDescId,
    name: Arc<str>,
    location: Option<OwnedSourceLocation>,
    context_id: ContextId,
    thread_id: u64,
    begin: Duration,
    depth: u32,
    parent: Option<SpanId>,
    child_time: Duration,
    interrupted: bool,
    args: Vec<OwnedArgEntry>,
}

/// A marker begun and not yet ended.
#[derive(Clone, Copy, Debug)]
struct OpenMark {
    id: FrameId,
    number: Option<FrameNumber>,
    context_id: ContextId,
    thread_id: u64,
    begin: Duration,
}

/// A frame point as a trace holds it.
#[derive(Clone, Debug)]
pub(crate) struct HeldPoint {
    pub(crate) kind: FramePointKind,
    /// The time it gives, in another clock's ticks.
    pub(crate) time: Option<OwnedClockTicks>,
    pub(crate) basis: TimingBasis,
    pub(crate) args: Vec<OwnedArgEntry>,
    /// When it was recorded.
    pub(crate) at: Duration,
}

/// A frame opportunity as a trace holds it.
#[derive(Clone, Debug)]
pub(crate) struct HeldOpportunity {
    /// When, in another clock's ticks.
    pub(crate) time: OwnedClockTicks,
    /// The interval the platform meant to leave before the next, in the
    /// same clock's ticks.
    pub(crate) interval: Option<u64>,
    /// When it was recorded.
    pub(crate) at: Duration,
}

/// Which track a span is on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Lane {
    Thread(u64),
    Timeline(TimelineId),
}

/// How a pushed snapshot followed the ones before it, as
/// [`Pushed::continuity`] returns it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Continuity {
    /// The trace's first snapshot, which held every event since the runtime
    /// began.
    First,
    /// It continued the previous snapshot with no events missing.
    Continuous,
    /// Events are missing before or within it; [`Trace::gaps`] says where.
    Gap {
        /// How many events are missing.
        missed: u64,
    },
}

/// What one [`Trace::push`] added.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pushed {
    continuity: Continuity,
    spans: Vec<SpanId>,
    moved: Vec<SpanId>,
    frames: Vec<FrameId>,
}

impl Pushed {
    /// Returns how the snapshot followed the ones before it.
    #[must_use]
    pub const fn continuity(&self) -> Continuity {
        self.continuity
    }

    /// Returns the spans that ended in the snapshot, including ones that
    /// began in an earlier one.
    #[must_use]
    pub fn spans(&self) -> &[SpanId] {
        &self.spans
    }

    /// Returns the timeline spans placed by earlier snapshots that moved
    /// because this one brought new readings of their clocks. They were
    /// placed past their clock's last reading at the time, at its nominal
    /// tick period, and the readings place them better.
    #[must_use]
    pub fn moved_spans(&self) -> &[SpanId] {
        &self.moved
    }

    /// Returns the frames, passes, and transactions that ended in the
    /// snapshot.
    #[must_use]
    pub fn frames(&self) -> &[FrameId] {
        &self.frames
    }
}

/// Why [`Trace::push`] refused a snapshot. The trace is unchanged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum PushError {
    /// The snapshot comes from another runtime instance, such as a
    /// restarted producer; start a new trace for it.
    OtherInstance {
        /// The trace's instance.
        expected: u64,
        /// The snapshot's instance.
        found: u64,
    },
    /// The snapshot holds only changes; apply it to the previous full
    /// snapshot with `OwnedSnapshot::apply` first.
    ChangesOnly,
}

impl fmt::Display for PushError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OtherInstance { expected, found } => write!(
                f,
                "the snapshot is from runtime instance {found:#x}, not this trace's {expected:#x}; \
                 start a new trace for it"
            ),
            Self::ChangesOnly => f.write_str(
                "the snapshot holds only changes; apply it to the previous full snapshot first",
            ),
        }
    }
}

impl core::error::Error for PushError {}

const fn mark_index(kind: MarkerKind) -> Option<usize> {
    match kind {
        MarkerKind::Frame => Some(0),
        MarkerKind::Transaction => Some(1),
        MarkerKind::Pass => Some(2),
        _ => None,
    }
}

/// A trace of a whole capture, as [`Trace::from_capture`] returns it, with
/// what could not be added.
#[derive(Debug)]
pub struct CapturedTrace {
    trace: Trace,
    skipped: u64,
    truncated: Option<std::io::Error>,
}

impl CapturedTrace {
    /// Returns the trace.
    #[must_use]
    pub const fn trace(&self) -> &Trace {
        &self.trace
    }

    /// Returns the trace, leaving the rest.
    #[must_use]
    pub fn into_trace(self) -> Trace {
        self.trace
    }

    /// Returns how many snapshots the trace refused: from another runtime
    /// instance, or holding only changes.
    #[must_use]
    pub const fn skipped(&self) -> u64 {
        self.skipped
    }

    /// Returns the error that cut the capture short, when it ends partway
    /// through a snapshot. The snapshots before it are in the trace.
    #[must_use]
    pub const fn truncated(&self) -> Option<&std::io::Error> {
        self.truncated.as_ref()
    }

    /// Returns whether every snapshot in the capture is in the trace: none
    /// skipped, and the capture not cut short.
    #[must_use]
    pub const fn complete(&self) -> bool {
        self.skipped == 0 && self.truncated.is_none()
    }
}

impl Trace {
    /// Creates a trace that has seen no snapshot.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a trace of one snapshot.
    ///
    /// # Panics
    ///
    /// Panics when the snapshot holds only changes; subscriptions and
    /// capture files hand out full snapshots.
    #[must_use]
    pub fn from_snapshot(snapshot: &OwnedSnapshot) -> Self {
        let mut trace = Self::new();
        trace
            .push(snapshot)
            .expect("a new trace accepts any full snapshot");
        trace
    }

    /// Creates a trace of a whole capture, pushing each snapshot in turn as
    /// a live consumer would, so events lost between them become
    /// [`gaps`](Self::gaps) where they were lost.
    ///
    /// Snapshots from another runtime instance than the first, such as a
    /// producer that restarted while it was recorded, are refused and
    /// counted in [`CapturedTrace::skipped`]. A capture cut short, as one
    /// still being written can be, keeps its complete snapshots and says so
    /// in [`CapturedTrace::truncated`].
    ///
    /// Fails for a capture with no snapshots, and for any error other than a
    /// truncated end.
    pub fn from_capture<R: std::io::Read>(
        reader: CaptureReader<R>,
    ) -> std::io::Result<CapturedTrace> {
        let mut trace = Self::new();
        let mut skipped = 0;
        let mut truncated = None;
        for snapshot in reader {
            let snapshot = match snapshot {
                Ok(snapshot) => snapshot,
                Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                    truncated = Some(error);
                    break;
                }
                Err(error) => return Err(error),
            };
            if trace.push(&snapshot).is_err() {
                skipped += 1;
            }
        }
        if trace.instance_id().is_none() {
            return Err(truncated.unwrap_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "the capture holds no snapshots",
                )
            }));
        }
        Ok(CapturedTrace {
            trace,
            skipped,
            truncated,
        })
    }

    /// Adds a snapshot of the trace's runtime instance: the events it holds
    /// that the trace has not seen, and its current threads, contexts,
    /// counters, histograms, and metadata.
    ///
    /// Snapshots may overlap, as consecutive full snapshots do; events the
    /// trace already holds are skipped. Events missing between or within
    /// snapshots become [`gaps`](Self::gaps): spans open across a gap are
    /// marked [interrupted](Span::interrupted), nesting starts over, and
    /// counter changes are unknown until the next sample.
    pub fn push(&mut self, snapshot: &OwnedSnapshot) -> Result<Pushed, PushError> {
        if snapshot.kind() == SnapshotKind::Changes {
            return Err(PushError::ChangesOnly);
        }
        let found = snapshot.metadata().instance_id();
        if let Some(expected) = self.instance_id
            && expected != found
        {
            return Err(PushError::OtherInstance { expected, found });
        }
        let first = self.instance_id.is_none();
        self.instance_id = Some(found);
        if first {
            self.complete_history = true;
        }

        for thread in snapshot.threads() {
            let track = self
                .tracks
                .entry(thread.id())
                .or_insert_with(|| Track::new(TrackKind::Thread(thread.id())));
            if let Some(name) = thread.name() {
                track.name = Some(name.to_owned());
            }
        }
        for context in snapshot.contexts() {
            self.contexts.insert(context.id(), context.clone());
        }

        let events: Vec<_> = snapshot
            .events()
            .iter()
            .filter(|event| event.sequence() >= self.next_sequence)
            .collect();
        let new_gaps = self.find_gaps(&events, snapshot);
        let missed: u64 = new_gaps.iter().map(|gap| gap.missed).sum();

        let mut pushed = Pushed {
            continuity: if missed > 0 {
                Continuity::Gap { missed }
            } else if first {
                Continuity::First
            } else {
                Continuity::Continuous
            },
            spans: Vec::new(),
            moved: Vec::new(),
            frames: Vec::new(),
        };
        let mut pending = new_gaps.iter().peekable();
        // The frame end whose counter samples are being collected.
        let mut collecting: Option<(FrameId, u64, Duration)> = None;
        for event in events {
            let at = nanos(event.at_nanos());
            while pending.next_if(|gap| gap.range.start() <= at).is_some() {
                self.lose_continuity();
                collecting = None;
            }
            self.clocks.observe(event);
            match event.observation() {
                OwnedObservation::ClockSync { clock } => {
                    self.recalibrated.insert(clock.clock_id());
                }
                OwnedObservation::ClockPair { first, second } => {
                    self.recalibrated.insert(first.clock_id());
                    self.recalibrated.insert(second.clock_id());
                }
                _ => {}
            }
            self.ingest(event, at, &mut pushed, &mut collecting);
            self.last_event_at = self.last_event_at.max(at);
        }
        if pending.next().is_some() {
            self.lose_continuity();
        }
        self.gaps.extend(new_gaps);
        // Clock readings may follow the work they time, so place timeline
        // spans once the whole snapshot is in, and keep trying later.
        self.place_timeline_spans(&mut pushed);

        self.next_sequence = self.next_sequence.max(snapshot.window().next());
        self.captured_at = self.captured_at.max(nanos(snapshot.captured_at_nanos()));
        self.metadata = Some(snapshot.metadata().clone());
        self.drops = snapshot.drops();
        self.counters = snapshot.counters().to_vec();
        self.histograms = snapshot.histograms().to_vec();
        Ok(pushed)
    }

    /// Finds the events missing from `[next_sequence, window.next())`, from
    /// the sequence numbers of the events present, and when they fell.
    fn find_gaps(
        &self,
        events: &[&footfall_session::OwnedTimedObservation],
        snapshot: &OwnedSnapshot,
    ) -> Vec<Gap> {
        let mut present: Vec<(u64, Duration)> = events
            .iter()
            .map(|event| (event.sequence(), nanos(event.at_nanos())))
            .collect();
        present.sort_unstable();
        let end = snapshot.window().next().max(self.next_sequence);
        let mut holes = Vec::new();
        let mut expected = self.next_sequence;
        let mut before = self.last_event_at;
        for (sequence, at) in present {
            if sequence > expected {
                holes.push(Gap {
                    range: TimeRange::new(before.min(at), before.max(at)),
                    missed: sequence - expected,
                });
            }
            expected = sequence + 1;
            before = at;
        }
        if end > expected {
            let captured = nanos(snapshot.captured_at_nanos());
            holes.push(Gap {
                range: TimeRange::new(before, captured.max(before)),
                missed: end - expected,
            });
        }
        // Many small holes, such as one thread's evicted events between
        // another's, describe one stretch of lost time.
        holes.sort_by_key(|gap| gap.range);
        let mut merged: Vec<Gap> = Vec::with_capacity(holes.len());
        for hole in holes {
            match merged.last_mut() {
                Some(last) if hole.range.start() <= last.range.end() => {
                    last.range =
                        TimeRange::new(last.range.start(), last.range.end().max(hole.range.end()));
                    last.missed += hole.missed;
                }
                _ => merged.push(hole),
            }
        }
        merged
    }

    /// Forgets nesting and counter history across lost events: spans open
    /// now may miss nested spans or their own end, and the next counter
    /// sample's change is unknown.
    fn lose_continuity(&mut self) {
        for begun in self.begun.values_mut() {
            begun.interrupted = true;
        }
        self.stacks.clear();
        for open in &mut self.open_marks {
            open.clear();
        }
        self.complete_history = false;
        for series in self.series.values_mut() {
            series.after_gap = true;
        }
    }

    fn intern(&mut self, name: &str) -> Arc<str> {
        if let Some(interned) = self.names.get(name) {
            return Arc::clone(interned);
        }
        let interned: Arc<str> = Arc::from(name);
        self.names.insert(Arc::clone(&interned));
        interned
    }

    fn innermost(&self, thread_id: u64) -> Option<SpanId> {
        self.stacks
            .get(&thread_id)
            .and_then(|stack| stack.last())
            .and_then(|token| self.begun.get(token))
            .map(|begun| begun.id)
    }

    fn ingest(
        &mut self,
        event: &footfall_session::OwnedTimedObservation,
        at: Duration,
        pushed: &mut Pushed,
        collecting: &mut Option<(FrameId, u64, Duration)>,
    ) {
        let thread_id = event.thread_id();
        match event.observation() {
            OwnedObservation::ScopeBegin {
                desc_id,
                name,
                location,
                context_id,
                token,
                args,
            } => {
                let name = self.intern(name);
                let parent = self.innermost(thread_id);
                let stack = self.stacks.entry(thread_id).or_default();
                let depth = u32::try_from(stack.len()).unwrap_or(u32::MAX);
                stack.push(*token);
                self.begun.insert(
                    *token,
                    Begun {
                        id: SpanId::new(event.sequence()),
                        desc_id: *desc_id,
                        name,
                        location: location.clone(),
                        context_id: *context_id,
                        thread_id,
                        begin: at,
                        depth,
                        parent,
                        child_time: Duration::ZERO,
                        interrupted: false,
                        args: args.clone(),
                    },
                );
            }
            OwnedObservation::ScopeEnd { token } => {
                let Some(begun) = self.begun.remove(token) else {
                    self.unmatched_ends.push(at);
                    return;
                };
                let duration = at.saturating_sub(begun.begin);
                if let Some(stack) = self.stacks.get_mut(&begun.thread_id)
                    && let Some(position) = stack.iter().rposition(|open| open == token)
                {
                    stack.remove(position);
                    if let Some(parent) = stack.last().and_then(|parent| self.begun.get_mut(parent))
                    {
                        parent.child_time = parent.child_time.saturating_add(duration);
                    }
                }
                let span = Span {
                    id: begun.id,
                    end_sequence: event.sequence(),
                    token: *token,
                    desc_id: begun.desc_id,
                    name: begun.name,
                    location: begun.location,
                    context_id: begun.context_id,
                    thread_id: begun.thread_id,
                    begin: begun.begin,
                    end: at,
                    depth: begun.depth,
                    parent: begun.parent,
                    child_time: begun.child_time,
                    interrupted: begun.interrupted,
                    args: begun.args,
                    timeline: None,
                    frame: None,
                    placement: None,
                };
                pushed.spans.push(span.id);
                self.span_index.insert(
                    span.id,
                    (Lane::Thread(span.thread_id), span.depth, span.begin),
                );
                let thread_id = span.thread_id;
                let track = self
                    .tracks
                    .entry(thread_id)
                    .or_insert_with(|| Track::new(TrackKind::Thread(thread_id)));
                track.row_mut(span.depth).insert(span);
            }
            OwnedObservation::FrameMarker {
                kind,
                phase,
                number,
                context_id,
            } => {
                *collecting = None;
                let Some(index) = mark_index(*kind) else {
                    return;
                };
                match phase {
                    MarkerPhase::Begin => self.open_marks[index].push(OpenMark {
                        id: FrameId::new(event.sequence()),
                        number: *number,
                        context_id: *context_id,
                        thread_id,
                        begin: at,
                    }),
                    MarkerPhase::End => {
                        // A numbered end closes the begin with its number,
                        // wherever it is; any other end, the latest begin.
                        let open_marks = &mut self.open_marks[index];
                        let position = match number {
                            Some(number) => open_marks
                                .iter()
                                .rposition(|open| open.number == Some(*number)),
                            None => open_marks.len().checked_sub(1),
                        };
                        if let Some(open) = position.map(|position| open_marks.remove(position)) {
                            let frame = Frame {
                                id: open.id,
                                number: open.number,
                                kind: *kind,
                                context_id: open.context_id,
                                thread_id: open.thread_id,
                                begin: open.begin,
                                end: at,
                                counters: Vec::new(),
                            };
                            let marks = &mut self.marks[index];
                            let position = marks.partition_point(|held| {
                                (held.begin, held.id) <= (frame.begin, frame.id)
                            });
                            marks.insert(position, frame);
                            if let Some(number) = open.number {
                                self.numbered.insert((index, number), open.id);
                            }
                            pushed.frames.push(open.id);
                            if *kind == MarkerKind::Frame {
                                *collecting = Some((open.id, thread_id, at));
                            }
                        }
                    }
                }
            }
            OwnedObservation::CounterSample {
                desc_id,
                name,
                unit,
                context_id,
                kind,
                value,
            } => {
                let name = self.intern(name);
                let complete_history = self.complete_history;
                let series = self
                    .series
                    .entry((*desc_id, *context_id))
                    .or_insert_with(|| CounterSeries {
                        desc_id: *desc_id,
                        name: Arc::clone(&name),
                        unit: *unit,
                        context_id: *context_id,
                        kind: *kind,
                        samples: Vec::new(),
                        after_gap: false,
                    });
                let change = match kind {
                    // A sample from before lost events says nothing about
                    // the change since.
                    CounterKind::Counter if series.after_gap => None,
                    CounterKind::Counter => match series.samples.last() {
                        Some(before) => Some(value.saturating_sub(before.value)),
                        None if complete_history => Some(*value),
                        None => None,
                    },
                    _ => None,
                };
                series.after_gap = false;
                series.samples.push(CounterSample {
                    at,
                    sequence: event.sequence(),
                    value: *value,
                    change,
                });
                if let Some((frame_id, frame_thread, frame_end)) = *collecting
                    && frame_thread == thread_id
                    && frame_end == at
                    && let Some(frame) = self.marks[0]
                        .iter_mut()
                        .rev()
                        .find(|frame| frame.id == frame_id)
                {
                    frame.counters.push(FrameCounter {
                        desc_id: *desc_id,
                        name,
                        unit: *unit,
                        context_id: *context_id,
                        kind: *kind,
                        value: *value,
                        change,
                    });
                }
            }
            OwnedObservation::Message {
                desc_id,
                name,
                context_id,
                text,
                args,
            } => {
                let name = self.intern(name);
                let span = self.innermost(thread_id);
                self.messages.push(Message {
                    sequence: event.sequence(),
                    at,
                    thread_id,
                    desc_id: *desc_id,
                    name,
                    context_id: *context_id,
                    text: text.clone(),
                    args: args.clone(),
                    span,
                });
            }
            OwnedObservation::ResourceCreate {
                kind_id,
                kind_name,
                resource_id,
                context_id,
                size,
            } => {
                let kind_name = self.intern(kind_name);
                self.live_resources
                    .insert((*kind_id, *resource_id), self.resources.len());
                self.resources.push(Resource {
                    kind_id: *kind_id,
                    kind_name,
                    resource_id: *resource_id,
                    context_id: *context_id,
                    created: Some(at),
                    dropped: None,
                    sizes: vec![(at, *size)],
                });
            }
            OwnedObservation::ResourceUpdate {
                kind_id,
                kind_name,
                resource_id,
                context_id,
                size,
            } => {
                let resource = self.live_resource(*kind_id, kind_name, *resource_id, *context_id);
                resource.context_id = *context_id;
                resource.sizes.push((at, *size));
            }
            OwnedObservation::ResourceDrop {
                kind_id,
                kind_name,
                resource_id,
                context_id,
            } => {
                let resource = self.live_resource(*kind_id, kind_name, *resource_id, *context_id);
                resource.context_id = *context_id;
                resource.dropped = Some(at);
                self.live_resources.remove(&(*kind_id, *resource_id));
            }
            OwnedObservation::Flow {
                flow_id,
                phase,
                context_id,
            } => {
                let span = self.innermost(thread_id);
                self.flows
                    .entry(*flow_id)
                    .or_insert_with(|| Flow {
                        id: *flow_id,
                        steps: Vec::new(),
                    })
                    .steps
                    .push(FlowStep {
                        sequence: event.sequence(),
                        at,
                        thread_id,
                        phase: *phase,
                        context_id: *context_id,
                        span,
                    });
            }
            OwnedObservation::TimelineSpan { span } => {
                self.unplaced
                    .push((event.sequence(), thread_id, at, span.clone()));
            }
            OwnedObservation::FramePoint {
                number,
                kind,
                time,
                basis,
                args,
            } => {
                self.frame_points
                    .entry(*number)
                    .or_default()
                    .push(HeldPoint {
                        kind: *kind,
                        time: time.clone(),
                        basis: *basis,
                        args: args.clone(),
                        at,
                    });
            }
            OwnedObservation::FramePlan {
                number,
                period,
                interval,
                depth,
                latency_allowance,
                hold,
                kind,
                args,
            } => {
                let ticks = |ticks: u64| Duration::from_nanos(period.ticks_to_nanos(ticks));
                let plan = crate::pacing::Plan::new(
                    interval.map(ticks),
                    *depth,
                    latency_allowance.map(ticks),
                    hold.map(|(held, reason)| (ticks(held), reason)),
                    *kind,
                    args.clone(),
                );
                self.frame_plans.insert(*number, (plan, at));
            }
            OwnedObservation::FrameOpportunity {
                stream,
                time,
                interval,
                ..
            } => {
                self.frame_opportunities
                    .entry(*stream)
                    .or_default()
                    .push(HeldOpportunity {
                        time: time.clone(),
                        interval: *interval,
                        at,
                    });
            }
            _ => {}
        }
    }

    /// Places the timeline spans whose clock now leads to the runtime's,
    /// moves the ones placed past their clock's last reading when new
    /// readings arrived, then nests each timeline's new and moved spans with
    /// the spans already there.
    ///
    /// So the same observations place the same way whether they arrived in
    /// one snapshot or many, as long as each clock's readings arrive in tick
    /// order.
    fn place_timeline_spans(&mut self, pushed: &mut Pushed) {
        // Each timeline's spans to nest, and the earliest begin, new or old,
        // among them.
        let mut placed = BTreeMap::<TimelineId, (Duration, Vec<Span>)>::new();
        let recalibrated = core::mem::take(&mut self.recalibrated);
        if !recalibrated.is_empty() {
            for (sequence, thread_id, at, work) in core::mem::take(&mut self.provisional) {
                // Only new readings of its clock, or of the clock it was
                // converted through, can move it.
                let via = self
                    .span(SpanId::new(sequence))
                    .and_then(|span| span.placement)
                    .and_then(|placement| placement.via);
                if !recalibrated.contains(&work.begin().clock_id())
                    && via.is_none_or(|via| !recalibrated.contains(&via))
                {
                    self.provisional.push((sequence, thread_id, at, work));
                    continue;
                }
                let Some((span, settled)) = self.convert(sequence, thread_id, &work) else {
                    continue;
                };
                let old = self.span(span.id).map(|old| (old.begin, old.end));
                if let Some((old_begin, old_end)) = old
                    && (old_begin, old_end) != (span.begin, span.end)
                {
                    if let Some(flow) = work.flow() {
                        self.move_flow_step(flow, span.id, span.begin);
                    }
                    pushed.moved.push(span.id);
                    let (from, spans) = placed
                        .entry(work.timeline_id())
                        .or_insert((old_begin, Vec::new()));
                    *from = (*from).min(old_begin).min(span.begin);
                    spans.push(span);
                }
                if !settled {
                    self.provisional.push((sequence, thread_id, at, work));
                }
            }
        }
        for (sequence, thread_id, at, work) in core::mem::take(&mut self.unplaced) {
            match self.convert(sequence, thread_id, &work) {
                Some((span, settled)) => {
                    self.register(sequence, thread_id, &work, span.begin);
                    pushed.spans.push(span.id);
                    let (from, spans) = placed
                        .entry(work.timeline_id())
                        .or_insert((span.begin, Vec::new()));
                    *from = (*from).min(span.begin);
                    spans.push(span);
                    if !settled {
                        self.provisional.push((sequence, thread_id, at, work));
                    }
                }
                None => self.unplaced.push((sequence, thread_id, at, work)),
            }
        }
        for (timeline_id, (from, spans)) in placed {
            self.nest(timeline_id, from, spans);
        }
    }

    /// Records a newly placed timeline span's frame and flow.
    fn register(
        &mut self,
        sequence: u64,
        thread_id: u64,
        work: &OwnedTimelineWork,
        begin_at: Duration,
    ) {
        let id = SpanId::new(sequence);
        if let Some(number) = work.frame() {
            self.frame_spans.entry(number).or_default().push(id);
        }
        if let Some(flow) = work.flow() {
            let steps = &mut self
                .flows
                .entry(flow)
                .or_insert_with(|| Flow {
                    id: flow,
                    steps: Vec::new(),
                })
                .steps;
            let step = FlowStep {
                sequence,
                at: begin_at,
                thread_id,
                phase: FlowPhase::End,
                context_id: work.context_id(),
                span: Some(id),
            };
            let at =
                steps.partition_point(|held| (held.at, held.sequence) <= (step.at, step.sequence));
            steps.insert(at, step);
        }
    }

    /// Moves a flow's step through a timeline span to the span's new begin.
    fn move_flow_step(&mut self, flow: FlowId, id: SpanId, begin_at: Duration) {
        let Some(flow) = self.flows.get_mut(&flow) else {
            return;
        };
        for step in &mut flow.steps {
            if step.span == Some(id) {
                step.at = begin_at;
            }
        }
        flow.steps.sort_by_key(|step| (step.at, step.sequence));
    }

    /// Converts a timeline span's times to the runtime's clock, as a span
    /// that [`nest`](Self::nest) places, and says whether later clock
    /// readings could still move it. Returns `None` while its clock does not
    /// yet lead to the runtime's.
    fn convert(
        &mut self,
        sequence: u64,
        thread_id: u64,
        work: &OwnedTimelineWork,
    ) -> Option<(Span, bool)> {
        let clock = work.begin().clock_id();
        let begin = self.clocks.to_runtime(clock, work.begin().ticks())?;
        let end = self.clocks.to_runtime(clock, work.end())?;
        let begin_at = nanos(begin.nanos());
        let end_at = nanos(end.nanos()).max(begin_at);
        let id = SpanId::new(sequence);
        let name = self.intern(work.name());
        let timeline_id = work.timeline_id();
        self.timelines.entry(timeline_id).or_insert_with(|| {
            let mut track = Track::new(TrackKind::Timeline {
                id: timeline_id,
                kind: work.timeline_kind(),
            });
            track.name = Some(work.timeline_name().to_owned());
            track
        });
        let settled = begin.settled() && end.settled();
        let span = Span {
            id,
            end_sequence: sequence,
            token: ScopeToken::INVALID,
            desc_id: work.desc_id(),
            name,
            location: work.location().cloned(),
            context_id: work.context_id(),
            thread_id,
            begin: begin_at,
            end: end_at,
            depth: 0,
            parent: None,
            child_time: Duration::ZERO,
            interrupted: false,
            args: work.args().to_vec(),
            timeline: Some(timeline_id),
            frame: work.frame(),
            placement: Some(Placement {
                between_syncs: begin.between_syncs() && end.between_syncs(),
                distance: nanos(begin.distance().max(end.distance())),
                via: begin.via(),
            }),
        };
        Some((span, settled))
    }

    /// Puts new and moved spans on a timeline's track, each inside the
    /// deepest span that contains it, whatever order they were reported in.
    ///
    /// Timelines report work when it is read back, often a child before the
    /// span around it. So the spans already on the track that begin at or
    /// after `from`, the earliest begin among the new spans and the moved
    /// spans' old and new places, come off and are nested again with them; a
    /// new span can only contain spans that begin no earlier than it does.
    /// Spans that begin earlier stay where they are.
    fn nest(&mut self, timeline_id: TimelineId, from: Duration, mut spans: Vec<Span>) {
        let Some(track) = self.timelines.get_mut(&timeline_id) else {
            return;
        };
        let mut taken = Vec::new();
        for row in &mut track.rows {
            taken.extend(row.take_from(from));
        }
        for span in &mut taken {
            // A parent that stays loses the time of the child taken from it;
            // the child gives it back if it nests there again.
            if let Some(parent) = span.parent
                && let Some((_, depth, begin)) = self.span_index.get(&parent).copied()
                && begin < from
                && let Some(outer) = track.row_mut(depth).find_mut(begin, parent)
            {
                outer.child_time = outer.child_time.saturating_sub(span.duration());
            }
        }
        // A moved span replaces its old place.
        let moved: HashSet<SpanId> = spans.iter().map(|span| span.id).collect();
        spans.extend(taken.into_iter().filter(|span| !moved.contains(&span.id)));
        for span in &mut spans {
            span.parent = None;
            span.depth = 0;
            span.child_time = Duration::ZERO;
        }
        // Outer spans first: by begin, then the longest.
        spans.sort_by_key(|span| (span.begin, core::cmp::Reverse(span.end), span.id));
        let mut open: Vec<usize> = Vec::new();
        for index in 0..spans.len() {
            let (begin, end) = (spans[index].begin, spans[index].end);
            while let Some(&outer) = open.last()
                && !(spans[outer].begin <= begin && end <= spans[outer].end)
            {
                open.pop();
            }
            if let Some(&outer) = open.last() {
                spans[outer].child_time = spans[outer].child_time.saturating_add(end - begin);
                spans[index].depth = spans[outer].depth + 1;
                spans[index].parent = Some(spans[outer].id);
            } else if let Some((depth, outer)) = Self::container(track, begin, end) {
                // Inside a span that stays on the track.
                outer.child_time = outer.child_time.saturating_add(end - begin);
                spans[index].depth = depth + 1;
                spans[index].parent = Some(outer.id);
            }
            open.push(index);
        }
        for span in spans {
            self.span_index.insert(
                span.id,
                (Lane::Timeline(timeline_id), span.depth, span.begin),
            );
            track.row_mut(span.depth).insert(span);
        }
        while track.rows.last().is_some_and(|row| row.spans.is_empty()) {
            track.rows.pop();
        }
    }

    /// Returns the deepest span on `track` that contains `begin..end`, and
    /// its depth.
    fn container(track: &mut Track, begin: Duration, end: Duration) -> Option<(u32, &mut Span)> {
        let range = TimeRange::new(begin, end);
        let (depth, id, outer_begin) = (0..track.depth()).rev().find_map(|row| {
            track
                .spans_in(row, range)
                .find(|outer| outer.begin <= begin && end <= outer.end)
                .map(|outer| (row, outer.id, outer.begin))
        })?;
        track
            .row_mut(depth)
            .find_mut(outer_begin, id)
            .map(|outer| (depth, outer))
    }

    /// Returns the live resource with this id, or one created before the
    /// trace began.
    fn live_resource(
        &mut self,
        kind_id: ResourceKindId,
        kind_name: &str,
        resource_id: ResourceId,
        context_id: ContextId,
    ) -> &mut Resource {
        let index = match self.live_resources.get(&(kind_id, resource_id)) {
            Some(index) => *index,
            None => {
                let kind_name = self.intern(kind_name);
                self.live_resources
                    .insert((kind_id, resource_id), self.resources.len());
                self.resources.push(Resource {
                    kind_id,
                    kind_name,
                    resource_id,
                    context_id,
                    created: None,
                    dropped: None,
                    sizes: Vec::new(),
                });
                self.resources.len() - 1
            }
        };
        &mut self.resources[index]
    }

    /// Forgets what ended before `before`: spans, frames, samples,
    /// messages, flow steps, dropped resources, gaps, frame points, timeline
    /// spans still waiting for their clock, and clock readings no longer
    /// needed, so a trace that follows a live session keeps a bounded
    /// history.
    ///
    /// Queries then treat the time before it as unobserved. Open spans and
    /// live resources are kept, however old.
    pub fn forget_before(&mut self, before: Duration) {
        if before <= self.forgotten_before {
            return;
        }
        self.forgotten_before = before;
        for track in self.tracks.values_mut().chain(self.timelines.values_mut()) {
            for row in &mut track.rows {
                for id in row.forget(before) {
                    self.span_index.remove(&id);
                }
            }
        }
        let span_index = &self.span_index;
        self.frame_spans.retain(|_, ids| {
            ids.retain(|id| span_index.contains_key(id));
            !ids.is_empty()
        });
        for marks in &mut self.marks {
            marks.retain(|frame| frame.end >= before);
        }
        let marks = &self.marks;
        self.numbered
            .retain(|(index, _), id| marks[*index].iter().any(|frame| frame.id == *id));
        for series in self.series.values_mut() {
            series.samples.retain(|sample| sample.at >= before);
        }
        self.messages.retain(|message| message.at >= before);
        self.flows.retain(|_, flow| {
            flow.steps.retain(|step| step.at >= before);
            !flow.steps.is_empty()
        });
        self.resources
            .retain(|resource| resource.dropped.is_none_or(|dropped| dropped >= before));
        self.live_resources = self
            .resources
            .iter()
            .enumerate()
            .filter(|(_, resource)| resource.dropped.is_none())
            .map(|(index, resource)| ((resource.kind_id, resource.resource_id), index))
            .collect();
        self.unmatched_ends.retain(|at| *at >= before);
        self.gaps.retain(|gap| gap.range.end() > before);
        // A frame's points go together, once all were recorded before
        // `before` and its markers and timeline spans are gone; feedback
        // that arrives later keeps the frame's earlier points with it.
        let numbered = &self.numbered;
        let frame_spans = &self.frame_spans;
        self.frame_points.retain(|number, points| {
            points.iter().any(|point| point.at >= before)
                || numbered.contains_key(&(0, *number))
                || frame_spans.contains_key(number)
        });
        let frame_points = &self.frame_points;
        self.frame_plans.retain(|number, (_, at)| {
            *at >= before
                || frame_points.contains_key(number)
                || numbered.contains_key(&(0, *number))
                || frame_spans.contains_key(number)
        });
        for opportunities in self.frame_opportunities.values_mut() {
            opportunities.retain(|opportunity| opportunity.at >= before);
        }
        self.frame_opportunities
            .retain(|_, opportunities| !opportunities.is_empty());
        self.unplaced.retain(|(_, _, at, _)| *at >= before);
        self.provisional.retain(|(_, _, at, _)| *at >= before);
        self.clocks
            .forget_before(u64::try_from(before.as_nanos()).unwrap_or(u64::MAX));
    }

    /// Returns the runtime instance the trace holds, or `None` before its
    /// first snapshot.
    #[must_use]
    pub const fn instance_id(&self) -> Option<u64> {
        self.instance_id
    }

    /// Returns the metadata of the latest snapshot: the producer, and how it
    /// was capturing.
    #[must_use]
    pub const fn metadata(&self) -> Option<&SnapshotMetadata> {
        self.metadata.as_ref()
    }

    /// Returns what the producer could not keep or attribute, as of the
    /// latest snapshot.
    #[must_use]
    pub const fn drops(&self) -> Drops {
        self.drops
    }

    /// Returns the time the trace observed: from what it has not forgotten
    /// to when its latest snapshot was taken. Events are missing from it
    /// where [`gaps`](Self::gaps) say.
    #[must_use]
    pub fn observed(&self) -> TimeRange {
        TimeRange::new(self.forgotten_before, self.captured_at)
    }

    /// Returns the clocks other than the producer's that it recorded syncs
    /// or pairs for, which convert times read from them, such as GPU
    /// timestamps, to the trace's time.
    #[must_use]
    pub const fn clocks(&self) -> &Clocks {
        &self.clocks
    }

    /// Returns where events are missing, by time.
    #[must_use]
    pub fn gaps(&self) -> &[Gap] {
        &self.gaps
    }

    /// Returns whether the producer recorded `signal`, as of the latest
    /// snapshot.
    #[must_use]
    pub fn recorded(&self, signal: Signal) -> Recorded {
        let Some(metadata) = &self.metadata else {
            return Recorded::Unknown;
        };
        let capture = metadata.capture();
        if capture.compiled_out() {
            return Recorded::CompiledOut;
        }
        let needs = signal.level();
        if capture.level() < needs || capture.level() == InstrumentationLevel::Off {
            return Recorded::BelowLevel {
                level: capture.level(),
                needs,
            };
        }
        if signal == Signal::CounterSamples && !capture.counter_samples() {
            return Recorded::SamplesOff;
        }
        Recorded::Yes
    }

    /// Starts a query over the trace's completed spans.
    #[must_use]
    pub fn spans(&self) -> SpanQuery<'_> {
        SpanQuery::new(self)
    }

    /// Returns one track per thread that recorded anything, by thread id,
    /// then one per timeline, such as a GPU queue, by timeline id.
    pub fn tracks(&self) -> impl Iterator<Item = &Track> {
        self.tracks.values().chain(self.timelines.values())
    }

    /// Returns the track of the thread `thread_id`.
    #[must_use]
    pub fn track(&self, thread_id: u64) -> Option<&Track> {
        self.tracks.get(&thread_id)
    }

    /// Returns the timelines that are not CPU threads, such as GPU queues,
    /// by timeline id.
    pub fn timelines(&self) -> impl Iterator<Item = &Track> {
        self.timelines.values()
    }

    /// Returns the track of the timeline `timeline_id`.
    #[must_use]
    pub fn timeline(&self, timeline_id: TimelineId) -> Option<&Track> {
        self.timelines.get(&timeline_id)
    }

    /// Returns the track of the timeline named `name`.
    #[must_use]
    pub fn timeline_named(&self, name: &str) -> Option<&Track> {
        self.timelines
            .values()
            .find(|track| track.name.as_deref() == Some(name))
    }

    /// Returns each numbered frame's pacing: its CPU and GPU work, its
    /// deadline and target, when it was shown, and whether that was on
    /// time, from the frame points a frame scheduler or display reported.
    ///
    /// A frame shown no more than `tolerance` after its target counts as on
    /// time, which absorbs jitter in reported presentation times. Frame
    /// points' times are converted through the trace's clocks when the
    /// pacing is computed, so readings that arrived after a point still
    /// place it.
    #[must_use]
    pub fn pacing(&self, tolerance: Duration) -> crate::Pacing {
        crate::pacing::pacing(self, tolerance)
    }

    /// Returns the spans on timelines reported as part of the frame the
    /// program numbered `number`, such as that frame's GPU passes.
    pub fn frame_spans(&self, number: impl Into<FrameNumber>) -> impl Iterator<Item = &Span> {
        self.frame_spans
            .get(&number.into())
            .into_iter()
            .flatten()
            .filter_map(|id| self.span(*id))
    }

    pub(crate) const fn frame_points(&self) -> &BTreeMap<FrameNumber, Vec<HeldPoint>> {
        &self.frame_points
    }

    pub(crate) const fn frame_opportunities(&self) -> &BTreeMap<FrameStream, Vec<HeldOpportunity>> {
        &self.frame_opportunities
    }

    pub(crate) const fn frame_plans(
        &self,
    ) -> &BTreeMap<FrameNumber, (crate::pacing::Plan, Duration)> {
        &self.frame_plans
    }

    pub(crate) fn numbered_frames(&self) -> impl Iterator<Item = FrameNumber> + '_ {
        self.numbered
            .keys()
            .filter(|(index, _)| *index == 0)
            .map(|(_, number)| *number)
            .chain(self.frame_spans.keys().copied())
    }

    pub(crate) fn track_kind_of(&self, span: &Span) -> TrackKind {
        match span.timeline.and_then(|id| self.timelines.get(&id)) {
            Some(track) => track.kind(),
            None => TrackKind::Thread(span.thread_id),
        }
    }

    /// Returns how many timeline spans could not be placed on the trace's
    /// timeline, because their clock has no sync or pair leading to the
    /// runtime's clock.
    #[must_use]
    pub fn unplaced_spans(&self) -> usize {
        self.unplaced.len()
    }

    fn lane(&self, lane: Lane) -> Option<&Track> {
        match lane {
            Lane::Thread(thread_id) => self.tracks.get(&thread_id),
            Lane::Timeline(timeline_id) => self.timelines.get(&timeline_id),
        }
    }

    /// Returns the span with id `id`, if the trace holds it.
    #[must_use]
    pub fn span(&self, id: SpanId) -> Option<&Span> {
        let (lane, depth, begin) = *self.span_index.get(&id)?;
        self.lane(lane)?
            .rows
            .get(usize::try_from(depth).ok()?)?
            .find(begin, id)
    }

    /// Returns the span that enclosed `span` when it began.
    #[must_use]
    pub fn parent(&self, span: &Span) -> Option<&Span> {
        self.span(span.parent?)
    }

    /// Returns the spans nested directly in `span`, by begin time.
    #[must_use]
    pub fn children(&self, span: &Span) -> Vec<&Span> {
        let lane = span
            .timeline
            .map_or(Lane::Thread(span.thread_id), Lane::Timeline);
        self.lane(lane)
            .map(|track| {
                track
                    .spans_in(span.depth.saturating_add(1), span.range())
                    .filter(|child| child.parent == Some(span.id))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Returns the spans that have begun but not ended, by begin time.
    #[must_use]
    pub fn open_spans(&self) -> Vec<OpenSpan> {
        let mut open: Vec<_> = self
            .begun
            .values()
            .map(|begun| OpenSpan {
                id: begun.id,
                name: Arc::clone(&begun.name),
                location: begun.location.clone(),
                context_id: begun.context_id,
                thread_id: begun.thread_id,
                begin: begun.begin,
                depth: begun.depth,
                interrupted: begun.interrupted,
            })
            .collect();
        open.sort_by_key(|span| (span.begin, span.id));
        open
    }

    pub(crate) fn begun(
        &self,
    ) -> impl Iterator<Item = (&str, ContextId, u64, Duration, &[OwnedArgEntry])> {
        self.begun.values().map(|begun| {
            (
                &*begun.name,
                begun.context_id,
                begun.thread_id,
                begun.begin,
                begun.args.as_slice(),
            )
        })
    }

    pub(crate) fn unmatched_ends_in(&self, range: TimeRange) -> u64 {
        self.unmatched_ends
            .iter()
            .filter(|at| range.contains(**at))
            .count() as u64
    }

    /// Returns the frames, by begin time.
    #[must_use]
    pub fn frames(&self) -> &[Frame] {
        &self.marks[0]
    }

    /// Returns the frames, passes, or transactions of `kind`, by begin time.
    #[must_use]
    pub fn marks(&self, kind: MarkerKind) -> &[Frame] {
        mark_index(kind).map_or(&[], |index| &self.marks[index])
    }

    /// Returns the frame, pass, or transaction with id `id`.
    #[must_use]
    pub fn frame(&self, id: FrameId) -> Option<&Frame> {
        self.marks.iter().flatten().find(|frame| frame.id == id)
    }

    /// Returns the frame the program numbered `number`: a plain `u64` for
    /// the default frame stream, or a [`FrameNumber`].
    #[must_use]
    pub fn frame_numbered(&self, number: impl Into<FrameNumber>) -> Option<&Frame> {
        self.numbered_mark(MarkerKind::Frame, number)
    }

    /// Returns the frame, pass, or transaction of `kind` the program
    /// numbered `number`.
    #[must_use]
    pub fn numbered_mark(
        &self,
        kind: MarkerKind,
        number: impl Into<FrameNumber>,
    ) -> Option<&Frame> {
        let index = mark_index(kind)?;
        let id = *self.numbered.get(&(index, number.into()))?;
        let marks = &self.marks[index];
        marks
            .binary_search_by_key(&id, |frame| frame.id)
            .ok()
            .map(|position| &marks[position])
            .or_else(|| marks.iter().find(|frame| frame.id == id))
    }

    /// Returns the frame that was in progress at `at`.
    #[must_use]
    pub fn frame_at(&self, at: Duration) -> Option<&Frame> {
        let frames = self.frames();
        let after = frames.partition_point(|frame| frame.begin <= at);
        frames[..after]
            .iter()
            .rev()
            .find(|frame| frame.range().contains(at))
    }

    /// Returns the frames that began in `range`, by begin time.
    #[must_use]
    pub fn frames_in(&self, range: TimeRange) -> &[Frame] {
        let frames = self.frames();
        let first = frames.partition_point(|frame| frame.begin < range.start());
        let last = frames.partition_point(|frame| frame.begin < range.end());
        &frames[first..last]
    }

    /// Returns the context with id `context_id`, kept even after the
    /// producer retired it so every span keeps its attribution.
    #[must_use]
    pub fn context(&self, context_id: ContextId) -> Option<&OwnedContextSnapshot> {
        self.contexts.get(&context_id)
    }

    /// Returns the value of the label `key` in the context `context_id`.
    #[must_use]
    pub fn label(&self, context_id: ContextId, key: &str) -> Option<&OwnedLabelValue> {
        self.context(context_id)?
            .entries()
            .iter()
            .find(|entry| entry.key() == key)
            .map(|entry| entry.value())
    }

    /// Describes the context `context_id` by its labels, such as
    /// `pass=main view=3`; the root context and unknown ones by their id.
    #[must_use]
    pub fn describe_context(&self, context_id: ContextId) -> String {
        match self.context(context_id) {
            Some(context) if !context.entries().is_empty() => context
                .entries()
                .iter()
                .map(|entry| format!("{}={}", entry.key(), entry.value()))
                .collect::<Vec<_>>()
                .join(" "),
            Some(_) if context_id.get() == 0 => String::from("root"),
            _ => format!("context {}", context_id.get()),
        }
    }

    /// Returns every counter, gauge, and histogram total of the latest
    /// snapshot.
    #[must_use]
    pub fn counters(&self) -> &[OwnedCounterSnapshot] {
        &self.counters
    }

    /// Returns the value distributions of the latest snapshot.
    #[must_use]
    pub fn histograms(&self) -> &[OwnedHistogramSnapshot] {
        &self.histograms
    }

    /// Returns the sampled counters and gauges, one series per counter and
    /// context.
    pub fn counter_series(&self) -> impl Iterator<Item = &CounterSeries> {
        self.series.values()
    }

    /// Returns the messages, by time.
    #[must_use]
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Returns the messages recorded in `range`, by time.
    #[must_use]
    pub fn messages_in(&self, range: TimeRange) -> &[Message] {
        let first = self
            .messages
            .partition_point(|message| message.at < range.start());
        let last = self
            .messages
            .partition_point(|message| message.at < range.end());
        &self.messages[first..last]
    }

    /// Returns every resource the trace saw, in order of first observation.
    #[must_use]
    pub fn resources(&self) -> &[Resource] {
        &self.resources
    }

    /// Returns the resources alive at `at`.
    pub fn resources_alive_at(&self, at: Duration) -> impl Iterator<Item = &Resource> {
        self.resources
            .iter()
            .filter(move |resource| resource.alive_at(at))
    }

    /// Returns the flows, by id.
    pub fn flows(&self) -> impl Iterator<Item = &Flow> {
        self.flows.values()
    }

    /// Returns the flow with id `id`.
    #[must_use]
    pub fn flow(&self, id: FlowId) -> Option<&Flow> {
        self.flows.get(&id)
    }

    /// Returns the flows with a step inside the span `span`: the causal
    /// links into and out of that work.
    pub fn flows_through(&self, span: SpanId) -> impl Iterator<Item = &Flow> {
        self.flows
            .values()
            .filter(move |flow| flow.steps.iter().any(|step| step.span == Some(span)))
    }
}
