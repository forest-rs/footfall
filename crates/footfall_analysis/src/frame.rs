// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Frames and other marked units of work, with the counters they changed.

use crate::time::TimeRange;
use core::fmt;
use core::time::Duration;
use footfall_core::{ContextId, CounterDescId, MarkerKind, Unit};
use footfall_runtime::CounterKind;
use std::sync::Arc;

/// Identifies a frame, pass, or transaction within its runtime instance:
/// the sequence number of its begin marker. Like a [`SpanId`](crate::SpanId),
/// it means the same thing in every snapshot and capture of that instance.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FrameId(u64);

impl FrameId {
    /// Creates the id of the frame begun by the marker with `sequence`.
    #[must_use]
    pub const fn new(sequence: u64) -> Self {
        Self(sequence)
    }

    /// Returns the sequence number of the frame's begin marker.
    #[must_use]
    pub const fn sequence(self) -> u64 {
        self.0
    }
}

impl fmt::Display for FrameId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "frame@{}", self.0)
    }
}

/// One frame, pass, or transaction: from a begin marker to the end marker
/// that closed it.
///
/// Each end marker closes the most recent open begin marker of the same
/// kind. Frames also carry the counters they changed, from the samples the
/// runtime records at each frame end.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frame {
    pub(crate) id: FrameId,
    pub(crate) kind: MarkerKind,
    pub(crate) context_id: ContextId,
    pub(crate) thread_id: u64,
    pub(crate) begin: Duration,
    pub(crate) end: Duration,
    pub(crate) counters: Vec<FrameCounter>,
}

impl Frame {
    /// Returns the frame's id.
    #[must_use]
    pub const fn id(&self) -> FrameId {
        self.id
    }

    /// Returns whether this is a frame, a pass, or a transaction.
    #[must_use]
    pub const fn kind(&self) -> MarkerKind {
        self.kind
    }

    /// Returns the context of the begin marker.
    #[must_use]
    pub const fn context_id(&self) -> ContextId {
        self.context_id
    }

    /// Returns the thread that recorded the begin marker.
    #[must_use]
    pub const fn thread_id(&self) -> u64 {
        self.thread_id
    }

    /// Returns when the frame began.
    #[must_use]
    pub const fn begin(&self) -> Duration {
        self.begin
    }

    /// Returns when the frame ended.
    #[must_use]
    pub const fn end(&self) -> Duration {
        self.end
    }

    /// Returns the time the frame covers.
    #[must_use]
    pub fn range(&self) -> TimeRange {
        TimeRange::new(self.begin, self.end)
    }

    /// Returns how long the frame lasted.
    #[must_use]
    pub fn duration(&self) -> Duration {
        self.end.saturating_sub(self.begin)
    }

    /// Returns the counters and gauges the frame updated, from the samples
    /// recorded at its end. Only frames carry them, not passes or
    /// transactions, and only when the runtime records samples; see
    /// [`Recorded`](crate::Recorded).
    #[must_use]
    pub fn counters(&self) -> &[FrameCounter] {
        &self.counters
    }

    /// Returns how much the counter named `name` grew during the frame,
    /// summed over every context it was recorded under, or `None` when the
    /// frame did not update it or a change is unknown; see
    /// [`FrameCounter::change`].
    #[must_use]
    pub fn total_change(&self, name: &str) -> Option<u64> {
        let mut changes = self
            .counters
            .iter()
            .filter(|counter| &*counter.name == name)
            .map(FrameCounter::change)
            .peekable();
        changes.peek()?;
        changes.try_fold(0_u64, |total, change| Some(total.saturating_add(change?)))
    }

    /// Returns the counter or gauge named `name` under the context
    /// `context_id`, if the frame updated it.
    #[must_use]
    pub fn counter(&self, name: &str, context_id: ContextId) -> Option<&FrameCounter> {
        self.counters
            .iter()
            .find(|counter| &*counter.name == name && counter.context_id == context_id)
    }
}

/// A counter or gauge a frame updated, as [`Frame::counters`] returns it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameCounter {
    pub(crate) desc_id: CounterDescId,
    pub(crate) name: Arc<str>,
    pub(crate) unit: Unit,
    pub(crate) context_id: ContextId,
    pub(crate) kind: CounterKind,
    pub(crate) value: u64,
    pub(crate) change: Option<u64>,
}

impl FrameCounter {
    /// Returns the descriptor name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the descriptor id, local to the runtime instance.
    #[must_use]
    pub const fn desc_id(&self) -> CounterDescId {
        self.desc_id
    }

    /// Returns the unit.
    #[must_use]
    pub const fn unit(&self) -> Unit {
        self.unit
    }

    /// Returns the context the counter was recorded under.
    #[must_use]
    pub const fn context_id(&self) -> ContextId {
        self.context_id
    }

    /// Returns whether it is an additive counter or a gauge.
    #[must_use]
    pub const fn kind(&self) -> CounterKind {
        self.kind
    }

    /// Returns its value at the end of the frame: cumulative for a counter,
    /// the last value set for a gauge.
    #[must_use]
    pub const fn value(&self) -> u64 {
        self.value
    }

    /// Returns how much a counter grew during the frame.
    ///
    /// This is the difference from the counter's previous sample. For a
    /// counter's first sample it is the whole value when the trace has seen
    /// the runtime's complete history, since the counter was zero before,
    /// and `None` otherwise, since the trace cannot tell. It is also `None`
    /// for the first sample after lost events, and always for a gauge.
    #[must_use]
    pub const fn change(&self) -> Option<u64> {
        self.change
    }
}
