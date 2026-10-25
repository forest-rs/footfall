// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What an answer covers: whether the data was recorded, what was counted,
//! what was left out, and where events are missing.

use crate::time::{Selection, TimeRange};
use core::fmt;
use core::time::Duration;
use footfall_core::InstrumentationLevel;

/// Where events are missing from a trace: the time they fell in and how
/// many there were, as [`Trace::gaps`](crate::Trace::gaps) returns it.
///
/// The runtime keeps the newest events and evicts the oldest when a thread
/// records faster than readers read, and a consumer can also start reading
/// after the runtime began. Sequence numbers show exactly which events are
/// missing; the range is when they were recorded, from the events around
/// them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Gap {
    pub(crate) range: TimeRange,
    pub(crate) missed: u64,
}

impl Gap {
    /// Returns when the missing events were recorded.
    #[must_use]
    pub const fn range(&self) -> TimeRange {
        self.range
    }

    /// Returns how many events are missing.
    #[must_use]
    pub const fn missed(&self) -> u64 {
        self.missed
    }
}

/// A kind of observation, for asking [`Trace::recorded`](crate::Trace::recorded)
/// whether the producer recorded it.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum Signal {
    /// Scope begins and ends.
    Spans,
    /// Frame, pass, and transaction markers.
    Marks,
    /// Messages.
    Messages,
    /// Flow steps.
    Flows,
    /// Resource lifecycles.
    Resources,
    /// Counter, gauge, and histogram totals.
    Counters,
    /// Counter samples from automatic marker triggers or explicit checkpoints.
    CounterSamples,
}

impl Signal {
    /// Returns the instrumentation level the producer must capture at for
    /// this signal to be recorded.
    #[must_use]
    pub const fn level(self) -> InstrumentationLevel {
        match self {
            Self::Counters | Self::CounterSamples => InstrumentationLevel::Counters,
            Self::Spans | Self::Marks | Self::Messages | Self::Flows => {
                InstrumentationLevel::Scoped
            }
            Self::Resources => InstrumentationLevel::Trace,
        }
    }
}

/// Whether a producer recorded a kind of observation, so an empty answer
/// can be told apart from one about data that was never captured.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Recorded {
    /// The producer recorded it: an empty answer means none happened.
    Yes,
    /// The producer's level excludes it.
    BelowLevel {
        /// The level the producer captured at.
        level: InstrumentationLevel,
        /// The level the observation needs.
        needs: InstrumentationLevel,
    },
    /// The producer was built with instrumentation compiled out.
    CompiledOut,
    /// The producer has counter sampling disabled.
    SamplesOff,
    /// The trace has seen no snapshot, so it cannot tell.
    Unknown,
}

impl Recorded {
    /// Returns `true` for [`Recorded::Yes`].
    #[must_use]
    pub const fn is_yes(self) -> bool {
        matches!(self, Self::Yes)
    }
}

impl fmt::Display for Recorded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Yes => f.write_str("recorded"),
            Self::BelowLevel { level, needs } => write!(
                f,
                "not recorded: the producer captured at level {}, and this needs {}",
                level.as_str(),
                needs.as_str()
            ),
            Self::CompiledOut => f.write_str(
                "not recorded: the producer was built with instrumentation compiled out",
            ),
            Self::SamplesOff => {
                f.write_str("not recorded: the producer has counter sampling disabled")
            }
            Self::Unknown => f.write_str("unknown: no snapshot seen"),
        }
    }
}

/// What a span answer covers, as every query result carries it.
///
/// A number alone cannot say whether it is complete. This does: whether
/// spans were recorded at all, how many were counted and how, which ones
/// could not be (still open, or cut off by the start of the trace), and
/// whether events are missing from the selected time.
///
/// Durations are elapsed wall-clock time on the recording thread; Footfall
/// does not measure CPU time, so a span that waited counts its waiting.
#[derive(Clone, Debug, PartialEq)]
pub struct Basis {
    pub(crate) selection: Selection,
    pub(crate) ranges: Option<(usize, Duration)>,
    pub(crate) recorded: Recorded,
    pub(crate) spans: u64,
    pub(crate) interrupted: u64,
    pub(crate) open: u64,
    pub(crate) unmatched_ends: u64,
    pub(crate) gaps: Vec<Gap>,
    pub(crate) unobserved: Duration,
    pub(crate) converted: u64,
    pub(crate) farthest_conversion: Duration,
    pub(crate) unplaced: u64,
}

