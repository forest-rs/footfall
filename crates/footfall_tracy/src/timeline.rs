// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Timelines and frames as Tracy GPU contexts, and frame points as a
//! lateness plot.
//!
//! Tracy draws a GPU context as its own row, timed by its own clock. Each
//! timeline and clock pair becomes one context, made on the first span that
//! can be placed. Footfall reports timeline spans once they are finished, but
//! Tracy builds a context's zone tree from the order of zone begins and
//! ends, and wants a context's timestamps to arrive in increasing order. So
//! spans are held until the end of the CPU frame they were reported in, then
//! sorted, nested by containment, and sent with their timestamps in order.
//!
//! A frame stream's frames are drawn the same way, in the frame scheduler's
//! clock, on two rows whose zones never overlap: each frame's CPU work,
//! between its numbered markers, as soon as it ends, and each shown frame's
//! time on screen, once the next is shown. A frame's latency and its slack
//! before its deadline are left to plots, since a frame's whole life
//! overlaps the next frames' and Tracy draws a row as one stack.

use footfall_core::{
    ClockId, ClockReading, DropReason, FrameNumber, FramePoint, FramePointKind, FrameStream,
    MarkerPhase, ScopeDesc, TickPeriod, TimelineId, TimelineSpan,
};
use hashbrown::{HashMap, HashSet};
use std::collections::BTreeMap;
use std::time::Instant;
use tracy_client::{Client, GpuContext, GpuContextType, GpuSpan};

/// How many spans a timeline holds before it sends them without waiting for
/// the end of the frame, for a program that reports many or never ends one.
const MAX_HELD: usize = 1_024;

/// How many frames' targets and presentation times are kept waiting for the
/// other to arrive.
const MAX_FRAMES_WAITING: usize = 1_024;

/// What a [`TracyBackend`](crate::TracyBackend) did with timeline spans, as
/// [`TracyBackend::timeline_counts`](crate::TracyBackend::timeline_counts)
/// returns.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TimelineCounts {
    /// Tracy GPU contexts made, one for each timeline and clock.
    pub contexts: u64,
    /// Spans sent to Tracy as GPU zones.
    pub sent: u64,
    /// Spans held until the end of the frame, not yet sent.
    pub held: u64,
    /// Spans left out because their clock could not be placed: it had no
    /// [`clock_sync`](footfall_core::clock_sync), and no
    /// [`clock_pair`](footfall_core::clock_pair) with a clock that had one.
    pub unplaced: u64,
    /// Spans left out because Tracy ran out of GPU contexts, of which a
    /// process gets 255.
    pub over_limit: u64,
    /// Spans sent with times moved later than they were, so that their
    /// context's times reach Tracy in order: they were reported after later
    /// work on the same timeline.
    pub moved: u64,
}

/// The latest reading of a clock synced with the backend's own.
#[derive(Clone, Copy, Debug)]
struct Sync {
    ticks: u64,
    period: TickPeriod,
    at: Instant,
}

/// Two clocks read at the same moment: `first` is placed through `second`.
#[derive(Clone, Copy, Debug)]
struct Pair {
    first_ticks: u64,
    first_period: TickPeriod,
    second: ClockId,
    second_ticks: u64,
    second_period: TickPeriod,
}

/// One timeline and clock, as a Tracy GPU context, with the spans it holds.
struct Context {
    gpu: GpuContext,
    clock: ClockId,
    held: Vec<Held>,
    /// The latest timestamp sent, which later ones may not precede.
    last: Option<u64>,
}

#[derive(Clone, Debug)]
struct Held {
    name: Name,
    begin: u64,
    end: u64,
}

