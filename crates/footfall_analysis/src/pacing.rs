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
use footfall_core::{
    DropReason, FrameNumber, FramePointKind, FrameStream, TimelineId, TimelineKind, TimingBasis,
};
use footfall_session::OwnedArgEntry;
use std::collections::BTreeSet;

/// One numbered frame's pacing: its CPU work, its GPU work, its deadline,
/// the times it was predicted, meant, and expected to be shown, when it was
/// shown, and what that adds up to, as [`Pacing::frames`] returns it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FramePacing {
    number: FrameNumber,
    frame: Option<FrameId>,
    work: Option<TimeRange>,
    gpu: Option<TimeRange>,
    gpu_busy: Option<Duration>,
    deadline: Option<Duration>,
    predicted: Option<Duration>,
    target: Option<Duration>,
    expected: Option<Duration>,
    presented: Option<Duration>,
    submitted: Option<Duration>,
    sampled: Option<Duration>,
    dropped: Option<DropReason>,
    unplaced_points: u64,
    /// The bases and args the points that had them came with.
    details: Vec<(FramePointKind, TimingBasis, Vec<OwnedArgEntry>)>,
    outcome: Outcome,
    why: Option<Why>,
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
///
/// It is the leading explanation, not proof. More than one condition can
/// hold for a frame, and some causes are inferred rather than observed;
/// [`FramePacing::why`] lists every condition that held, what was observed,
/// and how strongly the trace supports the cause.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum LateCause {
    /// Its CPU work was submitted after its deadline.
    MissedDeadline,
    /// It was submitted in time, but the frame before it was still waiting
    /// to be shown at this one's target: frames queued ahead pushed it back.
    /// The queue itself is not observed, so this is
    /// [`Support::Inferred`].
    Backlog,
    /// It was submitted in time, but its GPU work finished after its target.
    Gpu,
    /// It was submitted and its recorded GPU work finished in time, with
    /// nothing queued ahead of it, and it was shown late anyway: the
    /// compositor or display probably held it. Neither is observed, and GPU
    /// work that was not recorded may have run late, so this is
    /// [`Support::Unresolved`].
    Display,
    /// It was submitted in time with nothing queued ahead of it, but no GPU
    /// work was recorded for it, so whether the GPU or the display held it
    /// is not known.
    AfterSubmit,
    /// Not known: the trace has no deadline or CPU work for the frame.
    Unknown,
}

impl FramePacing {
    /// Returns the program's number for the frame, in its stream.
    #[must_use]
    pub const fn number(&self) -> FrameNumber {
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

    /// Returns how many of the frame's points could not be converted to the
    /// trace's time, because their clock has no sync or pair leading to the
    /// runtime's, and so were left out of its times.
    #[must_use]
    pub const fn unplaced_points(&self) -> u64 {
        self.unplaced_points
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
            FramePointKind::Submitted => self.submitted,
            FramePointKind::Sampled => self.sampled,
            _ => None,
        }
    }

    /// Returns when the frame was submitted to the compositor or swapchain,
    /// when that was reported. Judging the deadline uses it, or the frame's
    /// end marker without it.
    #[must_use]
    pub const fn submitted(&self) -> Option<Duration> {
        self.submitted
    }

    /// Returns the time the frame's content was made for, such as when its
    /// animations were sampled, when that was reported.
    #[must_use]
    pub const fn sampled(&self) -> Option<Duration> {
        self.sampled
    }

    /// Returns whether the frame's work was submitted by its deadline: by
    /// its [`Submitted`](FramePointKind::Submitted) point, or its end marker
    /// without one. `None` when the trace has no deadline or submission for
    /// it.
    ///
    /// The deadline is the frame's own, from the frame scheduler, so this
    /// holds whatever the refresh rate, and when it varies.
    #[must_use]
    pub fn met_deadline(&self) -> Option<bool> {
        let (deadline, submitted) = self.deadline.zip(self.submission())?;
        Some(submitted <= deadline)
    }