impl Basis {
    /// Returns how spans were measured against the selected ranges.
    #[must_use]
    pub const fn selection(&self) -> Selection {
        self.selection
    }

    /// Returns how many ranges were selected, after merging overlapping
    /// ones, and their total length, or `None` when the whole trace was.
    #[must_use]
    pub const fn ranges(&self) -> Option<(usize, Duration)> {
        self.ranges
    }

    /// Returns whether spans were recorded at all.
    #[must_use]
    pub const fn recorded(&self) -> Recorded {
        self.recorded
    }

    /// Returns how many completed spans were counted.
    #[must_use]
    pub const fn spans(&self) -> u64 {
        self.spans
    }

    /// Returns how many of the counted spans were open while events were
    /// lost, so their self time may include nested spans that went missing.
    #[must_use]
    pub const fn interrupted(&self) -> u64 {
        self.interrupted
    }

    /// Returns how many spans matching the query had begun by the end of
    /// the selected time but not yet ended, and so were left out: their
    /// durations are not known yet.
    #[must_use]
    pub const fn open(&self) -> u64 {
        self.open
    }

    /// Returns how many span ends in the selected time had no begin in the
    /// trace, of any name, and so were left out: they began before the trace
    /// or among missing events.
    #[must_use]
    pub const fn unmatched_ends(&self) -> u64 {
        self.unmatched_ends
    }

    /// Returns the gaps that overlap the selected time.
    #[must_use]
    pub fn gaps(&self) -> &[Gap] {
        &self.gaps
    }

    /// Returns how many events are missing from the selected time.
    #[must_use]
    pub fn missed_events(&self) -> u64 {
        self.gaps.iter().map(|gap| gap.missed).sum()
    }

    /// Returns how much of the selected time lies outside what the trace
    /// observed: after its latest snapshot, or before what it has
    /// forgotten.
    #[must_use]
    pub const fn unobserved(&self) -> Duration {
        self.unobserved
    }

    /// Returns how many of the counted spans were timed by another clock,
    /// such as a GPU's, and converted to the trace's time.
    #[must_use]
    pub const fn converted(&self) -> u64 {
        self.converted
    }

    /// Returns how far the farthest converted time lay from the clock
    /// reading used to place it; see [`Placement::distance`](crate::Placement::distance).
    #[must_use]
    pub const fn farthest_conversion(&self) -> Duration {
        self.farthest_conversion
    }

    /// Returns how many spans on timelines the query covers could not be
    /// placed at all, because their clock has no sync or pair leading to
    /// the runtime's clock, and so were left out.
    #[must_use]
    pub const fn unplaced(&self) -> u64 {
        self.unplaced
    }

    /// Returns `true` when nothing qualifies the answer: spans were recorded,
    /// every span in the selected time was counted, no events are missing,
    /// and the whole selected time was observed.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.recorded.is_yes()
            && self.interrupted == 0
            && self.open == 0
            && self.unmatched_ends == 0
            && self.unplaced == 0
            && self.gaps.is_empty()
            && self.unobserved.is_zero()
    }
}

impl fmt::Display for Basis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "metric: elapsed wall-clock time per thread, not CPU time"
        )?;
        if !self.recorded.is_yes() {
            return writeln!(f, "spans: {}", self.recorded);
        }
        match self.ranges {
            Some((count, length)) => writeln!(
                f,
                "population: {} completed spans that {} {count} range(s) totalling {length:?}",
                self.spans,
                self.selection.describe()
            )?,
            None => writeln!(f, "population: {} completed spans", self.spans)?,
        }
        if self.open > 0 || self.unmatched_ends > 0 {
            writeln!(
                f,
                "left out: {} still open, {} ended without a begin",
                self.open, self.unmatched_ends
            )?;
        }
        if self.converted > 0 || self.unplaced > 0 {
            writeln!(
                f,
                "other clocks: {} spans placed, up to {:?} from a clock reading; {} could not be placed",
                self.converted, self.farthest_conversion, self.unplaced
            )?;
        }
        if self.interrupted > 0 {
            writeln!(
                f,
                "caution: {} spans were open while events were lost; their self times may be too large",
                self.interrupted
            )?;
        }
        if self.gaps.is_empty() && self.unobserved.is_zero() {
            writeln!(f, "coverage: complete")
        } else {
            writeln!(
                f,
                "coverage: {} events missing in {} gap(s); {:?} not observed",
                self.missed_events(),
                self.gaps.len(),
                self.unobserved
            )
        }
    }
}
