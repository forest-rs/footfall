// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Time ranges, and how a span is measured against one.

use core::fmt;
use core::time::Duration;

/// A stretch of a trace's time, from [`start`](Self::start) up to but not
/// including [`end`](Self::end), measured from the producing runtime's
/// creation, like every time in a trace.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TimeRange {
    start: Duration,
    end: Duration,
}

impl TimeRange {
    /// Creates the range from `start` up to `end`; an `end` before `start`
    /// makes an empty range at `start`.
    #[must_use]
    pub fn new(start: Duration, end: Duration) -> Self {
        Self {
            start,
            end: end.max(start),
        }
    }

    /// Creates the range from `start` lasting `length`.
    #[must_use]
    pub fn starting_at(start: Duration, length: Duration) -> Self {
        Self::new(start, start.saturating_add(length))
    }

    /// Returns where the range starts.
    #[must_use]
    pub const fn start(&self) -> Duration {
        self.start
    }

    /// Returns where the range ends, exclusive.
    #[must_use]
    pub const fn end(&self) -> Duration {
        self.end
    }

    /// Returns how long the range lasts.
    #[must_use]
    pub fn length(&self) -> Duration {
        self.end.saturating_sub(self.start)
    }

    /// Returns `true` when the range covers no time.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }

    /// Returns `true` when `at` lies in the range.
    #[must_use]
    pub fn contains(&self, at: Duration) -> bool {
        self.start <= at && at < self.end
    }

    /// Returns the part of the range that `other` also covers, if any.
    #[must_use]
    pub fn intersection(&self, other: Self) -> Option<Self> {
        let start = self.start.max(other.start);
        let end = self.end.min(other.end);
        (start < end).then_some(Self { start, end })
    }

    /// Returns `true` when an interval from `begin` to `end` shares time
    /// with the range. An instant, where `begin == end`, does when the range
    /// contains it.
    #[must_use]
    pub fn overlaps(&self, begin: Duration, end: Duration) -> bool {
        if begin == end {
            self.contains(begin)
        } else {
            begin < self.end && end > self.start
        }
    }

    /// Returns how much of the interval from `begin` to `end` lies in the
    /// range.
    #[must_use]
    pub fn clip(&self, begin: Duration, end: Duration) -> Duration {
        self.end.min(end).saturating_sub(self.start.max(begin))
    }
}

impl fmt::Display for TimeRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}..{:?}", self.start, self.end)
    }
}

/// How a span counts toward a query over time ranges, such as frames.
///
/// The choice matters most for spans that cross a range's edge, such as a
/// worker's task that began in one frame and finished in the next.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Selection {
    /// Spans that began in a range, counted whole.
    ///
    /// Every span belongs to at most one of a sequence of adjacent ranges,
    /// so totals over consecutive frames add up without double counting,
    /// but a span can last past the range it is counted in.
    #[default]
    StartsWithin,
    /// Spans that share any time with a range, counted whole.
    ///
    /// This finds everything that was running, but a span crossing the edge
    /// between two adjacent ranges counts in both.
    Overlaps,
    /// Only the time each span spent inside the ranges: durations and self
    /// times are clipped to them.
    ///
    /// Totals measure the time spent inside the ranges, which is what adds
    /// up to a range's length on one thread.
    Inside,
}

impl Selection {
    /// Returns how the selection reads in a sentence: "began in",
    /// "overlapped", or "clipped to".
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::StartsWithin => "began in",
            Self::Overlaps => "overlapped",
            Self::Inside => "clipped to",
        }
    }
}

/// Sorts `ranges` and merges the ones that overlap or touch, so each instant
/// lies in at most one.
pub(crate) fn merge_ranges(mut ranges: Vec<TimeRange>) -> Vec<TimeRange> {
    ranges.retain(|range| !range.is_empty());
    ranges.sort();
    let mut merged: Vec<TimeRange> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    merged
}

/// Converts a runtime timestamp, in nanoseconds, to a trace time.
pub(crate) const fn nanos(at: u64) -> Duration {
    Duration::from_nanos(at)
}
