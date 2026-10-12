// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Counter series, messages, resource lifetimes, and flows: what a timeline
//! shows beside its spans.

use crate::span::SpanId;
use crate::time::TimeRange;
use core::time::Duration;
use footfall_core::{
    ContextId, CounterDescId, EventDescId, FlowId, FlowPhase, ResourceId, ResourceKindId, Unit,
};
use footfall_runtime::CounterKind;
use footfall_session::OwnedArgEntry;
use std::sync::Arc;

/// One counter or gauge under one context, sampled at each frame end: what a
/// timeline plots.
///
/// Samples exist only where the runtime records them, at frame ends; the
/// latest value of every counter, sampled or not, is in
/// [`Trace::counters`](crate::Trace::counters).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CounterSeries {
    pub(crate) desc_id: CounterDescId,
    pub(crate) name: Arc<str>,
    pub(crate) unit: Unit,
    pub(crate) context_id: ContextId,
    pub(crate) kind: CounterKind,
    pub(crate) samples: Vec<CounterSample>,
    /// Whether events were lost since the last sample.
    pub(crate) after_gap: bool,
}

impl CounterSeries {
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

    /// Returns the context the values were recorded under.
    #[must_use]
    pub const fn context_id(&self) -> ContextId {
        self.context_id
    }

    /// Returns whether it is an additive counter or a gauge.
    #[must_use]
    pub const fn kind(&self) -> CounterKind {
        self.kind
    }

    /// Returns every retained sample, by time.
    #[must_use]
    pub fn samples(&self) -> &[CounterSample] {
        &self.samples
    }

    /// Returns the samples taken in `range`, by time.
    #[must_use]
    pub fn samples_in(&self, range: TimeRange) -> &[CounterSample] {
        let first = self
            .samples
            .partition_point(|sample| sample.at < range.start());
        let last = self
            .samples
            .partition_point(|sample| sample.at < range.end());
        &self.samples[first..last]
    }
}

/// A counter's or gauge's value at one frame end.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CounterSample {
    pub(crate) at: Duration,
    pub(crate) sequence: u64,
    pub(crate) value: u64,
    pub(crate) change: Option<u64>,
}

impl CounterSample {
    /// Returns when the sample was taken.
    #[must_use]
    pub const fn at(&self) -> Duration {
        self.at
    }

    /// Returns the sample event's sequence number.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns the value: cumulative for a counter, the last value set for
    /// a gauge.
    #[must_use]
    pub const fn value(&self) -> u64 {
        self.value
    }

    /// Returns how much a counter grew since its previous sample; see
    /// [`FrameCounter::change`](crate::FrameCounter::change) for when it is
    /// unknown.
    #[must_use]
    pub const fn change(&self) -> Option<u64> {
        self.change
    }
}

/// One message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Message {
    pub(crate) sequence: u64,
    pub(crate) at: Duration,
    pub(crate) thread_id: u64,
    pub(crate) desc_id: EventDescId,
    pub(crate) name: Arc<str>,
    pub(crate) context_id: ContextId,
    pub(crate) text: String,
    pub(crate) args: Vec<OwnedArgEntry>,
    pub(crate) span: Option<SpanId>,
}

impl Message {
    /// Returns the message event's sequence number, which identifies it
    /// within the runtime instance.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns when the message was recorded.
    #[must_use]
    pub const fn at(&self) -> Duration {
        self.at
    }

    /// Returns the thread that recorded it.
    #[must_use]
    pub const fn thread_id(&self) -> u64 {
        self.thread_id
    }

    /// Returns the event descriptor's id, local to the runtime instance.
    #[must_use]
    pub const fn desc_id(&self) -> EventDescId {
        self.desc_id
    }

    /// Returns the event descriptor's name: the message's channel.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the context it was attributed to.
    #[must_use]
    pub const fn context_id(&self) -> ContextId {
        self.context_id
    }

    /// Returns the text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the per-instance arguments.
    #[must_use]
    pub fn args(&self) -> &[OwnedArgEntry] {
        &self.args
    }

    /// Returns the innermost span open on the recording thread when the
    /// message was recorded.
    #[must_use]
    pub const fn span(&self) -> Option<SpanId> {
        self.span
    }
}

