// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Runtime snapshot publication envelopes.

use alloc::{string::String, vec::Vec};

use footfall_core::{
    ContextId, CounterDescId, EventDescId, FlowId, FlowPhase, LabelValue, MarkerKind, MarkerPhase,
    ResourceId, ResourceKindId, ScopeDescId, ScopeToken, Unit,
};
use footfall_runtime::{
    ContextSnapshot, CounterKind, CounterSnapshot, HISTOGRAM_BUCKET_COUNT, HistogramSnapshot,
    Observation, Snapshot, SnapshotMetadata, TimedObservation,
};

use crate::{PROTOCOL_VERSION, ProtocolVersion};

/// Chunking policy for snapshot publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Chunking {
    /// Maximum number of contexts in one envelope. `0` means "all remaining".
    max_contexts: usize,
    /// Maximum number of counters in one envelope. `0` means "all remaining".
    max_counters: usize,
    /// Maximum number of histograms in one envelope. `0` means "all remaining".
    max_histograms: usize,
    /// Maximum number of events in one envelope. `0` means "all remaining".
    max_events: usize,
}

impl Chunking {
    /// Returns the maximum number of contexts in one envelope. `0` means "all remaining".
    #[must_use]
    pub const fn max_contexts(&self) -> usize {
        self.max_contexts
    }

    /// Returns the maximum number of counters in one envelope. `0` means "all remaining".
    #[must_use]
    pub const fn max_counters(&self) -> usize {
        self.max_counters
    }

    /// Returns the maximum number of histograms in one envelope. `0` means "all remaining".
    #[must_use]
    pub const fn max_histograms(&self) -> usize {
        self.max_histograms
    }

    /// Returns the maximum number of events in one envelope. `0` means "all remaining".
    #[must_use]
    pub const fn max_events(&self) -> usize {
        self.max_events
    }
}

impl Chunking {
    /// Creates an explicit chunking policy.
    #[must_use]
    pub const fn new(
        max_contexts: usize,
        max_counters: usize,
        max_histograms: usize,
        max_events: usize,
    ) -> Self {
        Self {
            max_contexts,
            max_counters,
            max_histograms,
            max_events,
        }
    }
}

impl Default for Chunking {
    fn default() -> Self {
        Self::new(256, 256, 256, 256)
    }
}

/// Header common to every session envelope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EnvelopeHeader {
    /// Protocol version used by this envelope.
    protocol_version: ProtocolVersion,
    /// Monotonic session sequence copied from the runtime snapshot.
    session_sequence: u64,
    /// Zero-based chunk index within the snapshot publication.
    chunk_index: u64,
}

impl EnvelopeHeader {
    /// Creates a new `EnvelopeHeader`.
    #[must_use]
    pub const fn new(
        protocol_version: ProtocolVersion,
        session_sequence: u64,
        chunk_index: u64,
    ) -> Self {
        Self {
            protocol_version,
            session_sequence,
            chunk_index,
        }
    }

    /// Returns the protocol version used by this envelope.
    #[must_use]
    pub const fn protocol_version(&self) -> ProtocolVersion {
        self.protocol_version
    }

    /// Returns the monotonic session sequence copied from the runtime snapshot.
    #[must_use]
    pub const fn session_sequence(&self) -> u64 {
        self.session_sequence
    }

    /// Returns the zero-based chunk index within the snapshot publication.
    #[must_use]
    pub const fn chunk_index(&self) -> u64 {
        self.chunk_index
    }
}

/// Borrowed description of one snapshot window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SnapshotSummary<'a> {
    /// Snapshot metadata identifying the process and session.
    metadata: &'a SnapshotMetadata,
    /// Snapshot start time relative to runtime creation.
    capture_start_micros: u64,
    /// Snapshot end time relative to runtime creation.
    captured_at_micros: u64,
    /// Number of canonical contexts in the snapshot.
    context_count: usize,
    /// Number of counters in the snapshot.
    counter_count: usize,
    /// Number of histograms in the snapshot.
    histogram_count: usize,
    /// Number of timestamped events in the snapshot.
    event_count: usize,
}

impl<'a> SnapshotSummary<'a> {
    /// Creates a new `SnapshotSummary`.
    #[must_use]
    pub const fn new(
        metadata: &'a SnapshotMetadata,
        capture_start_micros: u64,
        captured_at_micros: u64,
        context_count: usize,
        counter_count: usize,
        histogram_count: usize,
        event_count: usize,
    ) -> Self {
        Self {
            metadata,
            capture_start_micros,
            captured_at_micros,
            context_count,
            counter_count,
            histogram_count,
            event_count,
        }
    }