/// What a held zone is called: a timeline span's scope, or a frame.
#[derive(Clone, Debug)]
enum Name {
    Scope(&'static ScopeDesc),
    Text(String),
}

/// A frame's times, from its numbered markers and frame points, until it
/// is shown or dropped.
#[derive(Clone, Copy, Debug, Default)]
struct PendingFrame {
    /// When its CPU work began and ended, by the backend's clock.
    begin: Option<Instant>,
    end: Option<Instant>,
    deadline: Option<u64>,
}

/// One frame stream's Tracy rows: the frames built and the frames on
/// screen, in the frame scheduler's clock.
#[derive(Default)]
struct StreamRows {
    /// The frame scheduler's clock, once a frame point names it.
    clock: Option<(ClockId, TickPeriod)>,
    built: Option<Context>,
    on_screen: Option<Context>,
    /// The frame on screen, and when it was shown.
    shown: Option<(FrameNumber, u64)>,
}

/// Every timeline's state, behind the backend's lock.
#[derive(Default)]
pub(crate) struct Timelines {
    syncs: HashMap<ClockId, Sync>,
    pairs: HashMap<ClockId, Pair>,
    contexts: HashMap<(TimelineId, ClockId), Context>,
    /// Timelines named in a Tracy message about their spans being left out,
    /// so each is named once.
    reported: HashSet<TimelineId>,
    counts: TimelineCounts,
    targets: BTreeMap<FrameNumber, ClockReading>,
    presented: BTreeMap<FrameNumber, ClockReading>,
    frames: BTreeMap<FrameNumber, PendingFrame>,
    streams: HashMap<FrameStream, StreamRows>,
}

/// What a frame point produced for Tracy.
pub(crate) enum PointOutcome {
    /// The frame was shown this many nanoseconds after its target; negative
    /// when early.
    Lateness(i128),
    /// The frame numbered this was dropped, for this reason.
    Dropped(FrameNumber, DropReason),
    /// A frame of this stream was shown this many nanoseconds after its
    /// work began.
    Latency(FrameStream, i128),
    /// A frame of this stream's work ended this many nanoseconds before its
    /// deadline; negative when after.
    Slack(FrameStream, i128),
}

impl Timelines {
    pub(crate) fn counts(&self) -> TimelineCounts {
        let mut counts = self.counts;
        counts.held = self
            .contexts
            .values()
            .map(|context| context.held.len() as u64)
            .sum();
        counts
    }

    /// Records a sync and re-syncs every context it places.
    pub(crate) fn clock_sync(&mut self, reading: ClockReading) {
        let id = reading.clock().id();
        self.syncs.insert(
            id,
            Sync {
                ticks: reading.ticks(),
                period: reading.period(),
                at: Instant::now(),
            },
        );
        self.resync(id);
    }

    /// Records a pair and re-syncs every context it places.
    pub(crate) fn clock_pair(&mut self, first: ClockReading, second: ClockReading) {
        let id = first.clock().id();
        self.pairs.insert(
            id,
            Pair {
                first_ticks: first.ticks(),
                first_period: first.period(),
                second: second.clock().id(),
                second_ticks: second.ticks(),
                second_period: second.period(),
            },
        );
        self.resync(id);
    }

    /// Tells Tracy what every context placed by `clock` reads now.
    fn resync(&self, clock: ClockId) {
        let Self {
            syncs,
            pairs,
            contexts,
            ..
        } = self;
        for context in contexts.values() {
            let placed_by = context.clock == clock
                || pairs
                    .get(&context.clock)
                    .is_some_and(|pair| pair.second == clock);
            if placed_by && let Some(now) = ticks_at(syncs, pairs, context.clock, Instant::now()) {
                context.gpu.sync_gpu_time(to_i64(now));
            }
        }
    }

