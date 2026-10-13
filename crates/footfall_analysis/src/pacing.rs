// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Frame pacing: whether each frame was shown when it was meant to be, and
//! what its work looked like.

use crate::frame::FrameId;
use crate::query::DurationStats;
use crate::span::TrackKind;
use crate::time::{TimeRange, nanos};
use crate::trace::Trace;
use core::time::Duration;
use footfall_core::{DropReason, FramePointKind, TimelineId, TimelineKind};
use std::collections::BTreeSet;

/// One numbered frame's pacing: its CPU work, its GPU work, its deadline,
/// the times it was predicted, meant, and expected to be shown, when it was
/// shown, and what that adds up to, as [`Pacing::frames`] returns it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FramePacing {
    number: u64,
    frame: Option<FrameId>,
    work: Option<TimeRange>,
    gpu: Option<TimeRange>,
    gpu_busy: Option<Duration>,
    deadline: Option<Duration>,
    predicted: Option<Duration>,
    target: Option<Duration>,
    expected: Option<Duration>,
    presented: Option<Duration>,
    dropped: Option<DropReason>,
    outcome: Outcome,
}

/// Whether a frame was shown when it was meant to be, as
/// [`FramePacing::outcome`] says.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum Outcome {
    /// Shown at its target, within the tolerance either way.
    OnTime,
    /// Shown more than the tolerance before its target: on screen sooner
    /// than the time it was made for, such as an animation drawn for its
    /// target and shown a refresh early.
    Early {
        /// How much sooner than its target.
        by: Duration,
    },
    /// Shown more than the tolerance after its target.
    Late {
        /// How much later than its target.
        by: Duration,
        /// What made it late, as far as the trace can tell.
        cause: LateCause,
    },
    /// Never shown. Only some reasons are misses; see
    /// [`DropReason::is_miss`].
    Dropped {
        /// Why.
        reason: DropReason,
    },
    /// Its work was submitted after its deadline, and when it was shown is
    /// not known; it was probably late.
    SubmittedLate {
        /// How long after the deadline.
        by: Duration,
    },
    /// Not known: there is no presentation time and target to compare,
    /// and the work did not miss its deadline.
    Unknown,
}

impl Outcome {
    /// Returns `true` for a frame shown late, submitted late, or dropped for
    /// a reason that is a miss, such as missing its deadline. A frame the
    /// program discarded or superseded, or that a later frame replaced, is
    /// not a miss, and neither is one shown early.
    #[must_use]
    pub const fn missed(self) -> bool {
        match self {
            Self::Late { .. } | Self::SubmittedLate { .. } => true,
            Self::Dropped { reason } => reason.is_miss(),
            _ => false,
        }
    }
}

/// What made a frame late, as [`Outcome::Late`] says: the first of these the
/// trace shows, in this order.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum LateCause {
    /// Its CPU work was submitted after its deadline.
    MissedDeadline,
    /// It was submitted in time, but the frame before it was still waiting
    /// to be shown at this one's target: frames queued ahead pushed it back.
    Backlog,
    /// It was submitted in time, but its GPU work finished after its target.
    Gpu,
    /// It was submitted and rendered in time, with nothing queued ahead of
    /// it, and shown late anyway: the compositor or display held it.
    Display,
    /// It was submitted in time with nothing queued ahead of it, but no GPU
    /// work was recorded for it, so whether the GPU or the display held it
    /// is not known.
    AfterSubmit,
    /// Not known: the trace has no deadline or CPU work for the frame.
    Unknown,
}

impl FramePacing {
    /// Returns the program's number for the frame.
    #[must_use]
    pub const fn number(&self) -> u64 {
        self.number
    }

    /// Returns the frame its numbered markers delimit, if the trace holds
    /// it.
    #[must_use]
    pub const fn frame(&self) -> Option<FrameId> {
        self.frame
    }

    /// Returns the time between the frame's markers: its CPU work, and any
    /// waiting the program did between them, such as for a drawable.
    #[must_use]
    pub const fn work(&self) -> Option<TimeRange> {
        self.work
    }