    /// Returns the snapshot metadata identifying the process and session.
    #[must_use]
    pub const fn metadata(&self) -> &'a SnapshotMetadata {
        self.metadata
    }

    /// Returns the snapshot start time relative to runtime creation.
    #[must_use]
    pub const fn capture_start_micros(&self) -> u64 {
        self.capture_start_micros
    }

    /// Returns the snapshot end time relative to runtime creation.
    #[must_use]
    pub const fn captured_at_micros(&self) -> u64 {
        self.captured_at_micros
    }

    /// Returns the number of canonical contexts in the snapshot.
    #[must_use]
    pub const fn context_count(&self) -> usize {
        self.context_count
    }

    /// Returns the number of counters in the snapshot.
    #[must_use]
    pub const fn counter_count(&self) -> usize {
        self.counter_count
    }

    /// Returns the number of histograms in the snapshot.
    #[must_use]
    pub const fn histogram_count(&self) -> usize {
        self.histogram_count
    }

    /// Returns the number of timestamped events in the snapshot.
    #[must_use]
    pub const fn event_count(&self) -> usize {
        self.event_count
    }
}

/// Snapshot loss accounting carried at the end of publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DroppedSummary {
    /// Number of observations dropped because a new context could not be interned.
    contexts: u64,
    /// Number of counter updates dropped due to counter-key capacity pressure.
    counters: u64,
    /// Number of events dropped due to event-capacity pressure.
    events: u64,
}

impl DroppedSummary {
    /// Creates a new `DroppedSummary`.
    #[must_use]
    pub const fn new(contexts: u64, counters: u64, events: u64) -> Self {
        Self {
            contexts,
            counters,
            events,
        }
    }

    /// Returns the number of observations dropped because a new context could not be interned.
    #[must_use]
    pub const fn contexts(&self) -> u64 {
        self.contexts
    }

    /// Returns the number of counter updates dropped due to counter-key capacity pressure.
    #[must_use]
    pub const fn counters(&self) -> u64 {
        self.counters
    }

    /// Returns the number of events dropped due to event-capacity pressure.
    #[must_use]
    pub const fn events(&self) -> u64 {
        self.events
    }
}

/// One versioned session envelope delivered to a consumer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Envelope<'a> {
    /// Common envelope header.
    header: EnvelopeHeader,
    /// Borrowed payload for this chunk.
    payload: Payload<'a>,
}

impl<'a> Envelope<'a> {
    /// Creates a new `Envelope`.
    #[must_use]
    pub const fn new(header: EnvelopeHeader, payload: Payload<'a>) -> Self {
        Self { header, payload }
    }

    /// Returns the common envelope header.
    #[must_use]
    pub const fn header(&self) -> EnvelopeHeader {
        self.header
    }

    /// Returns the borrowed payload for this chunk.
    #[must_use]
    pub const fn payload(&self) -> Payload<'a> {
        self.payload
    }
}