    /// Holds `span` on its timeline's context, making the context if this is
    /// its first span. Returns a message for Tracy when the span had to be
    /// left out and its timeline has not been named in one yet.
    pub(crate) fn timeline_span(&mut self, client: &Client, span: &TimelineSpan) -> Option<String> {
        let timeline = span.timeline();
        let clock = span.clock().id();
        let key = (timeline.id(), clock);
        if !self.contexts.contains_key(&key) {
            let Some(now) = ticks_at(&self.syncs, &self.pairs, clock, Instant::now()) else {
                self.counts.unplaced += 1;
                return self.reported.insert(timeline.id()).then(|| {
                    format!(
                        "footfall: spans on timeline {} are left out until clock {} is synced",
                        timeline.name(),
                        span.clock().name(),
                    )
                });
            };
            let name = if self
                .contexts
                .keys()
                .any(|(other, _)| *other == timeline.id())
            {
                format!("{} ({})", timeline.name(), span.clock().name())
            } else {
                timeline.name().to_owned()
            };
            let Some(context) =
                new_context(client, &name, clock, now, span.period(), &mut self.counts)
            else {
                return self.reported.insert(timeline.id()).then(|| {
                    format!(
                        "footfall: spans on timeline {} are left out: Tracy allows 255 GPU contexts",
                        timeline.name(),
                    )
                });
            };
            self.contexts.insert(key, context);
        }
        let context = self
            .contexts
            .get_mut(&key)
            .expect("the context was just made");
        context.held.push(Held {
            name: Name::Scope(span.scope()),
            begin: span.begin(),
            end: span.end().max(span.begin()),
        });
        if context.held.len() >= MAX_HELD {
            context.send(&mut self.counts);
        }
        None
    }

    /// Sends every context's held spans.
    pub(crate) fn flush(&mut self) {
        for context in self.contexts.values_mut() {
            context.send(&mut self.counts);
        }
    }

    /// Records when the CPU work of the frame numbered `number` began or
    /// ended, by the backend's clock, and draws the work on its stream's
    /// built row when it ends, once the stream's clock is known.
    pub(crate) fn frame_marker(
        &mut self,
        client: &Client,
        number: FrameNumber,
        phase: MarkerPhase,
        at: Instant,
    ) {
        let frame = self.pending(number);
        match phase {
            MarkerPhase::Begin => {
                frame.begin = Some(at);
                return;
            }
            MarkerPhase::End => frame.end = Some(at),
        }
        let Some(begin) = frame.begin else {
            return;
        };
        let stream = number.stream();
        let Some((clock, period)) = self.streams.get(&stream).and_then(|rows| rows.clock) else {
            return;
        };
        let (Some(begin), Some(end)) = (
            ticks_at(&self.syncs, &self.pairs, clock, begin),
            ticks_at(&self.syncs, &self.pairs, clock, at),
        ) else {
            self.counts.unplaced += 1;
            return;
        };
        let rows = self.streams.entry(stream).or_default();
        if rows.built.is_none() {
            let name = format!("frames built, stream {stream}");
            rows.built = new_context(client, &name, clock, end, period, &mut self.counts);
        }
        if let Some(built) = &mut rows.built {
            built.held.push(Held {
                name: Name::Text(format!("frame {}", number.number())),
                begin,
                end: end.max(begin),
            });
            built.send(&mut self.counts);
        }
    }

    /// The times kept for the frame numbered `number`, forgetting the
    /// lowest-numbered frames beyond [`MAX_FRAMES_WAITING`].
    fn pending(&mut self, number: FrameNumber) -> &mut PendingFrame {
        if !self.frames.contains_key(&number) {
            while self.frames.len() >= MAX_FRAMES_WAITING {
                self.frames.pop_first();
            }
        }
        self.frames.entry(number).or_default()
    }

    /// Records a frame point, returning what Tracy should show for it. A
    /// frame's presentation draws the time on screen of the frame it
    /// replaced, and gives its latency and slack.
    pub(crate) fn frame_point(&mut self, client: &Client, point: &FramePoint) -> Vec<PointOutcome> {
        let number = point.number();
        if let Some(time) = point.time() {
            self.streams.entry(number.stream()).or_default().clock =
                Some((time.clock().id(), time.period()));
            if point.kind() == FramePointKind::Deadline {
                self.pending(number).deadline = Some(time.ticks());
            }
        }
        let mut outcomes = Vec::new();
        match (point.kind(), point.time()) {
            (FramePointKind::Dropped(reason), _) => {
                self.targets.remove(&number);
                self.presented.remove(&number);
                self.frames.remove(&number);
                outcomes.push(PointOutcome::Dropped(number, reason));
            }
            (FramePointKind::Target, Some(time)) => {
                if let Some(presented) = self.presented.remove(&number) {
                    outcomes.extend(lateness(time, presented));
                } else {
                    keep(&mut self.targets, number, time);
                }
            }
            (FramePointKind::Presented, Some(time)) => {
                outcomes.extend(self.shown(client, number, time));
                if let Some(target) = self.targets.remove(&number) {
                    outcomes.extend(lateness(target, time));
                } else {
                    keep(&mut self.presented, number, time);
                }
            }
            _ => {}
        }
        outcomes
    }

