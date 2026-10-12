// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Frame pacing: whether each frame was shown when it was meant to be, and
//! what its work looked like.

use crate::frame::FrameId;
use crate::query::DurationStats;
use crate::time::{TimeRange, nanos};
use crate::trace::Trace;
use core::time::Duration;
use footfall_core::FramePointKind;
use std::collections::BTreeSet;

/// One numbered frame's pacing: its CPU work, its GPU work, its deadline and
/// target, when it was shown, and what that adds up to, as
/// [`Pacing::frames`] returns it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FramePacing {
    number: u64,
    frame: Option<FrameId>,
    work: Option<TimeRange>,
    gpu: Option<TimeRange>,
    gpu_busy: Duration,
    deadline: Option<Duration>,
    target: Option<Duration>,
    presented: Option<Duration>,
    dropped: bool,
    outcome: Outcome,
}

/// Whether a frame was shown when it was meant to be, as
/// [`FramePacing::outcome`] says.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum Outcome {
    /// Shown at its target, within the tolerance.
    OnTime,
    /// Shown after its target.
    Late {
        /// How much later than its target.
        by: Duration,
    },
    /// Never shown.
    Dropped,
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
    /// Returns `true` for a frame shown late, dropped, or submitted late.
    #[must_use]
    pub const fn missed(self) -> bool {
        matches!(
            self,
            Self::Late { .. } | Self::Dropped | Self::SubmittedLate { .. }
        )
    }
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

    /// Returns when the frame's CPU work ran, between its markers.
    #[must_use]
    pub const fn work(&self) -> Option<TimeRange> {
        self.work
    }

    /// Returns when the frame's GPU work ran: from the first of its timeline
    /// spans to begin to the last to end.
    #[must_use]
    pub const fn gpu(&self) -> Option<TimeRange> {
        self.gpu
    }

    /// Returns how long the GPU was busy with the frame: the summed length
    /// of its outermost timeline spans.
    #[must_use]
    pub const fn gpu_busy(&self) -> Duration {
        self.gpu_busy
    }

    /// Returns the frame's deadline, the latest time its work could be
    /// submitted and still be shown at its target.
    #[must_use]
    pub const fn deadline(&self) -> Option<Duration> {
        self.deadline
    }

    /// Returns when the frame was meant to be shown.
    #[must_use]
    pub const fn target(&self) -> Option<Duration> {
        self.target
    }

    /// Returns when the frame was shown.
    #[must_use]
    pub const fn presented(&self) -> Option<Duration> {
        self.presented
    }

    /// Returns `true` when the frame was reported dropped.
    #[must_use]
    pub const fn dropped(&self) -> bool {
        self.dropped
    }

    /// Returns whether the frame was shown when it was meant to be.
    #[must_use]
    pub const fn outcome(&self) -> Outcome {
        self.outcome
    }

    /// Returns the time from the start of the frame's CPU work to its
    /// presentation: what a user waits.
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

    /// Returns how late a frame could be shown and still count as on time.
    #[must_use]
    pub const fn tolerance(&self) -> Duration {
        self.tolerance
    }

    /// Returns how many frames had each outcome: on time, late, dropped,
    /// submitted late, and unknown.
    #[must_use]
    pub fn counts(&self) -> OutcomeCounts {
        let mut counts = OutcomeCounts::default();
        for frame in &self.frames {
            match frame.outcome {
                Outcome::OnTime => counts.on_time += 1,
                Outcome::Late { .. } => counts.late += 1,
                Outcome::Dropped => counts.dropped += 1,
                Outcome::SubmittedLate { .. } => counts.submitted_late += 1,
                Outcome::Unknown => counts.unknown += 1,
            }
        }
        counts
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
            Outcome::Late { by } => Some(by),
            _ => None,
        }))
    }

    /// Returns the time between consecutive frames' presentations, in frame
    /// order: steady at the refresh interval when pacing is smooth.
    #[must_use]
    pub fn intervals(&self) -> DurationStats {
        DurationStats::of(self.intervals_iter())
    }

    fn intervals_iter(&self) -> impl Iterator<Item = Duration> + '_ {
        self.frames
            .windows(2)
            .filter(|pair| pair[1].number == pair[0].number + 1)
            .filter_map(|pair| Some(pair[1].presented?.saturating_sub(pair[0].presented?)))
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

    /// Returns the time from the start of each frame's CPU work to its
    /// presentation.
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
    /// Frames shown after their targets.
    pub late: u64,
    /// Frames never shown.
    pub dropped: u64,
    /// Frames submitted after their deadlines, with no presentation time.
    pub submitted_late: u64,
    /// Frames with too little to judge.
    pub unknown: u64,
}

impl OutcomeCounts {
    /// Returns how many frames were counted.
    #[must_use]
    pub const fn total(&self) -> u64 {
        self.on_time + self.late + self.dropped + self.submitted_late + self.unknown
    }
}

pub(crate) fn pacing(trace: &Trace, tolerance: Duration) -> Pacing {
    let mut numbers: BTreeSet<u64> = trace.numbered_frames().collect();
    numbers.extend(trace.frame_points().keys().copied());
    let mut unplaced_points = 0_u64;
    let frames = numbers
        .into_iter()
        .map(|number| {
            let frame = trace.frame_numbered(number);
            let mut deadline = None;
            let mut target = None;
            let mut presented = None;
            let mut dropped = false;
            for (kind, time) in trace.frame_points().get(&number).into_iter().flatten() {
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
                match kind {
                    FramePointKind::Deadline => deadline = at.or(deadline),
                    FramePointKind::Target => target = at.or(target),
                    FramePointKind::Presented => presented = at.or(presented),
                    FramePointKind::Dropped => dropped = true,
                    _ => {}
                }
            }
            let spans: Vec<_> = trace.frame_spans(number).collect();
            let gpu = spans
                .iter()
                .map(|span| span.range())
                .reduce(|a, b| TimeRange::new(a.start().min(b.start()), a.end().max(b.end())));
            let gpu_busy = spans
                .iter()
                .filter(|span| span.depth() == 0)
                .map(|span| span.duration())
                .sum();
            let work = frame.map(crate::Frame::range);
            let outcome = if dropped {
                Outcome::Dropped
            } else if let (Some(presented), Some(target)) = (presented, target) {
                if presented > target.saturating_add(tolerance) {
                    Outcome::Late {
                        by: presented - target,
                    }
                } else {
                    Outcome::OnTime
                }
            } else if let (Some(deadline), Some(work)) = (deadline, work)
                && work.end() > deadline
            {
                Outcome::SubmittedLate {
                    by: work.end() - deadline,
                }
            } else {
                Outcome::Unknown
            };
            FramePacing {
                number,
                frame: frame.map(crate::Frame::id),
                work,
                gpu,
                gpu_busy,
                deadline,
                target,
                presented,
                dropped,
                outcome,
            }
        })
        .collect();
    Pacing {
        frames,
        tolerance,
        unplaced_points,
    }
}