/// Borrowed payload variants for snapshot publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Payload<'a> {
    /// Describes the snapshot window before chunk data arrives.
    SnapshotStart(SnapshotSummary<'a>),
    /// One chunk of canonical context rows.
    Contexts(&'a [ContextSnapshot]),
    /// One chunk of aggregated counter rows.
    Counters(&'a [CounterSnapshot]),
    /// One chunk of aggregated histogram rows.
    Histograms(&'a [HistogramSnapshot]),
    /// One chunk of timestamped event rows.
    Events(&'a [TimedObservation]),
    /// Finishes publication with dropped-data totals.
    SnapshotEnd(DroppedSummary),
}

/// Consumer of versioned session envelopes.
pub trait Consumer {
    /// Error returned by this consumer when publication fails.
    type Error;

    /// Consumes one session envelope.
    fn consume(&mut self, envelope: Envelope<'_>) -> Result<(), Self::Error>;
}

/// Owned form of [`SnapshotMetadata`] suitable for transport and storage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedSnapshotMetadata {
    /// Process name used by exporters.
    process_name: String,
    /// Optional capture-session label.
    session_name: Option<String>,
    /// Monotonic sequence number of the capture session.
    session_sequence: u64,
    /// Wall-clock time of runtime creation in microseconds since the Unix
    /// epoch; adding it to any snapshot timestamp yields wall-clock time.
    wall_clock_anchor_micros: u64,
}

impl OwnedSnapshotMetadata {
    /// Creates a new `OwnedSnapshotMetadata`.
    #[must_use]
    pub const fn new(
        process_name: String,
        session_name: Option<String>,
        session_sequence: u64,
        wall_clock_anchor_micros: u64,
    ) -> Self {
        Self {
            process_name,
            session_name,
            session_sequence,
            wall_clock_anchor_micros,
        }
    }

    /// Returns the process name used by exporters.
    #[must_use]
    pub fn process_name(&self) -> &str {
        &self.process_name
    }

    /// Returns the optional capture-session label.
    #[must_use]
    pub fn session_name(&self) -> Option<&str> {
        self.session_name.as_deref()
    }

    /// Returns the monotonic sequence number of the capture session.
    #[must_use]
    pub const fn session_sequence(&self) -> u64 {
        self.session_sequence
    }

    /// Returns the wall-clock time of runtime creation in microseconds since the Unix
    /// epoch; adding it to any snapshot timestamp yields wall-clock time.
    #[must_use]
    pub const fn wall_clock_anchor_micros(&self) -> u64 {
        self.wall_clock_anchor_micros
    }
}

/// Owned description of one snapshot window.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedSnapshotSummary {
    /// Snapshot metadata identifying the process and session.
    metadata: OwnedSnapshotMetadata,
    /// Snapshot start time relative to runtime creation.
    capture_start_micros: u64,
    /// Snapshot end time relative to runtime creation.
    captured_at_micros: u64,
    /// Number of canonical contexts in the snapshot.
    context_count: usize,
    /// Number of counters in the snapshot.
    counter_count: usize,
    /// Number of histograms in the snapshot.
    histogram_count: usize,
    /// Number of timestamped events in the snapshot.
    event_count: usize,
}

impl OwnedSnapshotSummary {
    /// Creates a new `OwnedSnapshotSummary`.
    #[must_use]
    pub const fn new(
        metadata: OwnedSnapshotMetadata,
        capture_start_micros: u64,
        captured_at_micros: u64,
        context_count: usize,
        counter_count: usize,
        histogram_count: usize,
        event_count: usize,
    ) -> Self {
        Self {
            metadata,
            capture_start_micros,
            captured_at_micros,
            context_count,
            counter_count,
            histogram_count,
            event_count,
        }
    }

    /// Returns the snapshot metadata identifying the process and session.
    #[must_use]
    pub const fn metadata(&self) -> &OwnedSnapshotMetadata {
        &self.metadata
    }

    /// Returns the snapshot start time relative to runtime creation.
    #[must_use]
    pub const fn capture_start_micros(&self) -> u64 {
        self.capture_start_micros
    }

    /// Returns the snapshot end time relative to runtime creation.
    #[must_use]
    pub const fn captured_at_micros(&self) -> u64 {
        self.captured_at_micros
    }

    /// Returns the number of canonical contexts in the snapshot.
    #[must_use]
    pub const fn context_count(&self) -> usize {
        self.context_count
    }

    /// Returns the number of counters in the snapshot.
    #[must_use]
    pub const fn counter_count(&self) -> usize {
        self.counter_count
    }

    /// Returns the number of histograms in the snapshot.
    #[must_use]
    pub const fn histogram_count(&self) -> usize {
        self.histogram_count
    }

    /// Returns the number of timestamped events in the snapshot.
    #[must_use]
    pub const fn event_count(&self) -> usize {
        self.event_count
    }
}

/// Owned context row suitable for transport.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedContextSnapshot {
    /// Canonical context id.
    id: ContextId,
    /// Explicit semantic context labels.
    entries: Vec<OwnedContextEntry>,
}

impl OwnedContextSnapshot {
    /// Creates a new `OwnedContextSnapshot`.
    #[must_use]
    pub const fn new(id: ContextId, entries: Vec<OwnedContextEntry>) -> Self {
        Self { id, entries }
    }

    /// Returns the canonical context id.
    #[must_use]
    pub const fn id(&self) -> ContextId {
        self.id
    }

    /// Returns the explicit semantic context labels.
    #[must_use]
    pub fn entries(&self) -> &[OwnedContextEntry] {
        &self.entries
    }
}

/// Owned context entry suitable for transport.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedContextEntry {
    /// Semantic label key name.
    key: String,
    /// Semantic label value.
    value: OwnedLabelValue,
}

impl OwnedContextEntry {
    /// Creates a new `OwnedContextEntry`.
    #[must_use]
    pub const fn new(key: String, value: OwnedLabelValue) -> Self {
        Self { key, value }
    }

    /// Returns the semantic label key name.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Returns the semantic label value.
    #[must_use]
    pub const fn value(&self) -> &OwnedLabelValue {
        &self.value
    }
}

/// Owned label value suitable for transport.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum OwnedLabelValue {
    /// Unsigned numeric payload.
    Unsigned(u64),
    /// Signed numeric payload.
    Signed(i64),
    /// Boolean payload.
    Bool(bool),
    /// String payload.
    String(String),
}

/// Owned per-instance argument suitable for transport.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedArgEntry {
    /// Argument key.
    key: String,
    /// Argument value.
    value: OwnedLabelValue,
}

impl OwnedArgEntry {
    /// Creates a new `OwnedArgEntry`.
    #[must_use]
    pub const fn new(key: String, value: OwnedLabelValue) -> Self {
        Self { key, value }
    }

    /// Returns the argument key.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Returns the argument value.
    #[must_use]
    pub const fn value(&self) -> &OwnedLabelValue {
        &self.value
    }
}

