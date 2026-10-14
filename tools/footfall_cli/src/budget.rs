// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What makes a frame slow, for `analyze summary` and `analyze slow`: a
//! budget given on the command line, each frame's own deadline when the
//! trace records them, or a default budget when it does not.
//!
//! A deadline comes from the frame scheduler, so judging by it holds at any
//! refresh rate, and when the rate varies. A fixed budget assumes one rate.

use crate::format;
use core::time::Duration;
use footfall::analysis::{Frame, FrameId, Trace};
use std::collections::BTreeMap;

/// The budget used when none is given and the trace has no deadlines: one
/// refresh at 60 Hz.
pub(crate) const DEFAULT_MS: f64 = 16.7;

/// How frames are judged slow.
#[derive(Clone, Debug)]
pub(crate) enum Budget {
    /// Frames longer than `limit`; `default` when no budget was given and
    /// the trace has no deadlines.
    Fixed { limit: Duration, default: bool },
    /// Frames submitted after their own deadlines: for each frame with a
    /// deadline and a submission, whether it missed.
    Deadlines(BTreeMap<FrameId, bool>),
}

impl Budget {
    /// Judges by `budget_ms` when it is given; otherwise by the frames' own
    /// deadlines when `trace` records any, and by [`DEFAULT_MS`] when not.
    pub(crate) fn for_trace(trace: &Trace, budget_ms: Option<f64>) -> Self {
        if let Some(ms) = budget_ms {
            return Self::fixed(ms, false);
        }
        // The tolerance applies to presentation, not to deadlines.
        let missed: BTreeMap<_, _> = trace
            .pacing(Duration::ZERO)
            .frames()
            .iter()
            .filter_map(|frame| Some((frame.frame()?, !frame.met_deadline()?)))
            .collect();
        if missed.is_empty() {
            Self::fixed(DEFAULT_MS, true)
        } else {
            Self::Deadlines(missed)
        }
    }

    fn fixed(ms: f64, default: bool) -> Self {
        Self::Fixed {
            limit: Duration::from_secs_f64(ms.max(0.0) / 1000.0),
            default,
        }
    }

    /// Returns whether `frame` was slow, or `None` when it has no deadline
    /// to judge it by.
    pub(crate) fn is_slow(&self, frame: &Frame) -> Option<bool> {
        match self {
            Self::Fixed { limit, .. } => Some(frame.duration() > *limit),
            Self::Deadlines(missed) => missed.get(&frame.id()).copied(),
        }
    }

    /// Returns what a slow frame was, to follow a count: "over 16.7 ms" or
    /// "submitted after their deadlines".
    pub(crate) fn describe(&self) -> String {
        match self {
            Self::Fixed { limit, .. } => format!("over {}", format::duration(*limit)),
            Self::Deadlines(_) => String::from("submitted after their deadlines"),
        }
    }

    /// Returns a note on where the judgment came from, when it was not
    /// asked for: the default budget, used because there were no deadlines.
    pub(crate) fn note(&self) -> Option<&'static str> {
        match self {
            Self::Fixed { default: true, .. } => Some(
                "the default budget: the trace has no frame deadlines; set one with --budget-ms",
            ),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use footfall::analysis::Trace;
    use footfall::runtime::Runtime;
    use footfall::session::OwnedSnapshot;
    use footfall::{
        Backend as _, ClockDesc, ClockReading, ContextHandle, FrameNumber, FramePoint,
        FramePointKind, MarkerKind, MarkerPhase, TickPeriod,
    };

    use super::Budget;

    static HOST: ClockDesc = ClockDesc::new("host");

    /// A trace of three frames of at least 2 ms, the second submitted after
    /// its deadline, or with no deadlines at all.
    fn trace(deadlines: bool) -> Trace {
        // Not installed: the top test installs the global backend.
        let runtime = Runtime::default();
        runtime.clock_sync(ClockReading::new(&HOST, 0, TickPeriod::NANOSECOND));
        let marker = |phase, number: u64| {
            runtime.numbered_marker(
                MarkerKind::Frame,
                phase,
                FrameNumber::from(number),
                ContextHandle::ROOT,
            );
        };
        for number in 1..=3_u64 {
            marker(MarkerPhase::Begin, number);
            std::thread::sleep(Duration::from_millis(2));
            marker(MarkerPhase::End, number);
            if deadlines {
                // Frame 2's deadline passed as the run began; the others'
                // are an hour away.
                let deadline = if number == 2 { 0 } else { 3_600_000_000_000 };
                runtime.frame_point(&FramePoint::at(
                    number,
                    FramePointKind::Deadline,
                    &HOST,
                    TickPeriod::NANOSECOND,
                    deadline,
                ));
            }
        }
        Trace::from_snapshot(&OwnedSnapshot::from(runtime.snapshot()))
    }

    fn slow(trace: &Trace, budget: &Budget) -> Vec<Option<bool>> {
        trace
            .frames()
            .iter()
            .map(|frame| budget.is_slow(frame))
            .collect()
    }

    #[test]
    fn frames_are_judged_by_their_deadlines_when_the_trace_has_them() {
        let with = trace(true);
        let budget = Budget::for_trace(&with, None);
        assert!(matches!(budget, Budget::Deadlines(_)));
        assert_eq!(slow(&with, &budget), [Some(false), Some(true), Some(false)]);
        assert_eq!(budget.describe(), "submitted after their deadlines");
        assert!(budget.note().is_none());

        // A budget given is used even so.
        let given = Budget::for_trace(&with, Some(1.0));
        assert_eq!(slow(&with, &given), [Some(true); 3]);
        assert!(given.note().is_none());

        // Without deadlines, the default budget, and the report says so.
        let without = trace(false);
        let default = Budget::for_trace(&without, None);
        // A frame's length is wall-clock time, so a loaded machine can push
        // these past any budget; check the budget, not which side they fall.
        let Budget::Fixed {
            limit,
            default: true,
        } = default
        else {
            panic!("the default budget, not {default:?}");
        };
        assert_eq!(limit, Duration::from_secs_f64(super::DEFAULT_MS / 1000.0));
        assert!(slow(&without, &default).iter().all(Option::is_some));
        assert!(default.note().is_some());
    }
}