    /// Returns when the frame's GPU work ran: from the first of its spans
    /// on GPU timelines to begin to the last to end.
    #[must_use]
    pub const fn gpu(&self) -> Option<TimeRange> {
        self.gpu
    }

    /// Returns how long the GPU was busy with the frame: on each GPU
    /// timeline, the time covered by at least one of the frame's spans, so
    /// nested and overlapping spans count once, summed over the timelines.
    /// Two queues busy at once for 1 ms count 2 ms.
    ///
    /// Returns `None` when no GPU work was recorded for the frame, which is
    /// not the same as none being done.
    #[must_use]
    pub const fn gpu_busy(&self) -> Option<Duration> {
        self.gpu_busy
    }

    /// Returns the frame's deadline, the latest time its work could be
    /// submitted and still be shown at its target.
    #[must_use]
    pub const fn deadline(&self) -> Option<Duration> {
        self.deadline
    }

    /// Returns when the platform predicted, as the frame began, that it
    /// would be shown.
    #[must_use]
    pub const fn predicted(&self) -> Option<Duration> {
        self.predicted
    }

    /// Returns when the frame was meant to be shown.
    #[must_use]
    pub const fn target(&self) -> Option<Duration> {
        self.target
    }

    /// Returns when the frame was expected to be shown, as of submitting it.
    #[must_use]
    pub const fn expected(&self) -> Option<Duration> {
        self.expected
    }

    /// Returns when the frame was shown.
    #[must_use]
    pub const fn presented(&self) -> Option<Duration> {
        self.presented
    }

    /// Returns why the frame was never shown, when it was reported dropped.
    #[must_use]
    pub const fn dropped(&self) -> Option<DropReason> {
        self.dropped
    }

    /// Returns the time of the frame's point of `kind`: its
    /// [`Predicted`](FramePointKind::Predicted),
    /// [`Target`](FramePointKind::Target),
    /// [`Expected`](FramePointKind::Expected),
    /// [`Deadline`](FramePointKind::Deadline), or
    /// [`Presented`](FramePointKind::Presented) time.
    #[must_use]
    pub const fn time_of(&self, kind: FramePointKind) -> Option<Duration> {
        match kind {
            FramePointKind::Deadline => self.deadline,
            FramePointKind::Predicted => self.predicted,
            FramePointKind::Target => self.target,
            FramePointKind::Expected => self.expected,
            FramePointKind::Presented => self.presented,
            _ => None,
        }
    }

    /// Returns whether the frame was shown when it was meant to be.
    #[must_use]
    pub const fn outcome(&self) -> Outcome {
        self.outcome
    }

    /// Returns the time from the frame's begin marker to its presentation.
    ///
    /// This is how long the frame took to reach the screen once the program
    /// started it, not how long a person waited for a response: input that
    /// arrived before the frame began waited longer.
    #[must_use]
    pub fn latency(&self) -> Option<Duration> {
        Some(self.presented?.saturating_sub(self.work?.start()))
    }
}

/// Every numbered frame's pacing, with a summary, as
/// [`Trace::pacing`](crate::Trace::pacing) returns it.
#[derive(Clone, Debug, PartialEq)]
pub struct Pacing {
    frames: Vec<FramePacing>,
    tolerance: Duration,
    unplaced_points: u64,
    intervals: Vec<Duration>,
    interval_breaks: u64,
}

impl Pacing {
    /// Returns each numbered frame's pacing, by number.
    #[must_use]
    pub fn frames(&self) -> &[FramePacing] {
        &self.frames
    }

    /// Returns the pacing of the frame numbered `number`.
    #[must_use]
    pub fn frame(&self, number: u64) -> Option<&FramePacing> {
        self.frames
            .binary_search_by_key(&number, FramePacing::number)
            .ok()
            .map(|index| &self.frames[index])
    }

    /// Returns how far from its target a frame could be shown, either way,
    /// and still count as on time.
    #[must_use]
    pub const fn tolerance(&self) -> Duration {
        self.tolerance
    }