/// Owned counter row suitable for transport.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedCounterSnapshot {
    /// Counter descriptor id.
    desc_id: CounterDescId,
    /// Stable descriptor name.
    name: String,
    /// Unit copied from the counter descriptor.
    unit: Unit,
    /// Canonical semantic context id.
    context_id: ContextId,
    /// Whether the value is additive or gauge-like.
    kind: CounterKind,
    /// Aggregated value.
    value: u64,
    /// Last update time relative to runtime creation.
    updated_at_micros: u64,
}

impl OwnedCounterSnapshot {
    /// Creates a new `OwnedCounterSnapshot`.
    #[must_use]
    pub const fn new(
        desc_id: CounterDescId,
        name: String,
        unit: Unit,
        context_id: ContextId,
        kind: CounterKind,
        value: u64,
        updated_at_micros: u64,
    ) -> Self {
        Self {
            desc_id,
            name,
            unit,
            context_id,
            kind,
            value,
            updated_at_micros,
        }
    }

    /// Returns the counter descriptor id.
    #[must_use]
    pub const fn desc_id(&self) -> CounterDescId {
        self.desc_id
    }

    /// Returns the stable descriptor name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the unit copied from the counter descriptor.
    #[must_use]
    pub const fn unit(&self) -> Unit {
        self.unit
    }

    /// Returns the canonical semantic context id.
    #[must_use]
    pub const fn context_id(&self) -> ContextId {
        self.context_id
    }

    /// Returns whether the value is additive or gauge-like.
    #[must_use]
    pub const fn kind(&self) -> CounterKind {
        self.kind
    }

    /// Returns the aggregated value.
    #[must_use]
    pub const fn value(&self) -> u64 {
        self.value
    }

    /// Returns the last update time relative to runtime creation.
    #[must_use]
    pub const fn updated_at_micros(&self) -> u64 {
        self.updated_at_micros
    }
}

/// Owned histogram row suitable for transport.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedHistogramSnapshot {
    /// Counter descriptor id.
    desc_id: CounterDescId,
    /// Stable descriptor name.
    name: String,
    /// Unit copied from the counter descriptor.
    unit: Unit,
    /// Canonical semantic context id.
    context_id: ContextId,
    /// Number of recorded samples.
    count: u64,
    /// Saturating sum of recorded samples.
    sum: u64,
    /// Smallest recorded sample. Meaningless while `count` is zero.
    min: u64,
    /// Largest recorded sample.
    max: u64,
    /// Power-of-two bucket counts indexed by sample bit length.
    buckets: [u64; HISTOGRAM_BUCKET_COUNT],
    /// Last update time relative to runtime creation.
    updated_at_micros: u64,
}

impl OwnedHistogramSnapshot {
    /// Creates a new `OwnedHistogramSnapshot`.
    #[must_use]
    pub const fn new(
        desc_id: CounterDescId,
        name: String,
        unit: Unit,
        context_id: ContextId,
        count: u64,
        sum: u64,
        min: u64,
        max: u64,
        buckets: [u64; HISTOGRAM_BUCKET_COUNT],
        updated_at_micros: u64,
    ) -> Self {
        Self {
            desc_id,
            name,
            unit,
            context_id,
            count,
            sum,
            min,
            max,
            buckets,
            updated_at_micros,
        }
    }

    /// Returns the counter descriptor id.
    #[must_use]
    pub const fn desc_id(&self) -> CounterDescId {
        self.desc_id
    }

    /// Returns the stable descriptor name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the unit copied from the counter descriptor.
    #[must_use]
    pub const fn unit(&self) -> Unit {
        self.unit
    }

    /// Returns the canonical semantic context id.
    #[must_use]
    pub const fn context_id(&self) -> ContextId {
        self.context_id
    }

    /// Returns the number of recorded samples.
    #[must_use]
    pub const fn count(&self) -> u64 {
        self.count
    }

    /// Returns the saturating sum of recorded samples.
    #[must_use]
    pub const fn sum(&self) -> u64 {
        self.sum
    }

    /// Returns the smallest recorded sample. Meaningless while `count` is zero.
    #[must_use]
    pub const fn min(&self) -> u64 {
        self.min
    }

    /// Returns the largest recorded sample.
    #[must_use]
    pub const fn max(&self) -> u64 {
        self.max
    }

    /// Returns the power-of-two bucket counts indexed by sample bit length.
    #[must_use]
    pub const fn buckets(&self) -> &[u64; HISTOGRAM_BUCKET_COUNT] {
        &self.buckets
    }

    /// Returns the last update time relative to runtime creation.
    #[must_use]
    pub const fn updated_at_micros(&self) -> u64 {
        self.updated_at_micros
    }
}

/// Owned timestamped observation suitable for transport.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedTimedObservation {
    /// Observation time in microseconds since runtime creation.
    at_micros: u64,
    /// Runtime-assigned id of the thread that recorded the observation.
    thread_id: u64,
    /// Observation payload.
    observation: OwnedObservation,
}

