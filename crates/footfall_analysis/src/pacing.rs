// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Frame pacing: whether each frame was shown when it was meant to be, and
//! what its work looked like.

use crate::frame::FrameId;
use crate::query::DurationStats;
use crate::span::TrackKind;
use crate::time::{Shown, TimeRange, nanos};
use crate::trace::Trace;
use core::fmt;
use core::time::Duration;
use footfall_core::{
    BeginsAt, DropReason, FrameNumber, FramePointKind, FrameStream, TimelineId, TimelineKind,
    TimingBasis,
};
use footfall_session::OwnedArgEntry;
use std::collections::BTreeSet;

/// One numbered frame's pacing: when it was meant to begin and when it did,
/// its CPU work, its GPU work, its deadline, the times it was predicted,
/// meant, and expected to be shown, when it was shown, and what that adds
/// up to, as [`Pacing::frames`] returns it.
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
    scheduled: Option<(Duration, BeginsAt)>,
    began: Began,
    dropped: Option<DropReason>,
    unplaced_points: u64,
    /// The bases and args the points that had them came with.
    details: Vec<(FramePointKind, TimingBasis, Vec<OwnedArgEntry>)>,
    outcome: Outcome,
    why: Option<Why>,
    cadence: Option<Cadence>,
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

/// When a frame began, judged against when it could have; see
/// [`FramePacing::began_late`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Began {
    judged: bool,
    late: Option<Duration>,
    opportunity_late: Option<Duration>,
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
    /// It was to begin at a frame opportunity, and the platform skipped
    /// the one it was meant for, so it began at a later one: the
    /// opportunity it began at came at least half an interval after it was
    /// due. See [`FramePacing::opportunity_late`].
    SkippedOpportunity,
    /// It began more than the tolerance after it could have: after its
    /// scheduled time, such as a timer that woke late, or after the frame
    /// opportunity it was to begin at, such as a program busy elsewhere.
    /// See [`FramePacing::began_late`].
    BeganLate,
    /// It was submitted in time, but the frame before it was still waiting
    /// to be shown at this one's target: frames queued ahead pushed it back.
    /// The queue itself is not observed, so this is
    /// [`Support::Inferred`].
    Backlog,
    /// It was submitted in time, but its GPU work finished after its target.
    Gpu,
    /// It was shown a fraction of a refresh after its target, not a whole
    /// number of refreshes: no refresh came at its target, so the target
    /// itself was mispredicted, such as a refresh interval or phase that
    /// drifted. Judged only where the platform states its refresh
    /// interval with each opportunity, so this is [`Support::Inferred`].
    /// See [`Why::whole_refreshes`].
    Mispredicted,
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

/// Who a late frame's cause points at, as [`LateCause::party`] says.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum Party {
    /// The program's own work: its CPU work missed its deadline, its GPU
    /// work ran past its target, or it began later than it could have.
    Program,
    /// What happened outside the program: the platform skipped the
    /// opportunity the frame was meant for, mispredicted its target, or
    /// held it after it was submitted in time.
    Platform,
    /// Frames queued ahead of it: the late frame before it, whatever made
    /// that one late.
    Queue,
    /// Not known.
    Unknown,
}

impl LateCause {
    /// Returns who the cause points at: the program, the platform, frames
    /// queued ahead, or not known. Like the cause, it is the leading
    /// explanation, not proof; see [`Why::support`].
    #[must_use]
    pub const fn party(self) -> Party {
        match self {
            Self::MissedDeadline | Self::BeganLate | Self::Gpu => Party::Program,
            Self::SkippedOpportunity | Self::Mispredicted | Self::Display | Self::AfterSubmit => {
                Party::Platform
            }
            Self::Backlog => Party::Queue,
            Self::Unknown => Party::Unknown,
        }
    }
}

impl fmt::Display for Party {
    /// Says who in a word or two: `the program`, `the platform`, `the
    /// queue`, or `not known`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Program => "the program",
            Self::Platform => "the platform",
            Self::Queue => "the queue",
            Self::Unknown => "not known",
        })
    }
}

/// Whether a shown frame kept the rhythm its target set, as
/// [`FramePacing::cadence`] says: whether it was shown as long after the
/// frame shown before it, in its stream, as their targets were apart.
///
/// This is smoothness, apart from latency. A frame shown two refreshes
/// after its target, as the frame before it was, is late but steady: its
/// content moves as evenly as it was meant to, only later. A frame shown
/// on time after one shown late is on time, but came too soon after it.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum Cadence {
    /// Shown within the tolerance of its targets' spacing.
    Steady,
    /// Shown later after the frame before than their targets were apart:
    /// the frame before stayed on screen too long, a hitch.
    Stalled {
        /// How much longer than their targets' spacing.
        by: Duration,
    },
    /// Shown sooner after the frame before than their targets were apart:
    /// the frame before was cut short and the content jumped ahead, a
    /// hitch too.
    Hurried {
        /// How much shorter than their targets' spacing.
        by: Duration,
    },
}