    /// Returns how many frames had each outcome: on time, early, late (and
    /// why), dropped, skipped, submitted late, and unknown.
    #[must_use]
    pub fn counts(&self) -> OutcomeCounts {
        let mut counts = OutcomeCounts::default();
        for frame in &self.frames {
            match frame.outcome {
                Outcome::OnTime => counts.on_time += 1,
                Outcome::Early { .. } => counts.early += 1,
                Outcome::Late { cause, .. } => {
                    counts.late += 1;
                    let causes = &mut counts.late_causes;
                    match cause {
                        LateCause::MissedDeadline => causes.missed_deadline += 1,
                        LateCause::Backlog => causes.backlog += 1,
                        LateCause::Gpu => causes.gpu += 1,
                        LateCause::Display => causes.display += 1,
                        LateCause::AfterSubmit => causes.after_submit += 1,
                        LateCause::Unknown => causes.unknown += 1,
                    }
                }
                Outcome::Dropped { reason } if reason.is_miss() => counts.dropped += 1,
                Outcome::Dropped { .. } => counts.skipped += 1,
                Outcome::SubmittedLate { .. } => counts.submitted_late += 1,
                Outcome::Unknown => counts.unknown += 1,
            }
        }
        counts
    }

    /// Returns how far off the frames' times of `kind` were from when they
    /// were shown: for [`Predicted`](FramePointKind::Predicted),
    /// [`Target`](FramePointKind::Target), or
    /// [`Expected`](FramePointKind::Expected), how good each prediction was.
    /// Only frames with both times count, and a frame shown within the
    /// [`tolerance`](Self::tolerance) of the time counts as neither early
    /// nor late.
    #[must_use]
    pub fn prediction_error(&self, kind: FramePointKind) -> PredictionError {
        let mut off = Vec::new();
        let mut early = Vec::new();
        let mut late = Vec::new();
        for frame in &self.frames {
            let (Some(predicted), Some(presented)) = (frame.time_of(kind), frame.presented) else {
                continue;
            };
            let error = presented.abs_diff(predicted);
            off.push(error);
            if error > self.tolerance {
                if presented > predicted {
                    late.push(error);
                } else {
                    early.push(error);
                }
            }
        }
        PredictionError {
            off: DurationStats::of(off),
            early: DurationStats::of(early),
            late: DurationStats::of(late),
        }
    }

    /// Returns the work spent on frames that were never shown, for any
    /// reason: CPU work between their markers, and GPU busy time.
    #[must_use]
    pub fn wasted(&self) -> WastedWork {
        let mut wasted = WastedWork::default();
        for frame in self.frames.iter().filter(|frame| frame.dropped.is_some()) {
            wasted.frames += 1;
            if let Some(work) = frame.work {
                wasted.cpu_frames += 1;
                wasted.cpu += work.length();
            }
            if let Some(busy) = frame.gpu_busy {
                wasted.gpu_frames += 1;
                wasted.gpu += busy;
            }
        }
        wasted
    }

    /// Returns the frames whose outcome satisfies `keep`, by their frame
    /// ids, such as the late ones to compare with the rest.
    pub fn frame_ids(&self, keep: impl Fn(Outcome) -> bool) -> Vec<FrameId> {
        self.frames
            .iter()
            .filter(|frame| keep(frame.outcome))
            .filter_map(|frame| frame.frame)
            .collect()
    }

    /// Returns how much later than their targets the late frames were shown.
    #[must_use]
    pub fn lateness(&self) -> DurationStats {
        DurationStats::of(self.frames.iter().filter_map(|frame| match frame.outcome {
            Outcome::Late { by, .. } => Some(by),
            _ => None,
        }))
    }

    /// Returns how long each presented frame stayed on screen: the time
    /// from one presentation to the next, in frame order. It is steady at the
    /// refresh interval when pacing is smooth.
    ///
    /// A frame known never to have been shown, dropped or skipped, leaves
    /// the frame before it on screen, so the interval runs on to the next
    /// presentation and shows the stall. Where the trace cannot say what was
    /// on screen, the interval is left out rather than guessed; see
    /// [`interval_breaks`](Self::interval_breaks).
    #[must_use]
    pub fn intervals(&self) -> DurationStats {
        DurationStats::of(self.intervals.iter().copied())
    }

