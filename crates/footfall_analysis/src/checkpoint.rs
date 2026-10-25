// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Named sampling boundaries and changes over their intervals.

use crate::{SpanId, TimeRange};
use core::time::Duration;
use footfall_core::{ContextId, CounterDescId, Unit};
use footfall_runtime::CounterKind;
use std::sync::Arc;

/// One counter/gauge value captured as part of a checkpoint batch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointValue {
    pub(crate) desc_id: CounterDescId,
    pub(crate) name: Arc<str>,
    pub(crate) unit: Unit,
    pub(crate) context_id: ContextId,
    pub(crate) kind: CounterKind,
    pub(crate) value: u64,
}

impl CheckpointValue {
    /// Returns the runtime-local counter descriptor id.
    #[must_use]
    pub const fn desc_id(&self) -> CounterDescId {
        self.desc_id
    }
    /// Returns the counter name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Returns the value's unit.
    #[must_use]
    pub const fn unit(&self) -> Unit {
        self.unit
    }
    /// Returns the context under which the counter was recorded.
    #[must_use]
    pub const fn context_id(&self) -> ContextId {
        self.context_id
    }
    /// Returns whether this is an additive counter or gauge.
    #[must_use]
    pub const fn kind(&self) -> CounterKind {
        self.kind
    }
    /// Returns its cumulative counter value or current gauge value.
    #[must_use]
    pub const fn value(&self) -> u64 {
        self.value
    }
}

/// A named global counter checkpoint, identified by its boundary sequence.
///
/// Values are merged across threads. The context and enclosing scope identify
/// where sampling was requested, not exclusive ownership of those values.
/// Sampling locks shards in turn, so this is not an atomic process-wide snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CounterCheckpoint {
    pub(crate) sequence: u64,
    pub(crate) at: Duration,
    pub(crate) name: Arc<str>,
    pub(crate) context: ContextId,
    pub(crate) thread: u64,
    pub(crate) span: Option<SpanId>,
    pub(crate) values: Vec<CheckpointValue>,
    pub(crate) complete: bool,
}

impl CounterCheckpoint {
    /// Returns the boundary's stable event sequence within its runtime.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }
    /// Returns when sampling began, on the trace's clock.
    #[must_use]
    pub const fn at(&self) -> Duration {
        self.at
    }
    /// Returns the checkpoint name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Returns the context that requested sampling.
    #[must_use]
    pub const fn context_id(&self) -> ContextId {
        self.context
    }
    /// Returns the thread that sampled the counters.
    #[must_use]
    pub const fn thread_id(&self) -> u64 {
        self.thread
    }
    /// Returns the enclosing scope, when its begin was retained.
    #[must_use]
    pub const fn span(&self) -> Option<SpanId> {
        self.span
    }
    /// Returns the counter and gauge values retained from this batch.
    #[must_use]
    pub fn values(&self) -> &[CheckpointValue] {
        &self.values
    }
    /// Returns whether every sample expected at this boundary was retained.
    #[must_use]
    pub const fn complete(&self) -> bool {
        self.complete
    }
}

/// One counter/gauge over a checkpoint interval. Gauge values are reported at
/// both boundaries; only additive counters have an unsigned `change`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CounterChange {
    pub(crate) counter: CheckpointValue,
    pub(crate) before: Option<u64>,
    pub(crate) after: Option<u64>,
    pub(crate) change: Option<u64>,
}

impl CounterChange {
    /// Returns the counter's identity and metadata (and a retained endpoint value).
    #[must_use]
    pub const fn counter(&self) -> &CheckpointValue {
        &self.counter
    }
    /// Returns the baseline, or zero for an additive counter absent from a
    /// complete baseline batch. Missing gauge baselines remain unknown.
    #[must_use]
    pub const fn before(&self) -> Option<u64> {
        self.before
    }
    /// Returns the ending value; a row no longer present is unknown.
    #[must_use]
    pub const fn after(&self) -> Option<u64> {
        self.after
    }
    /// Returns the additive increase, or `None` for gauges, missing evidence,
    /// gaps, dropped counter updates, or a decreasing total. This is global interval activity, not
    /// exclusive attribution to one transaction.
    #[must_use]
    pub const fn change(&self) -> Option<u64> {
        self.change
    }
}

/// Changes between two checkpoints from one trace. Overlapping intervals may
/// include the same activity; their changes must not be summed as exclusive work.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CounterChanges {
    pub(crate) range: TimeRange,
    pub(crate) complete: bool,
    pub(crate) counters: Vec<CounterChange>,
}

impl CounterChanges {
    /// Returns the sampling interval (sampling is not atomic across shards).
    #[must_use]
    pub const fn range(&self) -> TimeRange {
        self.range
    }
    /// Returns whether both batches were complete, no events were missing
    /// between them, and the runtime reported no dropped counter updates.
    /// A counter that disappeared can still have an unknown delta.
    #[must_use]
    pub const fn complete(&self) -> bool {
        self.complete
    }
    /// Returns each counter/gauge present at either boundary.
    #[must_use]
    pub fn counters(&self) -> &[CounterChange] {
        &self.counters
    }
}