impl Cadence {
    /// Returns `true` for a frame that broke the rhythm: stalled or
    /// hurried.
    #[must_use]
    pub const fn is_hitch(self) -> bool {
        !matches!(self, Self::Steady)
    }
}

impl fmt::Display for Cadence {
    /// Says the cadence in a few words, such as `stalled by 8.33 ms`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Steady => f.write_str("steady"),
            Self::Stalled { by } => write!(f, "stalled by {}", Shown(*by)),
            Self::Hurried { by } => write!(f, "hurried by {}", Shown(*by)),
        }
    }
}

impl fmt::Display for Outcome {
    /// Says the outcome in a few words, such as `late by 8.33 ms: the
    /// platform skipped its opportunity`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OnTime => f.write_str("on time"),
            Self::Early { by } => write!(f, "early by {}", Shown(*by)),
            Self::Late { by, cause } => write!(f, "late by {}: {cause}", Shown(*by)),
            Self::Dropped { reason } => write!(f, "dropped: {reason}"),
            Self::SubmittedLate { by } => {
                write!(f, "submitted {} after its deadline", Shown(*by))
            }
            Self::Unknown => f.write_str("not known"),
        }
    }
}

impl fmt::Display for LateCause {
    /// Says the cause in a few words, such as `queued behind a late frame`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::MissedDeadline => "submitted after its deadline",
            Self::SkippedOpportunity => "the platform skipped its opportunity",
            Self::BeganLate => "began late",
            Self::Backlog => "queued behind a late frame",
            Self::Gpu => "GPU finished after the target",
            Self::Mispredicted => "its target was mispredicted",
            Self::Display => "held by the display",
            Self::AfterSubmit => "late after submit, no GPU work to tell",
            Self::Unknown => "cause not known",
        })
    }
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

    /// Returns the earliest time the frame's work was meant to begin, from
    /// its [`Scheduled`](FramePointKind::Scheduled) point.
    #[must_use]
    pub fn scheduled(&self) -> Option<Duration> {
        self.scheduled.map(|(at, _)| at)
    }

    /// Returns whether the frame was to begin at its scheduled time or at
    /// the first frame opportunity from then on.
    #[must_use]
    pub fn begins_at(&self) -> Option<BeginsAt> {
        self.scheduled.map(|(_, begins_at)| begins_at)
    }

    /// Returns how long after it could have the frame's work began, when
    /// that was more than the tolerance: after its scheduled time, for a
    /// frame that was to begin then, or after the first frame opportunity
    /// of its stream from its scheduled time on, for one that was to begin
    /// at an opportunity. A frame that began at the opportunity it was
    /// meant for did not begin late, even if that opportunity came late;
    /// see [`opportunity_late`](Self::opportunity_late).
    #[must_use]
    pub const fn began_late(&self) -> Option<Duration> {
        self.began.late
    }

    /// Returns how much later than it was due the frame opportunity the
    /// frame was to begin at came, when the platform skipped at least one:
    /// when it came at least half an interval after the first time from
    /// the frame's scheduled time on that the opportunity before it said
    /// the next would come, its interval, or the stream's median spacing
    /// without one, apart.
    ///
    /// It needs only the frame's scheduled point and its stream's
    /// opportunities, not its begin marker, so it also says why a frame
    /// dropped before it began was dropped, such as one that expired
    /// waiting for an opportunity that never came.
    #[must_use]
    pub const fn opportunity_late(&self) -> Option<Duration> {
        self.began.opportunity_late
    }

    /// Returns whether the trace held what judging when the frame began
    /// needs: its scheduled point and begin marker, and, for a frame that
    /// was to begin at an opportunity, its stream's opportunities from its
    /// scheduled time on. Without them, [`began_late`](Self::began_late)
    /// says nothing; [`opportunity_late`](Self::opportunity_late) needs no
    /// begin marker.
    #[must_use]
    pub const fn begin_judged(&self) -> bool {
        self.began.judged
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
    /// [`Scheduled`](FramePointKind::Scheduled),
    /// [`Predicted`](FramePointKind::Predicted),
    /// [`Target`](FramePointKind::Target),
    /// [`Expected`](FramePointKind::Expected),
    /// [`Deadline`](FramePointKind::Deadline),
    /// [`Submitted`](FramePointKind::Submitted),
    /// [`Sampled`](FramePointKind::Sampled), or
    /// [`Presented`](FramePointKind::Presented) time. A scheduled time is
    /// returned whatever [`BeginsAt`] `kind` carries.
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
            FramePointKind::Scheduled(_) => match self.scheduled {
                Some((at, _)) => Some(at),
                None => None,
            },
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
    /// whether its presentation time was observed or estimated. A
    /// [`Scheduled`](FramePointKind::Scheduled) kind finds the frame's
    /// scheduled point whatever [`BeginsAt`] it carries.
    #[must_use]
    pub fn basis(&self, kind: FramePointKind) -> TimingBasis {
        self.details
            .iter()
            .find(|(held, ..)| same_kind(*held, kind))
            .map_or(TimingBasis::Unknown, |(_, basis, _)| *basis)
    }

    /// Returns what the recorder knew about the frame at its point of
    /// `kind`, such as a scheduler's state when it planned the frame. A
    /// [`Scheduled`](FramePointKind::Scheduled) kind finds the frame's
    /// scheduled point whatever [`BeginsAt`] it carries.
    #[must_use]
    pub fn args(&self, kind: FramePointKind) -> &[OwnedArgEntry] {
        self.details
            .iter()
            .find(|(held, ..)| same_kind(*held, kind))
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

    /// Returns whether the frame was shown as long after the frame shown
    /// before it, in its stream, as their targets were apart; see
    /// [`Cadence`].
    ///
    /// `None` when that is not known: the frame was not shown, it or the
    /// frame shown before it had no target, it is its stream's first shown
    /// frame, or the trace cannot say what was on screen between them,
    /// such as a frame with neither a presentation nor a drop reported, or
    /// events lost.
    #[must_use]
    pub const fn cadence(&self) -> Option<Cadence> {
        self.cadence
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
    opportunities: Vec<StreamOpportunities>,
}

/// One frame stream's opportunities, placed in the trace's time.
#[derive(Clone, Debug, PartialEq)]
struct StreamOpportunities {
    stream: FrameStream,
    /// Each opportunity's time, and the interval the platform meant to
    /// leave after it, in order of time.
    times: Vec<(Duration, Option<Duration>)>,
    /// The median time between them, for opportunities recorded without an
    /// interval.
    median: Duration,
    /// How many could not be placed, because their clock has no path to
    /// the runtime's.
    unplaced: u64,
}

impl StreamOpportunities {
    /// The interval the platform meant to leave after the opportunity at
    /// `index`.
    fn interval_after(&self, index: usize) -> Duration {
        self.times[index].1.unwrap_or(self.median)
    }

    /// The refresh interval the platform stated with the last opportunity
    /// at or before `at`, if it stated one.
    fn stated_interval_at(&self, at: Duration) -> Option<Duration> {
        let after = self.times.partition_point(|(time, _)| *time <= at);
        self.times[..after]
            .last()
            .and_then(|(_, interval)| *interval)
            .filter(|interval| !interval.is_zero())
    }
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
            opportunities: self
                .opportunities
                .iter()
                .filter(|opportunities| opportunities.stream == stream)
                .cloned()
                .collect(),
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
                    match frame.why.map(|why| (why.support, why.unstated_basis())) {
                        Some((Support::Observed, unstated)) => {
                            counts.late_support.observed += 1;
                            counts.late_support.observed_unstated += u64::from(unstated);
                        }
                        Some((Support::Inferred, _)) => counts.late_support.inferred += 1,
                        _ => counts.late_support.unresolved += 1,
                    }
                    let causes = &mut counts.late_causes;
                    match cause {
                        LateCause::MissedDeadline => causes.missed_deadline += 1,
                        LateCause::SkippedOpportunity => causes.skipped_opportunity += 1,
                        LateCause::BeganLate => causes.began_late += 1,
                        LateCause::Backlog => causes.backlog += 1,
                        LateCause::Gpu => causes.gpu += 1,
                        LateCause::Mispredicted => causes.mispredicted += 1,
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

    /// Returns how many frame opportunities the trace recorded, how many
    /// the platform skipped between them, and how many could not be
    /// placed.
    ///
    /// An opportunity skipped is one that did not come when the one
    /// before it said the next would: the wait to the next, divided by the
    /// interval it gave (or the stream's median spacing without one) and
    /// rounded, less one. Waits across events lost from the trace are not
    /// counted.
    #[must_use]
    pub fn opportunities(&self) -> OpportunityCounts {
        let mut counts = OpportunityCounts::default();
        for stream in &self.opportunities {
            counts.recorded += stream.times.len() as u64;
            counts.unplaced += stream.unplaced;
            for (index, pair) in stream.times.windows(2).enumerate() {
                let (from, to) = (pair[0].0, pair[1].0);
                let interval = stream.interval_after(index);
                if interval.is_zero() || self.gaps.iter().any(|gap| gap.overlaps(from, to)) {
                    continue;
                }
                let wait = (to - from).as_nanos();
                let intervals = (wait + interval.as_nanos() / 2) / interval.as_nanos();
                counts.skipped += u64::try_from(intervals.saturating_sub(1)).unwrap_or(u64::MAX);
            }
        }
        counts
    }

    /// Returns the time from each frame opportunity to the next in its
    /// stream. It is steady at the refresh interval while the platform
    /// offers every refresh.
    #[must_use]
    pub fn opportunity_intervals(&self) -> DurationStats {
        DurationStats::of(
            self.opportunities
                .iter()
                .flat_map(|stream| stream.times.windows(2).map(|pair| pair[1].0 - pair[0].0)),
        )
    }

    /// Returns how much later than they could have the frames that began
    /// late began, whether or not they were shown on time; see
    /// [`FramePacing::began_late`].
    #[must_use]
    pub fn begin_lateness(&self) -> DurationStats {
        DurationStats::of(self.frames.iter().filter_map(FramePacing::began_late))
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

    /// Returns how many shown frames kept the rhythm their targets set,
    /// and how many broke it; see [`FramePacing::cadence`].
    ///
    /// Unlike [`stutters`](Self::stutters), which holds each interval to
    /// the median, this holds each frame to its own target, so a cadence
    /// the program chose to change is not a hitch, and latency that holds
    /// steady is told apart from hitches: see
    /// [`CadenceCounts::late_but_steady`].
    #[must_use]
    pub fn cadence(&self) -> CadenceCounts {
        let mut counts = CadenceCounts::default();
        for frame in &self.frames {
            match frame.cadence {
                Some(Cadence::Steady) => {
                    counts.steady += 1;
                    if matches!(frame.outcome, Outcome::Late { .. }) {
                        counts.late_but_steady += 1;
                    }
                }
                Some(Cadence::Stalled { .. }) => counts.stalled += 1,
                Some(Cadence::Hurried { .. }) => counts.hurried += 1,
                None => {}
            }
        }
        counts
    }

    /// Returns the time from each frame's begin marker to its presentation;
    /// see [`FramePacing::latency`].
    #[must_use]
    pub fn latency(&self) -> DurationStats {
        DurationStats::of(self.frames.iter().filter_map(FramePacing::latency))
    }

    /// Returns each lasting change in latency, in each stream: where the
    /// time from frames' begin markers to the screen moved by at least half
    /// a refresh and stayed there for [`LATENCY_STEP_FRAMES`] frames or more,
    /// in order.
    ///
    /// A frame queued in the presentation pipeline behind one that missed
    /// the compositor's cutoff shows here: every frame after it is a refresh
    /// later until one is skipped. A scheduler that learns the extra latency
    /// moves its targets with it, so the frames still look on time, and
    /// their cadence steady; this says the latency itself changed.
    ///
    /// Each frame's refresh interval is the one the stream's frame
    /// opportunities stated when it was to be shown, or else the stream's
    /// median: of the intervals its opportunities stated, or of how often
    /// its frames were shown. A stream with none of them has no steps.
    /// Missing frame numbers, missing latency measurements, and lost events
    /// start a new run, which must establish its own initial latency.
    #[must_use]
    pub fn latency_steps(&self) -> Vec<LatencyStep> {
        let mut steps = Vec::new();
        for stream in self.streams() {
            let Some(median) = self.refresh_interval(stream) else {
                continue;
            };
            let mut latencies = Vec::new();
            let mut previous: Option<(FrameNumber, Duration)> = None;
            for frame in self
                .frames
                .iter()
                .filter(|frame| frame.number.stream() == stream)
            {
                let measured = frame.work.zip(frame.presented).filter(|(work, shown)| {
                    !self
                        .gaps
                        .iter()
                        .any(|gap| gap.overlaps(work.start(), *shown))
                });
                let Some((work, shown)) = measured else {
                    latency_steps(&latencies, &mut steps);
                    latencies.clear();
                    previous = None;
                    continue;
                };
                if previous.is_some_and(|(number, at)| {
                    frame.number.previous() != Some(number)
                        || self.gaps.iter().any(|gap| gap.overlaps(at, shown))
                }) {
                    latency_steps(&latencies, &mut steps);
                    latencies.clear();
                }
                let refresh = self.refresh_at(frame).unwrap_or(median);
                latencies.push((
                    frame.number,
                    shown.saturating_sub(work.start()),
                    refresh / 2,
                ));
                previous = Some((frame.number, shown));
            }
            latency_steps(&latencies, &mut steps);
        }
        steps
    }

    /// The refresh interval the frame's stream stated with the last
    /// opportunity at or before the frame was to be shown, if it stated one.
    fn refresh_at(&self, frame: &FramePacing) -> Option<Duration> {
        let at = frame.target.or(frame.presented)?;
        self.opportunities
            .iter()
            .find(|opportunities| opportunities.stream == frame.number.stream())?
            .stated_interval_at(at)
    }

    /// The refresh interval of a stream: the median its opportunities
    /// stated, or else the median time its frames stayed on screen.
    fn refresh_interval(&self, stream: FrameStream) -> Option<Duration> {
        let stated = self
            .opportunities
            .iter()
            .find(|opportunities| opportunities.stream == stream)
            .map(|opportunities| {
                DurationStats::of(
                    opportunities
                        .times
                        .iter()
                        .filter_map(|(_, interval)| *interval),
                )
                .median()
            })
            .filter(|interval| !interval.is_zero());
        stated
            .or_else(|| {
                Some(
                    DurationStats::of(
                        self.intervals
                            .iter()
                            .filter(|(of, _)| *of == stream)
                            .map(|(_, interval)| *interval),
                    )
                    .median(),
                )
            })
            .filter(|interval| !interval.is_zero())
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
#[non_exhaustive]
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
#[non_exhaustive]
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
#[non_exhaustive]
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
    /// after its deadline, the platform skipped the opportunity it was to
    /// begin at, it began later than it could have, or its recorded GPU
    /// work finished after its target, and neither the times that show it
    /// nor the presentation time was an estimate. Deadlines and targets are plans, not
    /// observations; they are taken as given.
    ///
    /// The condition was observed; that it is what made the frame late
    /// is still the leading explanation, not proof.
    Observed,
    /// It follows from what was recorded, but rests on something that was
    /// not: a queue read from the frame before's presentation, or a
    /// submission or presentation time that was estimated.
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
#[non_exhaustive]
pub struct Why {
    /// The leading explanation: the first condition that held.
    pub cause: LateCause,
    /// How strongly the trace supports it.
    pub support: Support,
    /// How long after its deadline the frame was submitted, when it was.
    pub submitted_late: Option<Duration>,
    /// How much later than it was due the frame opportunity it was to
    /// begin at came, when the platform skipped at least one; see
    /// [`FramePacing::opportunity_late`].
    pub opportunity_late: Option<Duration>,
    /// How long after it could have it began, when more than the
    /// tolerance; see [`FramePacing::began_late`].
    pub began_late: Option<Duration>,
    /// Whether the trace held what judging when it began needs; see
    /// [`FramePacing::begin_judged`].
    pub begin_judged: bool,
    /// Whether the submission time came from a
    /// [`Submitted`](FramePointKind::Submitted) point rather than the frame's
    /// end marker.
    pub submit_reported: bool,
    /// How the submission time was known: the
    /// [`Submitted`](FramePointKind::Submitted) point's basis, or
    /// [`Observed`](TimingBasis::Observed) for the frame's end marker, which
    /// the runtime recorded as it happened.
    pub submitted: TimingBasis,
    /// How long after its target its recorded GPU work finished, when it
    /// did.
    pub gpu_late: Option<Duration>,
    /// Whether GPU work was recorded for the frame at all.
    pub gpu_recorded: bool,
    /// Whether the frame before it was still waiting to be shown at this
    /// one's target.
    pub behind_previous: bool,
    /// Whether it was shown a whole number of refreshes after its target,
    /// within the tolerance: a frame that missed refreshes, rather than one
    /// aimed at a time no refresh came. `None` when the trace does not say
    /// how often its stream refreshed: the frame opportunities near it
    /// were recorded without the refresh interval the platform meant.
    pub whole_refreshes: Option<bool>,
    /// How the frame's presentation time was known.
    pub presented: TimingBasis,
}

impl Why {
    /// Returns whether the cause rests on a time recorded without a basis
    /// ([`TimingBasis::Unknown`]): the presentation time, or the reported
    /// submission time of a missed deadline. Such a time is taken at its
    /// word, so the cause can still be [`Observed`](Support::Observed).
    #[must_use]
    pub fn unstated_basis(&self) -> bool {
        self.presented == TimingBasis::Unknown
            || (self.cause == LateCause::MissedDeadline
                && self.submit_reported
                && self.submitted == TimingBasis::Unknown)
    }
}

impl fmt::Display for Support {
    /// Says how strongly, in a word: `observed`, `inferred`, or
    /// `unresolved`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Observed => "observed",
            Self::Inferred => "inferred",
            Self::Unresolved => "unresolved",
        })
    }
}

impl fmt::Display for Why {
    /// Says the cause and how strongly the trace supports it, then every
    /// condition that held, such as `the platform skipped its opportunity,
    /// observed; its opportunity came 8.33 ms late`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}, {}", self.cause, self.support)?;
        if let Some(by) = self.submitted_late {
            write!(f, "; submitted {} after its deadline", Shown(by))?;
        }
        if let Some(by) = self.opportunity_late {
            write!(f, "; its opportunity came {} late", Shown(by))?;
        }
        if let Some(by) = self.began_late {
            write!(f, "; began {} late", Shown(by))?;
        }
        if let Some(by) = self.gpu_late {
            write!(f, "; GPU finished {} after the target", Shown(by))?;
        }
        if self.behind_previous {
            f.write_str("; the frame before was still waiting")?;
        }
        if self.whole_refreshes == Some(false) {
            f.write_str("; shown between refreshes")?;
        }
        if !self.begin_judged {
            f.write_str("; when it began was not judged")?;
        }
        if self.presented == TimingBasis::Estimated {
            f.write_str("; shown at an estimated time")?;
        }
        Ok(())
    }
}

/// How many late frames' causes had each [`Support`], as
/// [`OutcomeCounts::late_support`] holds them.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct SupportCounts {
    /// Their causes were observed.
    pub observed: u64,
    /// Of the observed, how many rest on a time recorded without saying
    /// whether it was observed or estimated; see [`Why::unstated_basis`].
    pub observed_unstated: u64,
    /// Their causes were inferred.
    pub inferred: u64,
    /// The trace stops short of their causes.
    pub unresolved: u64,
}

/// How many late frames had each [`LateCause`], as
/// [`OutcomeCounts::late_causes`] holds them.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct LateCauseCounts {
    /// Submitted after their deadlines.
    pub missed_deadline: u64,
    /// Begun at a later frame opportunity, because the platform skipped
    /// the one they were meant for.
    pub skipped_opportunity: u64,
    /// Begun later than they could have.
    pub began_late: u64,
    /// Pushed back by frames queued ahead.
    pub backlog: u64,
    /// Their GPU work finished after their targets.
    pub gpu: u64,
    /// Shown between refreshes: their targets were mispredicted.
    pub mispredicted: u64,
    /// Held by the compositor or display.
    pub display: u64,
    /// Submitted in time, with no GPU work to tell the GPU from the display.
    pub after_submit: u64,
    /// Not known.
    pub unknown: u64,
}