    /// Returns how many times the run of presentations broke because the
    /// trace could not say what was on screen: a frame with neither a
    /// presentation nor a drop reported, a frame number missing from the
    /// trace, or events lost between two presentations. The interval across
    /// each break is left out of [`intervals`](Self::intervals).
    #[must_use]
    pub const fn interval_breaks(&self) -> u64 {
        self.interval_breaks
    }

    fn intervals_iter(&self) -> impl Iterator<Item = Duration> + '_ {
        self.intervals.iter().copied()
    }

    /// Returns how many presentation intervals were at least half again as
    /// long as the median: a frame left on screen for an extra refresh,
    /// which reads as a stutter.
    #[must_use]
    pub fn stutters(&self) -> usize {
        let median = self.intervals().median();
        if median.is_zero() {
            return 0;
        }
        self.intervals_iter()
            .filter(|interval| *interval * 2 >= median * 3)
            .count()
    }

    /// Returns the time from each frame's begin marker to its presentation;
    /// see [`FramePacing::latency`].
    #[must_use]
    pub fn latency(&self) -> DurationStats {
        DurationStats::of(self.frames.iter().filter_map(FramePacing::latency))
    }

    /// Returns how many frame points could not be converted to the trace's
    /// time, because their clock has no sync or pair leading to the
    /// runtime's, and so were left out.
    #[must_use]
    pub const fn unplaced_points(&self) -> u64 {
        self.unplaced_points
    }
}

/// How many frames had each [`Outcome`], as [`Pacing::counts`] returns them.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OutcomeCounts {
    /// Frames shown at their targets.
    pub on_time: u64,
    /// Frames shown before their targets.
    pub early: u64,
    /// Frames shown after their targets.
    pub late: u64,
    /// The late frames, by what made them late.
    pub late_causes: LateCauseCounts,
    /// Frames never shown for a reason that is a miss.
    pub dropped: u64,
    /// Frames never shown for a reason that is not a miss: discarded,
    /// superseded, replaced, or for a display that was unavailable.
    pub skipped: u64,
    /// Frames submitted after their deadlines, with no presentation time.
    pub submitted_late: u64,
    /// Frames with too little to judge.
    pub unknown: u64,
}

impl OutcomeCounts {
    /// Returns how many frames were counted.
    #[must_use]
    pub const fn total(&self) -> u64 {
        self.on_time
            + self.early
            + self.late
            + self.dropped
            + self.skipped
            + self.submitted_late
            + self.unknown
    }

    /// Returns how many frames were missed: late, submitted late, or
    /// dropped for a reason that is a miss.
    #[must_use]
    pub const fn missed(&self) -> u64 {
        self.late + self.submitted_late + self.dropped
    }
}

/// How many late frames had each [`LateCause`], as
/// [`OutcomeCounts::late_causes`] holds them.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LateCauseCounts {
    /// Submitted after their deadlines.
    pub missed_deadline: u64,
    /// Pushed back by frames queued ahead.
    pub backlog: u64,
    /// Their GPU work finished after their targets.
    pub gpu: u64,
    /// Held by the compositor or display.
    pub display: u64,
    /// Submitted in time, with no GPU work to tell the GPU from the display.
    pub after_submit: u64,
    /// Not known.
    pub unknown: u64,
}

/// How far off a kind of predicted time was from when frames were shown, as
/// [`Pacing::prediction_error`] returns it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PredictionError {
    /// How far off, either way, for every frame with both times.
    pub off: DurationStats,
    /// For frames shown more than the tolerance before the predicted time,
    /// how much before.
    pub early: DurationStats,
    /// For frames shown more than the tolerance after the predicted time,
    /// how much after.
    pub late: DurationStats,
}

impl PredictionError {
    /// Returns how many frames were shown within the tolerance of the
    /// predicted time.
    #[must_use]
    pub const fn within(&self) -> u64 {
        self.off.count() - self.early.count() - self.late.count()
    }
}