    /// Draws the time on screen of the frame that the frame numbered
    /// `number`, shown at `presented`, replaced, and returns the frame's
    /// latency and slack.
    fn shown(
        &mut self,
        client: &Client,
        number: FrameNumber,
        presented: ClockReading,
    ) -> Vec<PointOutcome> {
        let pending = self.frames.remove(&number).unwrap_or_default();
        let (clock, period, at) = (
            presented.clock().id(),
            presented.period(),
            presented.ticks(),
        );
        let work =
            |at: Option<Instant>| at.and_then(|at| ticks_at(&self.syncs, &self.pairs, clock, at));
        let stream = number.stream();
        let (latency, slack) = figures(
            work(pending.begin),
            work(pending.end),
            pending.deadline,
            at,
            period,
        );
        let mut outcomes = Vec::new();
        outcomes.extend(latency.map(|nanos| PointOutcome::Latency(stream, nanos)));
        outcomes.extend(slack.map(|nanos| PointOutcome::Slack(stream, nanos)));
        let rows = self.streams.entry(stream).or_default();
        if let Some((before, shown)) = rows.shown.filter(|(_, shown)| *shown <= at) {
            if rows.on_screen.is_none() {
                let name = format!("frames on screen, stream {stream}");
                rows.on_screen = new_context(client, &name, clock, at, period, &mut self.counts);
            }
            if let Some(on_screen) = &mut rows.on_screen {
                on_screen.held.push(Held {
                    name: Name::Text(format!("frame {}", before.number())),
                    begin: shown,
                    end: at,
                });
                on_screen.send(&mut self.counts);
            }
        }
        rows.shown = Some((number, at));
        outcomes
    }
}

/// A shown frame's latency, from when its work began to `presented`, and
/// its slack, from when its work ended to its deadline, in nanoseconds of a
/// clock with `period`.
fn figures(
    begin: Option<u64>,
    end: Option<u64>,
    deadline: Option<u64>,
    presented: u64,
    period: TickPeriod,
) -> (Option<i128>, Option<i128>) {
    let nanos = |from: u64, to: u64| {
        (i128::from(to) - i128::from(from)) * i128::from(period.nanos())
            / i128::from(period.ticks())
    };
    (
        begin.map(|begin| nanos(begin, presented)),
        end.zip(deadline)
            .map(|(end, deadline)| nanos(end, deadline)),
    )
}

/// Makes a GPU context named `name`, timed by `clock`, which reads `now`.
fn new_context(
    client: &Client,
    name: &str,
    clock: ClockId,
    now: u64,
    period: TickPeriod,
    counts: &mut TimelineCounts,
) -> Option<Context> {
    // Footfall does not know which GPU API a timeline's work ran on, so
    // Tracy's context type is left unset.
    let Ok(gpu) = client.clone().new_gpu_context(
        Some(name),
        GpuContextType::Invalid,
        to_i64(now),
        period_nanos(period),
    ) else {
        counts.over_limit += 1;
        return None;
    };
    counts.contexts += 1;
    Some(Context {
        gpu,
        clock,
        held: Vec::new(),
        last: None,
    })
}

impl Context {
    /// Sends the held spans as nested zones, with their timestamps in order.
    fn send(&mut self, counts: &mut TimelineCounts) {
        if self.held.is_empty() {
            return;
        }
        let held = core::mem::take(&mut self.held);
        let plan = plan(&held, self.last);
        // Each zone's query ids return to the context when its span drops,
        // so every span is kept until both its timestamps are sent.
        let mut spans: Vec<GpuSpan> = Vec::with_capacity(held.len());
        for step in &plan.steps {
            match *step {
                Step::Begin(index) => {
                    let (name, location) = match &held[plan.order[index]].name {
                        Name::Scope(scope) => (scope.name(), scope.location()),
                        Name::Text(name) => (name.as_str(), None),
                    };
                    let (function, file, line) = location
                        .map_or(("footfall_timeline", "footfall", 0), |location| {
                            (location.function(), location.file(), location.line())
                        });
                    match self.gpu.span_alloc(name, function, file, line) {
                        Ok(span) => spans.push(span),
                        // Unreachable: a context sends at most `MAX_HELD`
                        // spans at once, far fewer than the 32,767 Tracy
                        // lets wait for timestamps.
                        Err(_) => return,
                    }
                }
                Step::End(index) => spans[index].end_zone(),
            }
        }
        for upload in &plan.uploads {
            let span = &spans[upload.index];
            if upload.end {
                span.upload_timestamp_end(to_i64(upload.ticks));
            } else {
                span.upload_timestamp_start(to_i64(upload.ticks));
            }
        }
        self.last = plan.last.or(self.last);
        counts.sent += held.len() as u64;
        counts.moved += plan.moved;
    }
}

/// One step in sending a batch: begin or end the zone for a held span,
/// numbered in the order they begin.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Step {
    Begin(usize),
    End(usize),
}