impl OwnedTimedObservation {
    /// Creates a new `OwnedTimedObservation`.
    #[must_use]
    pub const fn new(at_micros: u64, thread_id: u64, observation: OwnedObservation) -> Self {
        Self {
            at_micros,
            thread_id,
            observation,
        }
    }

    /// Returns the observation time in microseconds since runtime creation.
    #[must_use]
    pub const fn at_micros(&self) -> u64 {
        self.at_micros
    }

    /// Returns the runtime-assigned id of the thread that recorded the observation.
    #[must_use]
    pub const fn thread_id(&self) -> u64 {
        self.thread_id
    }

    /// Returns the observation payload.
    #[must_use]
    pub const fn observation(&self) -> &OwnedObservation {
        &self.observation
    }
}

/// Owned observation payload suitable for transport.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum OwnedObservation {
    /// Scope begin event.
    ScopeBegin {
        /// Descriptor id for the scope.
        desc_id: ScopeDescId,
        /// Stable descriptor name.
        name: String,
        /// Canonical semantic context id.
        context_id: ContextId,
        /// Opaque scope token returned to the caller.
        token: ScopeToken,
        /// Per-instance arguments carried by this scope begin.
        args: Vec<OwnedArgEntry>,
    },
    /// Scope end event.
    ScopeEnd {
        /// Opaque scope token previously returned from `scope_begin`.
        token: ScopeToken,
    },
    /// Resource creation event.
    ResourceCreate {
        /// Resource kind id.
        kind_id: ResourceKindId,
        /// Stable resource kind name.
        kind_name: String,
        /// Resource instance id.
        resource_id: ResourceId,
        /// Canonical semantic context id.
        context_id: ContextId,
        /// Logical resource size.
        size: u64,
    },
    /// Resource update event.
    ResourceUpdate {
        /// Resource kind id.
        kind_id: ResourceKindId,
        /// Stable resource kind name.
        kind_name: String,
        /// Resource instance id.
        resource_id: ResourceId,
        /// Canonical semantic context id.
        context_id: ContextId,
        /// Logical resource size.
        size: u64,
    },
    /// Resource drop event.
    ResourceDrop {
        /// Resource kind id.
        kind_id: ResourceKindId,
        /// Stable resource kind name.
        kind_name: String,
        /// Resource instance id.
        resource_id: ResourceId,
        /// Canonical semantic context id.
        context_id: ContextId,
    },
    /// Frame, transaction, or pass marker.
    FrameMarker {
        /// Marker kind.
        kind: MarkerKind,
        /// Marker phase.
        phase: MarkerPhase,
        /// Canonical semantic context id.
        context_id: ContextId,
    },
    /// Causal flow observation.
    Flow {
        /// Stable flow id linking related work.
        flow_id: FlowId,
        /// Flow phase.
        phase: FlowPhase,
        /// Canonical semantic context id.
        context_id: ContextId,
    },
    /// Message event with bounded text.
    Message {
        /// Descriptor id for the message channel.
        desc_id: EventDescId,
        /// Stable descriptor name.
        name: String,
        /// Canonical semantic context id.
        context_id: ContextId,
        /// Message text.
        text: String,
        /// Per-instance arguments carried by this message.
        args: Vec<OwnedArgEntry>,
    },
}

/// Owned versioned session envelope suitable for transport and storage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedSessionEnvelope {
    /// Common envelope header.
    header: EnvelopeHeader,
    /// Owned payload for this chunk.
    payload: OwnedSessionPayload,
}

impl OwnedSessionEnvelope {
    /// Creates a new `OwnedSessionEnvelope`.
    #[must_use]
    pub const fn new(header: EnvelopeHeader, payload: OwnedSessionPayload) -> Self {
        Self { header, payload }
    }

    /// Returns the common envelope header.
    #[must_use]
    pub const fn header(&self) -> EnvelopeHeader {
        self.header
    }

    /// Returns the owned payload for this chunk.
    #[must_use]
    pub const fn payload(&self) -> &OwnedSessionPayload {
        &self.payload
    }
}

/// Owned payload variants for snapshot publication.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum OwnedSessionPayload {
    /// Describes the snapshot window before chunk data arrives.
    SnapshotStart(OwnedSnapshotSummary),
    /// One chunk of canonical context rows.
    Contexts(Vec<OwnedContextSnapshot>),
    /// One chunk of aggregated counter rows.
    Counters(Vec<OwnedCounterSnapshot>),
    /// One chunk of aggregated histogram rows.
    Histograms(Vec<OwnedHistogramSnapshot>),
    /// One chunk of timestamped event rows.
    Events(Vec<OwnedTimedObservation>),
    /// Finishes publication with dropped-data totals.
    SnapshotEnd(DroppedSummary),
}