/// Work spent on frames that were never shown, as [`Pacing::wasted`]
/// returns it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WastedWork {
    /// How many frames were never shown.
    pub frames: u64,
    /// How many of them the trace holds the markers of.
    pub cpu_frames: u64,
    /// The time between their markers, over the `cpu_frames` frames; see
    /// [`FramePacing::work`].
    pub cpu: Duration,
    /// How many of them had GPU work recorded.
    pub gpu_frames: u64,
    /// The time the GPU was busy with them, over the `gpu_frames` frames;
    /// see [`FramePacing::gpu_busy`].
    pub gpu: Duration,
}

/// What a numbered frame's points and work say, before its outcome is
/// judged against the frame before it.
struct Raw {
    number: u64,
    frame: Option<FrameId>,
    work: Option<TimeRange>,
    gpu: Option<TimeRange>,
    gpu_busy: Option<Duration>,
    deadline: Option<Duration>,
    predicted: Option<Duration>,
    target: Option<Duration>,
    expected: Option<Duration>,
    presented: Option<Duration>,
    dropped: Option<DropReason>,
}

pub(crate) fn pacing(trace: &Trace, tolerance: Duration) -> Pacing {
    let mut numbers: BTreeSet<u64> = trace.numbered_frames().collect();
    numbers.extend(trace.frame_points().keys().copied());
    let mut unplaced_points = 0_u64;
    let raws: Vec<Raw> = numbers
        .into_iter()
        .map(|number| {
            let frame = trace.frame_numbered(number);
            let mut raw = Raw {
                number,
                frame: frame.map(crate::Frame::id),
                work: frame.map(crate::Frame::range),
                gpu: None,
                gpu_busy: None,
                deadline: None,
                predicted: None,
                target: None,
                expected: None,
                presented: None,
                dropped: None,
            };
            for (kind, time, _) in trace.frame_points().get(&number).into_iter().flatten() {
                let at = match time {
                    Some(time) => match trace.clocks().to_runtime(time.clock_id(), time.ticks()) {
                        Some(converted) => Some(nanos(converted.nanos())),
                        None => {
                            unplaced_points += 1;
                            continue;
                        }
                    },
                    None => None,
                };
                let slot = match kind {
                    FramePointKind::Deadline => &mut raw.deadline,
                    FramePointKind::Predicted => &mut raw.predicted,
                    FramePointKind::Target => &mut raw.target,
                    FramePointKind::Expected => &mut raw.expected,
                    FramePointKind::Presented => &mut raw.presented,
                    FramePointKind::Dropped(reason) => {
                        raw.dropped = Some(*reason);
                        continue;
                    }
                    _ => continue,
                };
                *slot = at.or(*slot);
            }
            let mut gpu: Vec<(TimelineId, TimeRange)> = trace
                .frame_spans(number)
                .filter_map(|span| Some((span.timeline()?, span.range())))
                .filter(|(timeline, _)| {
                    trace.timeline(*timeline).is_some_and(|track| {
                        matches!(
                            track.kind(),
                            TrackKind::Timeline {
                                kind: TimelineKind::Gpu,
                                ..
                            }
                        )
                    })
                })
                .collect();
            raw.gpu = gpu
                .iter()
                .map(|(_, range)| *range)
                .reduce(|a, b| TimeRange::new(a.start().min(b.start()), a.end().max(b.end())));
            raw.gpu_busy = (!gpu.is_empty()).then(|| busy(&mut gpu));
            raw
        })
        .collect();
    let frames: Vec<FramePacing> = raws
        .iter()
        .enumerate()
        .map(|(index, raw)| {
            // The frame just before, if the trace holds it.
            let before = index
                .checked_sub(1)
                .map(|before| &raws[before])
                .filter(|before| before.number + 1 == raw.number);
            let outcome = outcome(raw, before, tolerance);
            FramePacing {
                number: raw.number,
                frame: raw.frame,
                work: raw.work,
                gpu: raw.gpu,
                gpu_busy: raw.gpu_busy,
                deadline: raw.deadline,
                predicted: raw.predicted,
                target: raw.target,
                expected: raw.expected,
                presented: raw.presented,
                dropped: raw.dropped,
                outcome,
            }
        })
        .collect();
    let gaps: Vec<TimeRange> = trace.gaps().iter().map(crate::Gap::range).collect();
    let (intervals, interval_breaks) = intervals(&frames, &gaps);
    Pacing {
        frames,
        tolerance,
        unplaced_points,
        intervals,
        interval_breaks,
    }
}

