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

/// One counter or gauge under one context, sampled at work boundaries: what a
/// timeline plots.
///
/// Samples exist only where the runtime records them, at configured marker
/// ends or explicit checkpoints; the
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

/// A counter's or gauge's value at one sampling point.
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
    pub(crate) observations: Vec<ResourceObservation>,
    pub(crate) kind_id: ResourceKindId,
    pub(crate) kind_name: Arc<str>,
    pub(crate) resource_id: ResourceId,
    pub(crate) context_id: ContextId,
    pub(crate) created: Option<Duration>,
    pub(crate) dropped: Option<Duration>,
    pub(crate) sizes: Vec<(Duration, u64)>,
}

impl Resource {
    /// Returns the retained create, update, and drop observations in order,
    /// including their originating scopes. Absence of a creation means the
    /// trace did not observe the origin; later work must not be substituted.
    #[must_use]
    pub fn observations(&self) -> &[ResourceObservation] {
        &self.observations
    }

    /// Returns the observed creation, or `None` when the resource first
    /// appeared as an update or drop. Its enclosing scope identifies the
    /// creating work, not the resource's subsequent ownership roots.
    #[must_use]
    pub fn creation(&self) -> Option<&ResourceObservation> {
        self.observations
            .first()
            .filter(|event| event.action == ResourceAction::Create)
    }

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

    /// Returns when creation was observed, or `None` when it was not
    /// retained, whether before the trace or in missing events.
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
    ///
    /// This follows the retained lifetime model. For an unobserved creation
    /// it assumes an earlier origin; missing events can hide lifecycle
    /// changes. Inspect [`Self::observations`] and [`Trace::gaps`](crate::Trace::gaps)
    /// before treating this as proof of retention at a particular time.
    #[must_use]
    pub fn alive_at(&self, at: Duration) -> bool {
        self.created.is_none_or(|created| created <= at)
            && self.dropped.is_none_or(|dropped| at < dropped)
    }
}

/// The resource lifecycle action observed at one event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ResourceAction {
    /// A resource was created with a size.
    Create,
    /// Its recorded size changed.
    Update,
    /// It was released.
    Drop,
}

/// Provenance of one retained resource lifecycle event.
///
/// Scope association uses the recorded nesting on this event's thread. It
/// does not infer causal origin from timestamps on another thread or assign
/// ownership to undo stacks, graph caches, or other application roots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceObservation {
    pub(crate) sequence: u64,
    pub(crate) at: Duration,
    pub(crate) thread: u64,
    pub(crate) context: ContextId,
    pub(crate) span: Option<SpanId>,
    pub(crate) action: ResourceAction,
    pub(crate) size: Option<u64>,
}

impl ResourceObservation {
    /// Returns the event's stable sequence within its runtime instance.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }
    /// Returns when it was observed.
    #[must_use]
    pub const fn at(&self) -> Duration {
        self.at
    }
    /// Returns the recording thread.
    #[must_use]
    pub const fn thread_id(&self) -> u64 {
        self.thread
    }
    /// Returns this event's context (which can differ across a lifetime).
    #[must_use]
    pub const fn context_id(&self) -> ContextId {
        self.context
    }
    /// Returns the innermost enclosing scope whose begin was retained.
    /// `None` can mean unscoped work or missing nesting evidence; it does
    /// not establish that no creating work existed.
    #[must_use]
    pub const fn span(&self) -> Option<SpanId> {
        self.span
    }
    /// Returns the lifecycle action.
    #[must_use]
    pub const fn action(&self) -> ResourceAction {
        self.action
    }
    /// Returns the size at creation/update, and `None` for a drop.
    #[must_use]
    pub const fn size(&self) -> Option<u64> {
        self.size
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