/// Publishes one runtime snapshot to a consumer using a transport-neutral envelope stream.
pub fn publish_snapshot<C: Consumer>(
    snapshot: &Snapshot,
    chunking: Chunking,
    consumer: &mut C,
) -> Result<(), C::Error> {
    let mut chunk_index = 0_u64;
    consumer.consume(Envelope {
        header: EnvelopeHeader {
            protocol_version: PROTOCOL_VERSION,
            session_sequence: snapshot.metadata().session_sequence(),
            chunk_index,
        },
        payload: Payload::SnapshotStart(SnapshotSummary {
            metadata: snapshot.metadata(),
            capture_start_micros: snapshot.capture_start_micros(),
            captured_at_micros: snapshot.captured_at_micros(),
            context_count: snapshot.contexts().len(),
            counter_count: snapshot.counters().len(),
            histogram_count: snapshot.histograms().len(),
            event_count: snapshot.events().len(),
        }),
    })?;
    chunk_index = chunk_index.saturating_add(1);

    for contexts in snapshot
        .contexts()
        .chunks(normalized_chunk_size(chunking.max_contexts))
    {
        consumer.consume(Envelope {
            header: envelope_header(snapshot, chunk_index),
            payload: Payload::Contexts(contexts),
        })?;
        chunk_index = chunk_index.saturating_add(1);
    }

    for counters in snapshot
        .counters()
        .chunks(normalized_chunk_size(chunking.max_counters))
    {
        consumer.consume(Envelope {
            header: envelope_header(snapshot, chunk_index),
            payload: Payload::Counters(counters),
        })?;
        chunk_index = chunk_index.saturating_add(1);
    }

    for histograms in snapshot
        .histograms()
        .chunks(normalized_chunk_size(chunking.max_histograms))
    {
        consumer.consume(Envelope {
            header: envelope_header(snapshot, chunk_index),
            payload: Payload::Histograms(histograms),
        })?;
        chunk_index = chunk_index.saturating_add(1);
    }

    for events in snapshot
        .events()
        .chunks(normalized_chunk_size(chunking.max_events))
    {
        consumer.consume(Envelope {
            header: envelope_header(snapshot, chunk_index),
            payload: Payload::Events(events),
        })?;
        chunk_index = chunk_index.saturating_add(1);
    }

    consumer.consume(Envelope {
        header: envelope_header(snapshot, chunk_index),
        payload: Payload::SnapshotEnd(DroppedSummary {
            contexts: snapshot.dropped_contexts(),
            counters: snapshot.dropped_counters(),
            events: snapshot.dropped_events(),
        }),
    })
}

/// Collects one runtime snapshot into owned transport-friendly envelopes.
#[must_use]
pub fn collect_snapshot(snapshot: &Snapshot, chunking: Chunking) -> Vec<OwnedSessionEnvelope> {
    let mut collector = CollectingConsumer::default();
    publish_snapshot(snapshot, chunking, &mut collector)
        .expect("collecting consumer is infallible");
    collector.envelopes
}

fn envelope_header(snapshot: &Snapshot, chunk_index: u64) -> EnvelopeHeader {
    EnvelopeHeader {
        protocol_version: PROTOCOL_VERSION,
        session_sequence: snapshot.metadata().session_sequence(),
        chunk_index,
    }
}

fn normalized_chunk_size(max_items: usize) -> usize {
    if max_items == 0 {
        usize::MAX
    } else {
        max_items
    }
}

impl Envelope<'_> {
    /// Converts one borrowed envelope into its owned equivalent.
    #[must_use]
    pub fn into_owned(self) -> OwnedSessionEnvelope {
        OwnedSessionEnvelope {
            header: self.header,
            payload: match self.payload {
                Payload::SnapshotStart(summary) => {
                    OwnedSessionPayload::SnapshotStart(OwnedSnapshotSummary {
                        metadata: OwnedSnapshotMetadata {
                            process_name: String::from(summary.metadata.process_name()),
                            session_name: summary.metadata.session_name().map(String::from),
                            session_sequence: summary.metadata.session_sequence(),
                            wall_clock_anchor_micros: summary.metadata.wall_clock_anchor_micros(),
                        },
                        capture_start_micros: summary.capture_start_micros,
                        captured_at_micros: summary.captured_at_micros,
                        context_count: summary.context_count,
                        counter_count: summary.counter_count,
                        histogram_count: summary.histogram_count,
                        event_count: summary.event_count,
                    })
                }
                Payload::Contexts(contexts) => OwnedSessionPayload::Contexts(
                    contexts.iter().map(owned_context_snapshot).collect(),
                ),
                Payload::Counters(counters) => OwnedSessionPayload::Counters(
                    counters.iter().map(owned_counter_snapshot).collect(),
                ),
                Payload::Histograms(histograms) => OwnedSessionPayload::Histograms(
                    histograms.iter().map(owned_histogram_snapshot).collect(),
                ),
                Payload::Events(events) => OwnedSessionPayload::Events(
                    events.iter().filter_map(owned_timed_observation).collect(),
                ),
                Payload::SnapshotEnd(dropped) => OwnedSessionPayload::SnapshotEnd(dropped),
            },
        }
    }
}