/// How many frames in a row must stay at a new latency for
/// [`Pacing::latency_steps`] to count it.
pub const LATENCY_STEP_FRAMES: usize = 5;

/// A lasting change in latency, as [`Pacing::latency_steps`] returns it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct LatencyStep {
    /// The first frame at the new latency.
    pub at: FrameNumber,
    /// The typical latency before: the median of the frames that set it.
    pub from: Duration,
    /// The typical latency after: the median of the
    /// [`LATENCY_STEP_FRAMES`] frames that showed the step.
    pub to: Duration,
}

impl LatencyStep {
    /// Returns `true` for a step up: frames took longer to reach the
    /// screen after it.
    #[must_use]
    pub fn rose(&self) -> bool {
        self.to > self.from
    }
}

impl fmt::Display for LatencyStep {
    /// Says the step in a few words, such as `latency rose from 25.0 ms to
    /// 33.3 ms at frame 412`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "latency {} from {} to {} at frame {}",
            if self.rose() { "rose" } else { "fell" },
            Shown(self.from),
            Shown(self.to),
            self.at,
        )
    }
}

/// Finds the lasting changes in one stream's latencies: runs of
/// [`LATENCY_STEP_FRAMES`] frames each at least its own threshold, half its
/// refresh, from the level set before, the same way.
fn latency_steps(latencies: &[(FrameNumber, Duration, Duration)], steps: &mut Vec<LatencyStep>) {
    let median = |run: &[(FrameNumber, Duration, Duration)]| {
        DurationStats::of(run.iter().map(|(_, latency, _)| *latency)).median()
    };
    let Some(first) = latencies.get(..LATENCY_STEP_FRAMES) else {
        return;
    };
    let mut level = median(first);
    // Where the run of frames away from the level began, and which way.
    let mut run: Option<(usize, bool)> = None;
    for (index, &(_, latency, threshold)) in latencies.iter().enumerate().skip(LATENCY_STEP_FRAMES)
    {
        let up = latency >= level + threshold;
        let down = latency + threshold <= level;
        if threshold.is_zero() || (!up && !down) {
            run = None;
            continue;
        }
        let start = match run {
            Some((start, rising)) if rising == up => start,
            _ => index,
        };
        run = Some((start, up));
        if index + 1 - start >= LATENCY_STEP_FRAMES {
            let to = median(&latencies[start..=index]);
            steps.push(LatencyStep {
                at: latencies[start].0,
                from: level,
                to,
            });
            level = to;
            run = None;
        }
    }
}