/// Returns how long each presented frame stayed on screen, and how many
/// times the trace could not say; see [`Pacing::intervals`].
fn intervals(frames: &[FramePacing], gaps: &[TimeRange]) -> (Vec<Duration>, u64) {
    let mut intervals = Vec::new();
    let mut breaks = 0;
    // The frame examined last, and when the frame on screen was shown.
    let mut before: Option<u64> = None;
    let mut shown: Option<Duration> = None;
    for frame in frames {
        if before.is_some_and(|number| frame.number != number + 1) && shown.take().is_some() {
            breaks += 1;
        }
        before = Some(frame.number);
        match (frame.presented, frame.dropped) {
            (Some(at), _) => {
                if let Some(since) = shown {
                    if gaps.iter().any(|gap| gap.overlaps(since, at)) {
                        breaks += 1;
                    } else {
                        intervals.push(at.saturating_sub(since));
                    }
                }
                shown = Some(at);
            }
            // Never shown: the frame before stays on screen.
            (None, Some(_)) => {}
            (None, None) => {
                if shown.take().is_some() {
                    breaks += 1;
                }
            }
        }
    }
    (intervals, breaks)
}

/// Returns the time covered by at least one of the ranges on each timeline,
/// summed over the timelines.
fn busy(spans: &mut [(TimelineId, TimeRange)]) -> Duration {
    spans.sort_by_key(|(timeline, range)| (*timeline, range.start()));
    let mut total = Duration::ZERO;
    let mut covering: Option<(TimelineId, Duration, Duration)> = None;
    for &(timeline, range) in spans.iter() {
        covering = match covering {
            Some((on, start, end)) if on == timeline && range.start() <= end => {
                Some((on, start, end.max(range.end())))
            }
            Some((_, start, end)) => {
                total += end - start;
                Some((timeline, range.start(), range.end()))
            }
            None => Some((timeline, range.start(), range.end())),
        };
    }
    if let Some((_, start, end)) = covering {
        total += end - start;
    }
    total
}

/// Judges a frame against its target and deadline, and a late one against
/// the frame before it, `before`.
fn outcome(raw: &Raw, before: Option<&Raw>, tolerance: Duration) -> Outcome {
    if let Some(reason) = raw.dropped {
        return Outcome::Dropped { reason };
    }
    let submitted_late = raw
        .deadline
        .zip(raw.work)
        .and_then(|(deadline, work)| work.end().checked_sub(deadline))
        .filter(|by| !by.is_zero());
    let Some((presented, target)) = raw.presented.zip(raw.target) else {
        return match submitted_late {
            Some(by) => Outcome::SubmittedLate { by },
            None => Outcome::Unknown,
        };
    };
    if presented.saturating_add(tolerance) < target {
        return Outcome::Early {
            by: target - presented,
        };
    }
    if presented <= target.saturating_add(tolerance) {
        return Outcome::OnTime;
    }
    let submitted_in_time =
        raw.deadline.is_some() && raw.work.is_some() && submitted_late.is_none();
    let cause = if submitted_late.is_some() {
        LateCause::MissedDeadline
    } else if before
        .and_then(|before| before.presented)
        .is_some_and(|shown| shown.saturating_add(tolerance) >= target)
    {
        LateCause::Backlog
    } else if raw.gpu.is_some_and(|gpu| gpu.end() > target) {
        LateCause::Gpu
    } else if !submitted_in_time {
        LateCause::Unknown
    } else if raw.gpu.is_some() {
        LateCause::Display
    } else {
        LateCause::AfterSubmit
    };
    Outcome::Late {
        by: presented - target,
        cause,
    }
}