    /// Returns how long after its deadline the frame's work was submitted,
    /// when it was; see [`met_deadline`](Self::met_deadline).
    #[must_use]
    pub fn submitted_late(&self) -> Option<Duration> {
        let (deadline, submitted) = self.deadline.zip(self.submission())?;
        submitted.checked_sub(deadline).filter(|by| !by.is_zero())
    }

    /// The time judged against the deadline.
    fn submission(&self) -> Option<Duration> {
        self.submitted.or(self.work.map(|work| work.end()))
    }

    /// Returns how the recorder came by the frame's time of `kind`, such as
    /// whether its presentation time was observed or estimated.
    #[must_use]
    pub fn basis(&self, kind: FramePointKind) -> TimingBasis {
        self.details
            .iter()
            .find(|(held, ..)| *held == kind)
            .map_or(TimingBasis::Unknown, |(_, basis, _)| *basis)
    }

    /// Returns what the recorder knew about the frame at its point of
    /// `kind`, such as a scheduler's state when it planned the frame.
    #[must_use]
    pub fn args(&self, kind: FramePointKind) -> &[OwnedArgEntry] {
        self.details
            .iter()
            .find(|(held, ..)| *held == kind)
            .map_or(&[], |(.., args)| args)
    }

    /// Returns whether the frame was shown when it was meant to be.
    #[must_use]
    pub const fn outcome(&self) -> Outcome {
        self.outcome
    }

    /// Returns what the trace shows about why a late frame was late: every
    /// condition that held, what was observed, and how strongly the trace
    /// supports the cause. `None` unless the frame was
    /// [`Late`](Outcome::Late).
    #[must_use]
    pub const fn why(&self) -> Option<&Why> {
        self.why.as_ref()
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
    /// Each presentation interval, with the stream it belongs to.
    intervals: Vec<(FrameStream, Duration)>,
    interval_breaks: u64,
    gaps: Vec<TimeRange>,
}

impl Pacing {
    /// Returns each numbered frame's pacing, by stream, then by number.
    #[must_use]
    pub fn frames(&self) -> &[FramePacing] {
        &self.frames
    }

    /// Returns the pacing of the frame numbered `number`: a plain `u64` for
    /// the default frame stream, or a [`FrameNumber`].
    #[must_use]
    pub fn frame(&self, number: impl Into<FrameNumber>) -> Option<&FramePacing> {
        let number = number.into();
        self.frames
            .binary_search_by_key(&number, FramePacing::number)
            .ok()
            .map(|index| &self.frames[index])
    }

    /// Returns the frame streams the frames belong to, such as one per
    /// window, in order.
    #[must_use]
    pub fn streams(&self) -> Vec<FrameStream> {
        let mut streams: Vec<_> = self
            .frames
            .iter()
            .map(|frame| frame.number.stream())
            .collect();
        streams.dedup();
        streams
    }