/// One timestamp to send.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Upload {
    index: usize,
    end: bool,
    ticks: u64,
}

/// How to send a batch: the zone begins and ends that nest it, and the
/// timestamps in increasing order.
#[derive(Debug, Default, Eq, PartialEq)]
struct Plan {
    /// The held spans' indices, in the order their zones begin.
    order: Vec<usize>,
    steps: Vec<Step>,
    uploads: Vec<Upload>,
    /// The latest timestamp sent.
    last: Option<u64>,
    /// How many spans had a time moved later.
    moved: u64,
}

/// Plans sending `held`, sorted by begin and then longest first, so each
/// span nests in the latest open span that contains it. No timestamp is
/// sent before `after`, the context's latest.
fn plan(held: &[Held], after: Option<u64>) -> Plan {
    let mut order: Vec<usize> = (0..held.len()).collect();
    order.sort_by(|&a, &b| {
        held[a]
            .begin
            .cmp(&held[b].begin)
            .then(held[b].end.cmp(&held[a].end))
    });
    // Zones are numbered in the order they begin, which is `order`.
    let mut plan = Plan::default();
    let mut open: Vec<(usize, u64)> = Vec::new();
    let mut times: Vec<Upload> = Vec::with_capacity(held.len() * 2);
    for (index, &held_index) in order.iter().enumerate() {
        let span = &held[held_index];
        while let Some(&(open_index, end)) = open.last() {
            if span.end <= end {
                break;
            }
            plan.steps.push(Step::End(open_index));
            open.pop();
        }
        plan.steps.push(Step::Begin(index));
        open.push((index, span.end));
        times.push(Upload {
            index,
            end: false,
            ticks: span.begin,
        });
        times.push(Upload {
            index,
            end: true,
            ticks: span.end,
        });
    }
    while let Some((open_index, _)) = open.pop() {
        plan.steps.push(Step::End(open_index));
    }
    times.sort_by_key(|upload| upload.ticks);
    let mut floor = after;
    let mut moved = vec![false; held.len()];
    for mut upload in times {
        if let Some(floor) = floor
            && upload.ticks < floor
        {
            upload.ticks = floor;
            moved[upload.index] = true;
        }
        floor = Some(upload.ticks);
        plan.uploads.push(upload);
    }
    plan.last = floor.filter(|_| !plan.uploads.is_empty());
    plan.moved = moved.iter().filter(|moved| **moved).count() as u64;
    plan.order = order;
    plan
}