/// How many shown frames kept their rhythm, as [`Pacing::cadence`] returns
/// them.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct CadenceCounts {
    /// Shown as long after the frame before as their targets were apart.
    pub steady: u64,
    /// Of the steady, how many were late: shown behind their targets by
    /// about as much as the frame before them. That is latency holding
    /// steady, such as frames queued ahead, not a hitch.
    pub late_but_steady: u64,
    /// Shown later after the frame before than their targets were apart.
    pub stalled: u64,
    /// Shown sooner after the frame before than their targets were apart.
    pub hurried: u64,
}

impl CadenceCounts {
    /// Returns how many frames had their cadence judged.
    #[must_use]
    pub const fn judged(&self) -> u64 {
        self.steady + self.stalled + self.hurried
    }

    /// Returns how many broke the rhythm: stalled or hurried.
    #[must_use]
    pub const fn hitches(&self) -> u64 {
        self.stalled + self.hurried
    }
}

/// How many frame opportunities a trace recorded and the platform skipped,
/// as [`Pacing::opportunities`] returns them.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct OpportunityCounts {
    /// Opportunities the trace recorded and placed.
    pub recorded: u64,
    /// Opportunities the platform skipped between them.
    pub skipped: u64,
    /// Opportunities left out because their clock has no path to the
    /// runtime's.
    pub unplaced: u64,
}