#[derive(Default)]
struct CollectingConsumer {
    envelopes: Vec<OwnedSessionEnvelope>,
}

impl Consumer for CollectingConsumer {
    type Error = core::convert::Infallible;

    fn consume(&mut self, envelope: Envelope<'_>) -> Result<(), Self::Error> {
        self.envelopes.push(envelope.into_owned());
        Ok(())
    }
}

fn owned_context_snapshot(snapshot: &ContextSnapshot) -> OwnedContextSnapshot {
    OwnedContextSnapshot {
        id: snapshot.id(),
        entries: snapshot
            .context()
            .iter()
            .map(|entry| OwnedContextEntry {
                key: String::from(entry.key().name()),
                value: owned_label_value(entry.value()),
            })
            .collect(),
    }
}

fn owned_args(args: footfall_core::Args) -> Vec<OwnedArgEntry> {
    args.iter()
        .map(|entry| OwnedArgEntry {
            key: String::from(entry.key()),
            value: owned_label_value(entry.value()),
        })
        .collect()
}

fn owned_label_value(value: LabelValue) -> OwnedLabelValue {
    match value {
        LabelValue::Unsigned(value) => OwnedLabelValue::Unsigned(value),
        LabelValue::Signed(value) => OwnedLabelValue::Signed(value),
        LabelValue::Bool(value) => OwnedLabelValue::Bool(value),
        LabelValue::StaticStr(value) => OwnedLabelValue::String(String::from(value)),
        // Value kinds this protocol version cannot carry travel as their
        // debug text rather than being dropped.
        _ => OwnedLabelValue::String(alloc::format!("{value:?}")),
    }
}

fn owned_counter_snapshot(counter: &CounterSnapshot) -> OwnedCounterSnapshot {
    OwnedCounterSnapshot {
        desc_id: counter.desc_id(),
        name: String::from(counter.name()),
        unit: counter.unit(),
        context_id: counter.context_id(),
        kind: counter.kind(),
        value: counter.value(),
        updated_at_micros: counter.updated_at_micros(),
    }
}

fn owned_histogram_snapshot(histogram: &HistogramSnapshot) -> OwnedHistogramSnapshot {
    OwnedHistogramSnapshot {
        desc_id: histogram.desc_id(),
        name: String::from(histogram.name()),
        unit: histogram.unit(),
        context_id: histogram.context_id(),
        count: histogram.count(),
        sum: histogram.sum(),
        min: histogram.min(),
        max: histogram.max(),
        buckets: *histogram.buckets(),
        updated_at_micros: histogram.updated_at_micros(),
    }
}

fn owned_timed_observation(observation: &TimedObservation) -> Option<OwnedTimedObservation> {
    Some(OwnedTimedObservation {
        at_micros: observation.at_micros(),
        thread_id: observation.thread_id(),
        observation: owned_observation(observation.observation())?,
    })
}