/// One resource's life: when it was created, how its size changed, and
/// when it was dropped, as far as the trace saw.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Resource {
    pub(crate) kind_id: ResourceKindId,
    pub(crate) kind_name: Arc<str>,
    pub(crate) resource_id: ResourceId,
    pub(crate) context_id: ContextId,
    pub(crate) created: Option<Duration>,
    pub(crate) dropped: Option<Duration>,
    pub(crate) sizes: Vec<(Duration, u64)>,
}

impl Resource {
    /// Returns the resource kind's id, local to the runtime instance.
    #[must_use]
    pub const fn kind_id(&self) -> ResourceKindId {
        self.kind_id
    }

    /// Returns the resource kind's name.
    #[must_use]
    pub fn kind_name(&self) -> &str {
        &self.kind_name
    }

    /// Returns the resource's id within its kind.
    #[must_use]
    pub const fn resource_id(&self) -> ResourceId {
        self.resource_id
    }

    /// Returns the context of its latest observation.
    #[must_use]
    pub const fn context_id(&self) -> ContextId {
        self.context_id
    }

    /// Returns when it was created, or `None` when its creation came before
    /// the trace's first events.
    #[must_use]
    pub const fn created(&self) -> Option<Duration> {
        self.created
    }

    /// Returns when it was dropped, or `None` while it lives.
    #[must_use]
    pub const fn dropped(&self) -> Option<Duration> {
        self.dropped
    }

    /// Returns each size it was given, with when, starting with its size at
    /// creation.
    #[must_use]
    pub fn sizes(&self) -> &[(Duration, u64)] {
        &self.sizes
    }

    /// Returns its latest size.
    #[must_use]
    pub fn size(&self) -> Option<u64> {
        self.sizes.last().map(|(_, size)| *size)
    }

    /// Returns its size at `at`, or `None` when it was not alive then or
    /// the trace did not see its size by then.
    #[must_use]
    pub fn size_at(&self, at: Duration) -> Option<u64> {
        if !self.alive_at(at) {
            return None;
        }
        let known = self.sizes.partition_point(|(when, _)| *when <= at);
        known.checked_sub(1).map(|index| self.sizes[index].1)
    }

    /// Returns `true` when it was alive at `at`: created at or before it,
    /// or before the trace began, and not yet dropped.
    #[must_use]
    pub fn alive_at(&self, at: Duration) -> bool {
        self.created.is_none_or(|created| created <= at)
            && self.dropped.is_none_or(|dropped| at < dropped)
    }
}

/// One causal flow: the steps that link work across threads and time, each
/// placed in the span it happened in.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Flow {
    pub(crate) id: FlowId,
    pub(crate) steps: Vec<FlowStep>,
}

impl Flow {
    /// Returns the flow's id.
    #[must_use]
    pub const fn id(&self) -> FlowId {
        self.id
    }

    /// Returns the steps the trace saw, in order.
    #[must_use]
    pub fn steps(&self) -> &[FlowStep] {
        &self.steps
    }

    /// Returns `true` when the trace saw both the flow's begin and its end.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.steps
            .first()
            .is_some_and(|step| step.phase == FlowPhase::Begin)
            && self
                .steps
                .last()
                .is_some_and(|step| step.phase == FlowPhase::End)
    }
}

/// One step of a [`Flow`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FlowStep {
    pub(crate) sequence: u64,
    pub(crate) at: Duration,
    pub(crate) thread_id: u64,
    pub(crate) phase: FlowPhase,
    pub(crate) context_id: ContextId,
    pub(crate) span: Option<SpanId>,
}

impl FlowStep {
    /// Returns the step event's sequence number.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns when the step was recorded.
    #[must_use]
    pub const fn at(&self) -> Duration {
        self.at
    }

    /// Returns the thread that recorded it.
    #[must_use]
    pub const fn thread_id(&self) -> u64 {
        self.thread_id
    }

    /// Returns whether it begins, continues, or ends the flow.
    #[must_use]
    pub const fn phase(&self) -> FlowPhase {
        self.phase
    }

    /// Returns the context it was attributed to.
    #[must_use]
    pub const fn context_id(&self) -> ContextId {
        self.context_id
    }

    /// Returns the innermost span open on the recording thread at the
    /// step: the work the flow passed through.
    #[must_use]
    pub const fn span(&self) -> Option<SpanId> {
        self.span
    }
}