/// Returns what `clock` read, or reads, at `now`: from its own latest sync,
/// or from a pair with a clock that has one, before or after the sync.
fn ticks_at(
    syncs: &HashMap<ClockId, Sync>,
    pairs: &HashMap<ClockId, Pair>,
    clock: ClockId,
    now: Instant,
) -> Option<u64> {
    let elapsed = |sync: &Sync| match now.checked_duration_since(sync.at) {
        Some(after) => i128::try_from(after.as_nanos()).ok(),
        None => i128::try_from(sync.at.duration_since(now).as_nanos())
            .ok()
            .map(|before| -before),
    };
    if let Some(sync) = syncs.get(&clock) {
        return Some(advance(sync.ticks, sync.period, elapsed(sync)?));
    }
    let pair = pairs.get(&clock)?;
    let sync = syncs.get(&pair.second)?;
    // How far the second clock has come since the pair, in nanoseconds.
    let second_now = i128::from(advance(sync.ticks, sync.period, elapsed(sync)?));
    let since = (second_now - i128::from(pair.second_ticks))
        * i128::from(pair.second_period.nanos())
        / i128::from(pair.second_period.ticks());
    Some(advance(pair.first_ticks, pair.first_period, since))
}

/// Returns `ticks` moved on by `nanos` nanoseconds of a clock with `period`.
fn advance(ticks: u64, period: TickPeriod, nanos: i128) -> u64 {
    let delta = nanos * i128::from(period.ticks()) / i128::from(period.nanos().max(1));
    let ticks = i128::from(ticks) + delta;
    u64::try_from(ticks.max(0)).unwrap_or(u64::MAX)
}

/// Returns a tick period as Tracy takes it: nanoseconds per tick.
#[expect(
    clippy::cast_possible_truncation,
    reason = "Tracy takes the period as an f32"
)]
fn period_nanos(period: TickPeriod) -> f32 {
    (f64::from(period.nanos()) / f64::from(period.ticks())) as f32
}

/// Returns ticks as Tracy takes them, saturating.
fn to_i64(ticks: u64) -> i64 {
    i64::try_from(ticks).unwrap_or(i64::MAX)
}

/// Keeps a frame's time until the other arrives, forgetting the
/// lowest-numbered frames beyond [`MAX_FRAMES_WAITING`].
fn keep(
    waiting: &mut BTreeMap<FrameNumber, ClockReading>,
    number: FrameNumber,
    time: ClockReading,
) {
    waiting.insert(number, time);
    while waiting.len() > MAX_FRAMES_WAITING {
        waiting.pop_first();
    }
}

/// Returns how long after `target` a frame was `presented`, when both are
/// readings of the same clock.
fn lateness(target: ClockReading, presented: ClockReading) -> Option<PointOutcome> {
    if target.clock() != presented.clock() {
        return None;
    }
    let nanos = |reading: ClockReading| {
        i128::from(reading.ticks()) * i128::from(reading.period().nanos())
            / i128::from(reading.period().ticks())
    };
    Some(PointOutcome::Lateness(nanos(presented) - nanos(target)))
}

#[cfg(test)]
mod tests {
    use super::{Held, Name, Pair, Plan, Step, Sync, Upload, advance, figures, plan, ticks_at};
    use footfall_core::{ClockDesc, ScopeDesc, TickPeriod};
    use hashbrown::HashMap;
    use std::time::{Duration, Instant};

    static FRAME: ScopeDesc = ScopeDesc::new("frame");
    static PASS: ScopeDesc = ScopeDesc::new("pass");
    static GPU: ClockDesc = ClockDesc::new("gpu");
    static HOST: ClockDesc = ClockDesc::new("host");

    fn held(begin: u64, end: u64) -> Held {
        Held {
            name: Name::Scope(if end - begin > 50 { &FRAME } else { &PASS }),
            begin,
            end,
        }
    }

    fn uploads(plan: &Plan) -> Vec<(usize, bool, u64)> {
        plan.uploads
            .iter()
            .map(|Upload { index, end, ticks }| (*index, *end, *ticks))
            .collect()
    }

    #[test]
    fn spans_nest_by_containment_whatever_order_they_arrive_in() {
        // Two passes reported before the frame that contains them.
        let plan = plan(&[held(10, 30), held(30, 60), held(0, 100)], None);
        assert_eq!(plan.order, [2, 0, 1]);
        assert_eq!(
            plan.steps,
            [
                Step::Begin(0),
                Step::Begin(1),
                Step::End(1),
                Step::Begin(2),
                Step::End(2),
                Step::End(0),
            ]
        );
        assert_eq!(
            uploads(&plan),
            [
                (0, false, 0),
                (1, false, 10),
                (1, true, 30),
                (2, false, 30),
                (2, true, 60),
                (0, true, 100),
            ]
        );
        assert_eq!(plan.last, Some(100));
        assert_eq!(plan.moved, 0);
    }