/// How far off a kind of predicted time was from when frames were shown, as
/// [`Pacing::prediction_error`] returns it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
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
#[non_exhaustive]
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
                scheduled: None,
                began: Began::default(),
                dropped: None,
                unplaced_points: 0,
                details: Vec::new(),
                outcome: Outcome::Unknown,
                why: None,
                cadence: None,
            };
            for point in trace.frame_points().get(&number).into_iter().flatten() {
                let (kind, time) = (&point.kind, &point.time);
                if point.basis != TimingBasis::Unknown || !point.args.is_empty() {
                    raw.details.retain(|(held, ..)| !same_kind(*held, *kind));
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
                    FramePointKind::Scheduled(begins_at) => {
                        if let Some(at) = at {
                            raw.scheduled = Some((at, *begins_at));
                        }
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
    let gaps: Vec<TimeRange> = trace.gaps().iter().map(crate::Gap::range).collect();
    let opportunities = stream_opportunities(trace);
    for frame in &mut frames {
        let stream = opportunities
            .iter()
            .find(|opportunities| opportunities.stream == frame.number.stream());
        frame.began = began(frame, stream, &gaps, tolerance);
    }
    for index in 0..frames.len() {
        // The frame just before in the same stream, if the trace holds it.
        let before = index
            .checked_sub(1)
            .map(|before| &frames[before])
            .filter(|before| frames[index].number.previous() == Some(before.number));
        let stream = opportunities
            .iter()
            .find(|opportunities| opportunities.stream == frames[index].number.stream());
        let (judged, why) = outcome(&frames[index], before, stream, tolerance);
        frames[index].outcome = judged;
        frames[index].why = why;
    }
    cadences(&mut frames, &gaps, tolerance);
    let (intervals, interval_breaks) = intervals(&frames, &gaps);
    Pacing {
        frames,
        tolerance,
        intervals,
        interval_breaks,
        gaps,
        opportunities,
    }
}

/// Whether two point kinds are the same kind, whatever a scheduled point
/// begins at.
fn same_kind(a: FramePointKind, b: FramePointKind) -> bool {
    match (a, b) {
        (FramePointKind::Scheduled(_), FramePointKind::Scheduled(_)) => true,
        _ => a == b,
    }
}

/// Places each stream's frame opportunities in the trace's time.
fn stream_opportunities(trace: &Trace) -> Vec<StreamOpportunities> {
    trace
        .frame_opportunities()
        .iter()
        .map(|(stream, held)| {
            let mut unplaced = 0;
            let mut times: Vec<(Duration, Option<Duration>)> = held
                .iter()
                .filter_map(|opportunity| {
                    let time = &opportunity.time;
                    let Some(at) = trace.clocks().to_runtime(time.clock_id(), time.ticks()) else {
                        unplaced += 1;
                        return None;
                    };
                    let interval = opportunity
                        .interval
                        .map(|ticks| Duration::from_nanos(time.period().ticks_to_nanos(ticks)));
                    Some((nanos(at.nanos()), interval))
                })
                .collect();
            times.sort_by_key(|(at, _)| *at);
            let median =
                DurationStats::of(times.windows(2).map(|pair| pair[1].0 - pair[0].0)).median();
            StreamOpportunities {
                stream: *stream,
                times,
                median,
                unplaced,
            }
        })
        .collect()
}

/// Judges when a frame began against when it could have: at its scheduled
/// time, or at the first of its stream's opportunities from then on. For
/// one that was to begin at an opportunity, also judges whether that
/// opportunity came when the one before it said it would.
fn began(
    frame: &FramePacing,
    opportunities: Option<&StreamOpportunities>,
    gaps: &[TimeRange],
    tolerance: Duration,
) -> Began {
    let Some((scheduled, begins_at)) = frame.scheduled else {
        return Began::default();
    };
    // A frame dropped before it began has no begin marker, but whether its
    // opportunity came can still be judged.
    let began = frame.work.map(|work| work.start());
    let late_by = |could: Duration| {
        began
            .and_then(|began| began.checked_sub(could))
            .filter(|by| *by > tolerance)
    };
    match begins_at {
        BeginsAt::Time => Began {
            judged: began.is_some(),
            late: late_by(scheduled),
            opportunity_late: None,
        },
        BeginsAt::Opportunity => {
            let Some(stream) = opportunities else {
                return Began::default();
            };
            let first = stream.times.partition_point(|(at, _)| *at < scheduled);
            let Some(&(offered, _)) = stream.times.get(first) else {
                return Began::default();
            };
            let opportunity_late = first.checked_sub(1).and_then(|before| {
                let from = stream.times[before].0;
                let interval = stream.interval_after(before);
                if interval.is_zero() || gaps.iter().any(|gap| gap.overlaps(from, offered)) {
                    return None;
                }
                // The first time from the scheduled one on that the
                // opportunity before said the next would come.
                let steps = scheduled
                    .saturating_sub(from)
                    .as_nanos()
                    .div_ceil(interval.as_nanos())
                    .max(1);
                let due = from + interval * u32::try_from(steps).ok()?;
                offered.checked_sub(due).filter(|by| *by * 2 >= interval)
            });
            Began {
                judged: began.is_some(),
                late: late_by(offered),
                opportunity_late,
            }
        }
        _ => Began::default(),
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

/// Judges each shown frame's cadence against the frame shown before it in
/// its stream; see [`FramePacing::cadence`].
///
/// Frames known never to have been shown are passed over, as the frame
/// before them stays on screen; a frame the trace cannot say about, a
/// missing frame number, or events lost between two presentations break
/// the run, as for [`Pacing::intervals`].
fn cadences(frames: &mut [FramePacing], gaps: &[TimeRange], tolerance: Duration) {
    // The frame shown last in the run: when it was shown and its target.
    let mut shown: Option<(Duration, Option<Duration>)> = None;
    let mut before: Option<FrameNumber> = None;
    for frame in frames.iter_mut() {
        if before.is_none_or(|number| frame.number.previous() != Some(number)) {
            shown = None;
        }
        before = Some(frame.number);
        let Some(at) = frame.presented else {
            if frame.dropped.is_none() {
                shown = None;
            }
            continue;
        };
        if let Some((since, Some(since_target))) = shown
            && let Some(target) = frame.target
            && !gaps.iter().any(|gap| gap.overlaps(since, at))
        {
            let meant = target.saturating_sub(since_target);
            let spacing = at.saturating_sub(since);
            frame.cadence = Some(if spacing > meant + tolerance {
                Cadence::Stalled {
                    by: spacing - meant,
                }
            } else if spacing + tolerance < meant {
                Cadence::Hurried {
                    by: meant - spacing,
                }
            } else {
                Cadence::Steady
            });
        }
        shown = Some((at, frame.target));
    }
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
    opportunities: Option<&StreamOpportunities>,
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
    // Whole refreshes late, or between them: the refresh interval the
    // platform stated when the frame was to be shown.
    let whole_refreshes = opportunities
        .and_then(|stream| stream.stated_interval_at(target))
        .map(|interval| {
            let off = (presented - target).as_nanos() % interval.as_nanos();
            let tolerance = tolerance.as_nanos();
            off <= tolerance || interval.as_nanos() - off <= tolerance
        });
    let (cause, support) = if submitted_late.is_some() {
        (LateCause::MissedDeadline, Support::Observed)
    } else if raw.began.opportunity_late.is_some() {
        (LateCause::SkippedOpportunity, Support::Observed)
    } else if raw.began.late.is_some() {
        (LateCause::BeganLate, Support::Observed)
    } else if behind_previous {
        (LateCause::Backlog, Support::Inferred)
    } else if gpu_late.is_some() {
        (LateCause::Gpu, Support::Observed)
    } else if whole_refreshes == Some(false) {
        (LateCause::Mispredicted, Support::Inferred)
    } else if !submitted_in_time {
        (LateCause::Unknown, Support::Unresolved)
    } else if raw.gpu.is_some() {
        (LateCause::Display, Support::Unresolved)
    } else {
        (LateCause::AfterSubmit, Support::Unresolved)
    };
    let presented_basis = raw.basis(FramePointKind::Presented);
    let submitted_basis = if raw.submitted.is_some() {
        raw.basis(FramePointKind::Submitted)
    } else {
        TimingBasis::Observed
    };
    // A condition shown by an estimated time, and lateness measured against
    // an estimated presentation, are inferred, not observed.
    let estimated = presented_basis == TimingBasis::Estimated
        || (cause == LateCause::MissedDeadline && submitted_basis == TimingBasis::Estimated);
    let support = if support == Support::Observed && estimated {
        Support::Inferred
    } else {
        support
    };
    let why = Why {
        cause,
        support,
        submitted_late,
        opportunity_late: raw.began.opportunity_late,
        began_late: raw.began.late,
        begin_judged: raw.began.judged,
        submit_reported: raw.submitted.is_some(),
        submitted: submitted_basis,
        gpu_late,
        gpu_recorded: raw.gpu.is_some(),
        behind_previous,
        whole_refreshes,
        presented: presented_basis,
    };
    let late = Outcome::Late {
        by: presented - target,
        cause,
    };
    (late, Some(why))
}