/// Returns `None` for observation kinds this protocol version cannot carry.
fn owned_observation(observation: &Observation) -> Option<OwnedObservation> {
    Some(match observation {
        Observation::ScopeBegin {
            desc_id,
            name,
            context_id,
            token,
            args,
        } => OwnedObservation::ScopeBegin {
            desc_id: *desc_id,
            name: String::from(*name),
            context_id: *context_id,
            token: *token,
            args: owned_args(*args),
        },
        Observation::ScopeEnd { token } => OwnedObservation::ScopeEnd { token: *token },
        Observation::ResourceCreate {
            kind_id,
            kind_name,
            resource_id,
            context_id,
            size,
        } => OwnedObservation::ResourceCreate {
            kind_id: *kind_id,
            kind_name: String::from(*kind_name),
            resource_id: *resource_id,
            context_id: *context_id,
            size: *size,
        },
        Observation::ResourceUpdate {
            kind_id,
            kind_name,
            resource_id,
            context_id,
            size,
        } => OwnedObservation::ResourceUpdate {
            kind_id: *kind_id,
            kind_name: String::from(*kind_name),
            resource_id: *resource_id,
            context_id: *context_id,
            size: *size,
        },
        Observation::ResourceDrop {
            kind_id,
            kind_name,
            resource_id,
            context_id,
        } => OwnedObservation::ResourceDrop {
            kind_id: *kind_id,
            kind_name: String::from(*kind_name),
            resource_id: *resource_id,
            context_id: *context_id,
        },
        Observation::FrameMarker {
            kind,
            phase,
            context_id,
        } => OwnedObservation::FrameMarker {
            kind: *kind,
            phase: *phase,
            context_id: *context_id,
        },
        Observation::Flow {
            flow_id,
            phase,
            context_id,
        } => OwnedObservation::Flow {
            flow_id: *flow_id,
            phase: *phase,
            context_id: *context_id,
        },
        Observation::Message {
            desc_id,
            name,
            context_id,
            text,
            args,
        } => OwnedObservation::Message {
            desc_id: *desc_id,
            name: String::from(*name),
            context_id: *context_id,
            text: text.clone(),
            args: owned_args(*args),
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};

    use footfall_core::{Context, ContextId, CounterDescId, LabelKey, Unit};

    static FRAME: LabelKey = LabelKey::new("frame");
    use footfall_runtime::{
        ContextSnapshot, CounterKind, CounterSnapshot, Observation, Snapshot, SnapshotMetadata,
        TimedObservation,
    };

    use super::{Chunking, Consumer, DroppedSummary, Envelope, Payload, publish_snapshot};
    use crate::PROTOCOL_VERSION;

    #[test]
    fn publish_snapshot_chunks_contexts_counters_and_events() {
        let mut histogram = footfall_runtime::HistogramSnapshot::new(
            CounterDescId::new(3),
            "frame_micros",
            Unit::Micros,
            ContextId::new(1),
        );
        histogram.record(8, 20);
        histogram.record(16, 21);
        let metadata = SnapshotMetadata::new("demo")
            .with_session_name("capture")
            .with_session_sequence(3)
            .with_wall_clock_anchor_micros(1_700_000_000_000_000);
        let snapshot = Snapshot::builder(metadata)
            .capture_window(10, 30)
            .context(ContextSnapshot::new(ContextId::new(0), Context::root()))
            .context(ContextSnapshot::new(
                ContextId::new(1),
                Context::root().with(&FRAME, 7_u32),
            ))
            .counter(CounterSnapshot::new(
                CounterDescId::new(1),
                "ops",
                Unit::Count,
                ContextId::new(1),
                CounterKind::Counter,
                5,
                20,
            ))
            .counter(CounterSnapshot::new(
                CounterDescId::new(2),
                "bytes",
                Unit::Bytes,
                ContextId::new(1),
                CounterKind::Gauge,
                9,
                21,
            ))
            .histogram(histogram)
            .event(TimedObservation::new(
                22,
                0,
                1,
                Observation::ScopeEnd {
                    token: footfall_core::ScopeToken::new(1),
                },
            ))
            .event(TimedObservation::new(
                23,
                1,
                2,
                Observation::ScopeEnd {
                    token: footfall_core::ScopeToken::new(2),
                },
            ))
            .dropped(1, 2, 3)
            .build();

        let mut consumer = RecordingConsumer::default();
        publish_snapshot(&snapshot, Chunking::new(1, 1, 1, 1), &mut consumer)
            .expect("publication should succeed");

        assert_eq!(consumer.protocol_version, Some(PROTOCOL_VERSION));
        assert_eq!(
            consumer.payloads,
            vec![
                "start",
                "contexts:1",
                "contexts:1",
                "counters:1",
                "counters:1",
                "histograms:1",
                "events:1",
                "events:1",
                "end",
            ]
        );
        assert_eq!(
            consumer.dropped,
            Some(DroppedSummary {
                contexts: 1,
                counters: 2,
                events: 3,
            })
        );
    }

    #[derive(Default)]
    struct RecordingConsumer {
        protocol_version: Option<crate::ProtocolVersion>,
        payloads: Vec<&'static str>,
        dropped: Option<DroppedSummary>,
    }

    impl Consumer for RecordingConsumer {
        type Error = ();

        fn consume(&mut self, envelope: Envelope<'_>) -> Result<(), Self::Error> {
            self.protocol_version = Some(envelope.header.protocol_version);
            match envelope.payload {
                Payload::SnapshotStart(summary) => {
                    assert_eq!(summary.metadata.process_name(), "demo");
                    assert_eq!(summary.context_count, 2);
                    self.payloads.push("start");
                }
                Payload::Contexts(rows) => {
                    self.payloads.push(match rows.len() {
                        1 => "contexts:1",
                        _ => "contexts:n",
                    });
                }
                Payload::Counters(rows) => {
                    self.payloads.push(match rows.len() {
                        1 => "counters:1",
                        _ => "counters:n",
                    });
                }
                Payload::Histograms(rows) => {
                    self.payloads.push(match rows.len() {
                        1 => "histograms:1",
                        _ => "histograms:n",
                    });
                }
                Payload::Events(rows) => {
                    self.payloads.push(match rows.len() {
                        1 => "events:1",
                        _ => "events:n",
                    });
                }
                Payload::SnapshotEnd(dropped) => {
                    self.payloads.push("end");
                    self.dropped = Some(dropped);
                }
            }
            Ok(())
        }
    }
}