    #[test]
    fn overlapping_spans_are_siblings_with_their_times_in_order() {
        let plan = plan(&[held(0, 20), held(10, 30)], None);
        assert_eq!(
            plan.steps,
            [Step::Begin(0), Step::End(0), Step::Begin(1), Step::End(1)]
        );
        assert_eq!(
            uploads(&plan),
            [(0, false, 0), (1, false, 10), (0, true, 20), (1, true, 30)]
        );
    }

    #[test]
    fn work_reported_after_later_work_is_moved_up_to_it() {
        let plan = plan(&[held(40, 45), held(90, 95)], Some(50));
        assert_eq!(
            uploads(&plan),
            [(0, false, 50), (0, true, 50), (1, false, 90), (1, true, 95)]
        );
        assert_eq!(plan.moved, 1);
        assert_eq!(plan.last, Some(95));
    }

    #[test]
    fn a_shown_frames_latency_runs_from_its_work_and_its_slack_to_its_deadline() {
        // Mach ticks: 125 ns per 3.
        let period = TickPeriod::new(125, 3);
        assert_eq!(
            figures(Some(240), Some(312), Some(360), 1_200, period),
            (Some(40_000), Some(2_000)),
            "960 ticks are 40 µs; 48 ticks are 2 µs"
        );
        assert_eq!(
            figures(None, Some(400), Some(360), 1_200, period),
            (None, Some(-1_666)),
            "work that ended after its deadline has negative slack"
        );
    }

    #[test]
    fn a_clock_reads_back_before_its_sync() {
        let at = Instant::now();
        let mut syncs = HashMap::new();
        syncs.insert(
            HOST.id(),
            Sync {
                ticks: 10_000,
                period: TickPeriod::NANOSECOND,
                at,
            },
        );
        let earlier = at
            .checked_sub(Duration::from_micros(4))
            .expect("an earlier instant");
        assert_eq!(
            ticks_at(&syncs, &HashMap::new(), HOST.id(), earlier),
            Some(6_000)
        );
    }

    #[test]
    fn a_synced_clock_reads_on_from_its_sync() {
        let at = Instant::now();
        let mut syncs = HashMap::new();
        // 24 MHz: 3 ticks every 125 ns.
        let period = TickPeriod::new(125, 3);
        syncs.insert(
            GPU.id(),
            Sync {
                ticks: 1_000,
                period,
                at,
            },
        );
        let pairs = HashMap::new();
        let later = at + Duration::from_micros(10);
        assert_eq!(ticks_at(&syncs, &pairs, GPU.id(), later), Some(1_240));
        assert_eq!(ticks_at(&syncs, &pairs, HOST.id(), later), None);
    }

    #[test]
    fn a_paired_clock_reads_on_through_the_synced_one() {
        let at = Instant::now();
        let mut syncs = HashMap::new();
        syncs.insert(
            HOST.id(),
            Sync {
                ticks: 5_000,
                period: TickPeriod::new(125, 3),
                at,
            },
        );
        let mut pairs = HashMap::new();
        // The GPU, in nanoseconds, read 1,000,000 when the host read 2,600:
        // 2,400 host ticks, 100 µs, before the host's sync.
        pairs.insert(
            GPU.id(),
            Pair {
                first_ticks: 1_000_000,
                first_period: TickPeriod::NANOSECOND,
                second: HOST.id(),
                second_ticks: 2_600,
                second_period: TickPeriod::new(125, 3),
            },
        );
        let later = at + Duration::from_micros(10);
        assert_eq!(ticks_at(&syncs, &pairs, GPU.id(), later), Some(1_110_000));
    }

    #[test]
    fn advancing_saturates_at_zero() {
        assert_eq!(advance(10, TickPeriod::NANOSECOND, -100), 0);
    }
}