    /// Returns the pacing of one frame stream's frames alone, such as one
    /// window's. The other answers pool every stream's frames; each frame is
    /// judged against the frame before it in its own stream either way.
    #[must_use]
    pub fn in_stream(&self, stream: FrameStream) -> Self {
        let frames: Vec<FramePacing> = self
            .frames
            .iter()
            .filter(|frame| frame.number.stream() == stream)
            .cloned()
            .collect();
        let (intervals, interval_breaks) = intervals(&frames, &self.gaps);
        Self {
            frames,
            tolerance: self.tolerance,
            intervals,
            interval_breaks,
            gaps: self.gaps.clone(),
        }
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
                    match frame.why.map(|why| why.support) {
                        Some(Support::Observed) => counts.late_support.observed += 1,
                        Some(Support::Inferred) => counts.late_support.inferred += 1,
                        _ => counts.late_support.unresolved += 1,
                    }
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

    /// Returns how many of the shown frames' presentation times were
    /// observed, such as reported by the compositor, how many estimated,
    /// and how many came without saying. Judgments on estimated times are
    /// only as good as the estimate.
    #[must_use]
    pub fn presentation_basis(&self) -> BasisCounts {
        let mut counts = BasisCounts::default();
        for frame in self.frames.iter().filter(|frame| frame.presented.is_some()) {
            match frame.basis(FramePointKind::Presented) {
                TimingBasis::Observed => counts.observed += 1,
                TimingBasis::Estimated => counts.estimated += 1,
                _ => counts.unknown += 1,
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

    /// Returns how many frames had their work submitted by their deadlines
    /// and how many after, whether or not they were shown; see
    /// [`FramePacing::met_deadline`]. Frames with no deadline or submission
    /// count as neither.
    #[must_use]
    pub fn deadlines(&self) -> DeadlineCounts {
        let mut counts = DeadlineCounts::default();
        for frame in &self.frames {
            match frame.met_deadline() {
                Some(true) => counts.met += 1,
                Some(false) => counts.missed += 1,
                None => {}
            }
        }
        counts
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
    /// The intervals are pooled over every stream. Windows shown at
    /// different rates mix here; [`in_stream`](Self::in_stream) gives one
    /// window's cadence.
    ///
    /// A frame known never to have been shown, dropped or skipped, leaves
    /// the frame before it on screen, so the interval runs on to the next
    /// presentation and shows the stall. Where the trace cannot say what was
    /// on screen, the interval is left out rather than guessed; see
    /// [`interval_breaks`](Self::interval_breaks).
    #[must_use]
    pub fn intervals(&self) -> DurationStats {
        DurationStats::of(self.intervals.iter().map(|(_, interval)| *interval))
    }

    /// Returns how many times the run of presentations broke because the
    /// trace could not say what was on screen: a frame with neither a
    /// presentation nor a drop reported, a frame number missing from the
    /// trace, or events lost between two presentations. The interval across
    /// each break is left out of [`intervals`](Self::intervals). Frames
    /// still waiting for their reports when the trace ends are not a break:
    /// no presentation after them is left out.
    #[must_use]
    pub const fn interval_breaks(&self) -> u64 {
        self.interval_breaks
    }

    /// Returns how many presentation intervals were at least half again as
    /// long as the median interval of their own stream: a frame left on
    /// screen for an extra refresh, which reads as a stutter.
    ///
    /// It is a heuristic for long intervals, not a measure of what a person
    /// saw. Each stream is held to its own median, so windows shown at
    /// different rates do not make each other's intervals look long, but a
    /// deliberate change of rate within a stream, such as a variable
    /// refresh rate slowing down, counts too.
    #[must_use]
    pub fn stutters(&self) -> usize {
        let mut count = 0;
        // The intervals are in frame order, so each stream's are together.
        for run in self.intervals.chunk_by(|a, b| a.0 == b.0) {
            let median = DurationStats::of(run.iter().map(|(_, interval)| *interval)).median();
            if median.is_zero() {
                continue;
            }
            count += run
                .iter()
                .filter(|(_, interval)| *interval * 2 >= median * 3)
                .count();
        }
        count
    }

    /// Returns the time from each frame's begin marker to its presentation;
    /// see [`FramePacing::latency`].
    #[must_use]
    pub fn latency(&self) -> DurationStats {
        DurationStats::of(self.frames.iter().filter_map(FramePacing::latency))
    }

    /// Returns how many of the frames' points could not be converted to the
    /// trace's time, because their clock has no sync or pair leading to the
    /// runtime's, and so were left out; see
    /// [`FramePacing::unplaced_points`].
    #[must_use]
    pub fn unplaced_points(&self) -> u64 {
        self.frames.iter().map(FramePacing::unplaced_points).sum()
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
    /// The late frames, by how strongly the trace supports their causes.
    pub late_support: SupportCounts,
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

/// How many frames were submitted by their deadlines, as
/// [`Pacing::deadlines`] returns them.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DeadlineCounts {
    /// Submitted by their deadlines.
    pub met: u64,
    /// Submitted after their deadlines.
    pub missed: u64,
}

impl DeadlineCounts {
    /// Returns how many frames had a deadline and a submission to judge.
    #[must_use]
    pub const fn judged(&self) -> u64 {
        self.met + self.missed
    }
}

/// How many times had each [`TimingBasis`], as
/// [`Pacing::presentation_basis`] returns them.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BasisCounts {
    /// Observed as they happened.
    pub observed: u64,
    /// Worked out rather than observed.
    pub estimated: u64,
    /// Recorded without saying.
    pub unknown: u64,
}

/// How strongly the trace supports a judgment, as [`Why::support`] says.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum Support {
    /// The trace recorded the condition itself: the frame was submitted
    /// after its deadline, or its recorded GPU work finished after its
    /// target, and its presentation time was not an estimate.
    Observed,
    /// It follows from what was recorded, but rests on something that was
    /// not: a queue read from the frame before's presentation, or a
    /// presentation time that was estimated.
    Inferred,
    /// The trace stops short of an answer: the program's part looked in
    /// time, and what happened after it was not observed.
    Unresolved,
}

/// What the trace shows about why a late frame was late, as
/// [`FramePacing::why`] returns it.
///
/// [`cause`](Self::cause) is the first condition that held, but more than
/// one can: a frame can be submitted late and have slow GPU work too. The
/// conditions say each that held, and the stages say what was observed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Why {
    /// The leading explanation: the first condition that held.
    pub cause: LateCause,
    /// How strongly the trace supports it.
    pub support: Support,
    /// How long after its deadline the frame was submitted, when it was.
    pub submitted_late: Option<Duration>,
    /// Whether the submission time came from a
    /// [`Submitted`](FramePointKind::Submitted) point rather than the frame's
    /// end marker.
    pub submit_reported: bool,
    /// How long after its target its recorded GPU work finished, when it
    /// did.
    pub gpu_late: Option<Duration>,
    /// Whether GPU work was recorded for the frame at all.
    pub gpu_recorded: bool,
    /// Whether the frame before it was still waiting to be shown at this
    /// one's target.
    pub behind_previous: bool,
    /// How the frame's presentation time was known.
    pub presented: TimingBasis,
}

/// How many late frames' causes had each [`Support`], as
/// [`OutcomeCounts::late_support`] holds them.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SupportCounts {
    /// Their causes were observed.
    pub observed: u64,
    /// Their causes were inferred.
    pub inferred: u64,
    /// The trace stops short of their causes.
    pub unresolved: u64,
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

pub(crate) fn pacing(trace: &Trace, tolerance: Duration) -> Pacing {
    let mut numbers: BTreeSet<FrameNumber> = trace.numbered_frames().collect();
    numbers.extend(trace.frame_points().keys().copied());
    let mut frames: Vec<FramePacing> = numbers
        .into_iter()
        .map(|number| {
            let frame = trace.frame_numbered(number);
            let mut raw = FramePacing {
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
                submitted: None,
                sampled: None,
                dropped: None,
                unplaced_points: 0,
                details: Vec::new(),
                outcome: Outcome::Unknown,
                why: None,
            };
            for point in trace.frame_points().get(&number).into_iter().flatten() {
                let (kind, time) = (&point.kind, &point.time);
                if point.basis != TimingBasis::Unknown || !point.args.is_empty() {
                    raw.details.retain(|(held, ..)| held != kind);
                    raw.details.push((*kind, point.basis, point.args.clone()));
                }
                let at = match time {
                    Some(time) => match trace.clocks().to_runtime(time.clock_id(), time.ticks()) {
                        Some(converted) => Some(nanos(converted.nanos())),
                        None => {
                            raw.unplaced_points += 1;
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
                    FramePointKind::Submitted => &mut raw.submitted,
                    FramePointKind::Sampled => &mut raw.sampled,
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
    for index in 0..frames.len() {
        // The frame just before in the same stream, if the trace holds it.
        let before = index
            .checked_sub(1)
            .map(|before| &frames[before])
            .filter(|before| frames[index].number.previous() == Some(before.number));
        let (judged, why) = outcome(&frames[index], before, tolerance);
        frames[index].outcome = judged;
        frames[index].why = why;
    }
    let gaps: Vec<TimeRange> = trace.gaps().iter().map(crate::Gap::range).collect();
    let (intervals, interval_breaks) = intervals(&frames, &gaps);
    Pacing {
        frames,
        tolerance,
        intervals,
        interval_breaks,
        gaps,
    }
}

/// Returns how long each presented frame stayed on screen, and how many
/// times the trace could not say; see [`Pacing::intervals`].
fn intervals(frames: &[FramePacing], gaps: &[TimeRange]) -> (Vec<(FrameStream, Duration)>, u64) {
    let mut intervals = Vec::new();
    let mut breaks = 0;
    // The frame examined last, and when the frame on screen was shown.
    let mut before: Option<FrameNumber> = None;
    let mut shown: Option<Duration> = None;
    // Whether what was on screen stopped being known. It is a break once a
    // later presentation shows an interval was left out: frames still in
    // flight when the trace ends leave nothing out.
    let mut broken = false;
    for frame in frames {
        if before.is_some_and(|number| number.stream() != frame.number.stream()) {
            // Another stream's frames, such as another window's.
            shown = None;
            broken = false;
        } else if before.is_some_and(|number| frame.number.previous() != Some(number))
            && shown.take().is_some()
        {
            broken = true;
        }
        before = Some(frame.number);
        match (frame.presented, frame.dropped) {
            (Some(at), _) => {
                if let Some(since) = shown {
                    if gaps.iter().any(|gap| gap.overlaps(since, at)) {
                        breaks += 1;
                    } else {
                        intervals.push((frame.number.stream(), at.saturating_sub(since)));
                    }
                } else if broken {
                    breaks += 1;
                }
                broken = false;
                shown = Some(at);
            }
            // Never shown: the frame before stays on screen.
            (None, Some(_)) => {}
            (None, None) => {
                if shown.take().is_some() {
                    broken = true;
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
fn outcome(
    raw: &FramePacing,
    before: Option<&FramePacing>,
    tolerance: Duration,
) -> (Outcome, Option<Why>) {
    if let Some(reason) = raw.dropped {
        return (Outcome::Dropped { reason }, None);
    }
    let submitted_late = raw.submitted_late();
    let Some((presented, target)) = raw.presented.zip(raw.target) else {
        let outcome = match submitted_late {
            Some(by) => Outcome::SubmittedLate { by },
            None => Outcome::Unknown,
        };
        return (outcome, None);
    };
    if presented.saturating_add(tolerance) < target {
        let by = target - presented;
        return (Outcome::Early { by }, None);
    }
    if presented <= target.saturating_add(tolerance) {
        return (Outcome::OnTime, None);
    }
    let submitted_in_time = raw.met_deadline() == Some(true);
    let behind_previous = before
        .and_then(|before| before.presented)
        .is_some_and(|shown| shown.saturating_add(tolerance) >= target);
    let gpu_late = raw
        .gpu
        .and_then(|gpu| gpu.end().checked_sub(target))
        .filter(|by| !by.is_zero());
    let (cause, support) = if submitted_late.is_some() {
        (LateCause::MissedDeadline, Support::Observed)
    } else if behind_previous {
        (LateCause::Backlog, Support::Inferred)
    } else if gpu_late.is_some() {
        (LateCause::Gpu, Support::Observed)
    } else if !submitted_in_time {
        (LateCause::Unknown, Support::Unresolved)
    } else if raw.gpu.is_some() {
        (LateCause::Display, Support::Unresolved)
    } else {
        (LateCause::AfterSubmit, Support::Unresolved)
    };
    let presented_basis = raw.basis(FramePointKind::Presented);
    // Lateness measured against an estimated presentation is an estimate.
    let support = if support == Support::Observed && presented_basis == TimingBasis::Estimated {
        Support::Inferred
    } else {
        support
    };
    let why = Why {
        cause,
        support,
        submitted_late,
        submit_reported: raw.submitted.is_some(),
        gpu_late,
        gpu_recorded: raw.gpu.is_some(),
        behind_previous,
        presented: presented_basis,
    };
    let late = Outcome::Late {
        by: presented - target,
        cause,
    };
    (late, Some(why))
}
