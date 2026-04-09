// Copyright 2026 the Spoor Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Transport-neutral session envelopes for Spoor.
//!
//! This crate sits downstream of `spoor_runtime`. It does not participate in hot-path emission.
//! Instead it gives exporters, local daemons, and future live transports one versioned envelope
//! shape for consuming runtime snapshots.

use spoor_core::{
    ContextId, CounterDescId, FlowId, FlowPhase, LabelKey, LabelValue, MarkerKind, MarkerPhase,
    ResourceId, ResourceKindId, ScopeDescId, ScopeToken,
};
use spoor_runtime::{
    ContextSnapshot, CounterKind, CounterSnapshot, Observation, Snapshot, SnapshotMetadata,
    TimedObservation,
};

/// Current protocol version for Spoor session envelopes.
pub const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::new(1, 0);

/// Semantic protocol version for a stream of session envelopes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProtocolVersion {
    major: u16,
    minor: u16,
}

impl ProtocolVersion {
    /// Creates a new protocol version.
    #[must_use]
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }

    /// Returns the major protocol version.
    #[must_use]
    pub const fn major(self) -> u16 {
        self.major
    }

    /// Returns the minor protocol version.
    #[must_use]
    pub const fn minor(self) -> u16 {
        self.minor
    }
}

/// Chunking policy for snapshot publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Chunking {
    /// Maximum number of contexts in one envelope. `0` means "all remaining".
    pub max_contexts: usize,
    /// Maximum number of counters in one envelope. `0` means "all remaining".
    pub max_counters: usize,
    /// Maximum number of events in one envelope. `0` means "all remaining".
    pub max_events: usize,
}

impl Chunking {
    /// Creates an explicit chunking policy.
    #[must_use]
    pub const fn new(max_contexts: usize, max_counters: usize, max_events: usize) -> Self {
        Self {
            max_contexts,
            max_counters,
            max_events,
        }
    }
}

impl Default for Chunking {
    fn default() -> Self {
        Self::new(256, 256, 256)
    }
}

/// Header common to every session envelope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EnvelopeHeader {
    /// Protocol version used by this envelope.
    pub protocol_version: ProtocolVersion,
    /// Monotonic session sequence copied from the runtime snapshot.
    pub session_sequence: u64,
    /// Zero-based chunk index within the snapshot publication.
    pub chunk_index: u64,
}

/// Borrowed description of one snapshot window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SnapshotSummary<'a> {
    /// Snapshot metadata identifying the process and session.
    pub metadata: &'a SnapshotMetadata,
    /// Snapshot start time relative to runtime creation.
    pub capture_start_micros: u64,
    /// Snapshot end time relative to runtime creation.
    pub captured_at_micros: u64,
    /// Number of canonical contexts in the snapshot.
    pub context_count: usize,
    /// Number of counters in the snapshot.
    pub counter_count: usize,
    /// Number of timestamped events in the snapshot.
    pub event_count: usize,
}

/// Snapshot loss accounting carried at the end of publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DroppedSummary {
    /// Number of observations dropped because a new context could not be interned.
    pub contexts: u64,
    /// Number of counter updates dropped due to counter-key capacity pressure.
    pub counters: u64,
    /// Number of events dropped due to event-capacity pressure.
    pub events: u64,
}

/// One versioned session envelope delivered to a consumer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Envelope<'a> {
    /// Common envelope header.
    pub header: EnvelopeHeader,
    /// Borrowed payload for this chunk.
    pub payload: Payload<'a>,
}

/// Borrowed payload variants for snapshot publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Payload<'a> {
    /// Describes the snapshot window before chunk data arrives.
    SnapshotStart(SnapshotSummary<'a>),
    /// One chunk of canonical context rows.
    Contexts(&'a [ContextSnapshot]),
    /// One chunk of aggregated counter rows.
    Counters(&'a [CounterSnapshot]),
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
    pub process_name: String,
    /// Optional capture-session label.
    pub session_name: Option<String>,
    /// Monotonic sequence number of the capture session.
    pub session_sequence: u64,
}

/// Owned description of one snapshot window.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedSnapshotSummary {
    /// Snapshot metadata identifying the process and session.
    pub metadata: OwnedSnapshotMetadata,
    /// Snapshot start time relative to runtime creation.
    pub capture_start_micros: u64,
    /// Snapshot end time relative to runtime creation.
    pub captured_at_micros: u64,
    /// Number of canonical contexts in the snapshot.
    pub context_count: usize,
    /// Number of counters in the snapshot.
    pub counter_count: usize,
    /// Number of timestamped events in the snapshot.
    pub event_count: usize,
}

/// Owned context row suitable for transport.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedContextSnapshot {
    /// Canonical context id.
    pub id: ContextId,
    /// Explicit semantic context labels.
    pub entries: Vec<OwnedContextEntry>,
}

/// Owned context entry suitable for transport.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedContextEntry {
    /// Semantic label key.
    pub key: LabelKey,
    /// Semantic label value.
    pub value: OwnedLabelValue,
}

/// Owned label value suitable for transport.
#[derive(Clone, Debug, Eq, PartialEq)]
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

/// Owned counter row suitable for transport.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedCounterSnapshot {
    /// Counter descriptor id.
    pub desc_id: CounterDescId,
    /// Stable descriptor name.
    pub name: String,
    /// Canonical semantic context id.
    pub context_id: ContextId,
    /// Whether the value is additive or gauge-like.
    pub kind: CounterKind,
    /// Aggregated value.
    pub value: u64,
    /// Last update time relative to runtime creation.
    pub updated_at_micros: u64,
}

/// Owned timestamped observation suitable for transport.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedTimedObservation {
    /// Observation time in microseconds since runtime creation.
    pub at_micros: u64,
    /// Observation payload.
    pub observation: OwnedObservation,
}

/// Owned observation payload suitable for transport.
#[derive(Clone, Debug, Eq, PartialEq)]
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
}

/// Owned versioned session envelope suitable for transport and storage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedSessionEnvelope {
    /// Common envelope header.
    pub header: EnvelopeHeader,
    /// Owned payload for this chunk.
    pub payload: OwnedSessionPayload,
}

/// Owned payload variants for snapshot publication.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OwnedSessionPayload {
    /// Describes the snapshot window before chunk data arrives.
    SnapshotStart(OwnedSnapshotSummary),
    /// One chunk of canonical context rows.
    Contexts(Vec<OwnedContextSnapshot>),
    /// One chunk of aggregated counter rows.
    Counters(Vec<OwnedCounterSnapshot>),
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
            session_sequence: snapshot.metadata.session_sequence,
            chunk_index,
        },
        payload: Payload::SnapshotStart(SnapshotSummary {
            metadata: &snapshot.metadata,
            capture_start_micros: snapshot.capture_start_micros,
            captured_at_micros: snapshot.captured_at_micros,
            context_count: snapshot.contexts.len(),
            counter_count: snapshot.counters.len(),
            event_count: snapshot.events.len(),
        }),
    })?;
    chunk_index = chunk_index.saturating_add(1);

    for contexts in snapshot
        .contexts
        .chunks(normalized_chunk_size(chunking.max_contexts))
    {
        consumer.consume(Envelope {
            header: envelope_header(snapshot, chunk_index),
            payload: Payload::Contexts(contexts),
        })?;
        chunk_index = chunk_index.saturating_add(1);
    }

    for counters in snapshot
        .counters
        .chunks(normalized_chunk_size(chunking.max_counters))
    {
        consumer.consume(Envelope {
            header: envelope_header(snapshot, chunk_index),
            payload: Payload::Counters(counters),
        })?;
        chunk_index = chunk_index.saturating_add(1);
    }

    for events in snapshot
        .events
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
            contexts: snapshot.dropped_contexts,
            counters: snapshot.dropped_counters,
            events: snapshot.dropped_events,
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
        session_sequence: snapshot.metadata.session_sequence,
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
                            process_name: summary.metadata.process_name.clone(),
                            session_name: summary.metadata.session_name.clone(),
                            session_sequence: summary.metadata.session_sequence,
                        },
                        capture_start_micros: summary.capture_start_micros,
                        captured_at_micros: summary.captured_at_micros,
                        context_count: summary.context_count,
                        counter_count: summary.counter_count,
                        event_count: summary.event_count,
                    })
                }
                Payload::Contexts(contexts) => OwnedSessionPayload::Contexts(
                    contexts.iter().map(owned_context_snapshot).collect(),
                ),
                Payload::Counters(counters) => OwnedSessionPayload::Counters(
                    counters.iter().map(owned_counter_snapshot).collect(),
                ),
                Payload::Events(events) => OwnedSessionPayload::Events(
                    events.iter().map(owned_timed_observation).collect(),
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
        id: snapshot.id,
        entries: snapshot
            .context
            .iter()
            .map(|entry| OwnedContextEntry {
                key: entry.key(),
                value: owned_label_value(entry.value()),
            })
            .collect(),
    }
}

fn owned_label_value(value: LabelValue) -> OwnedLabelValue {
    match value {
        LabelValue::Unsigned(value) => OwnedLabelValue::Unsigned(value),
        LabelValue::Signed(value) => OwnedLabelValue::Signed(value),
        LabelValue::Bool(value) => OwnedLabelValue::Bool(value),
        LabelValue::StaticStr(value) => OwnedLabelValue::String(String::from(value)),
    }
}

fn owned_counter_snapshot(counter: &CounterSnapshot) -> OwnedCounterSnapshot {
    OwnedCounterSnapshot {
        desc_id: counter.desc_id,
        name: String::from(counter.name),
        context_id: counter.context_id,
        kind: counter.kind,
        value: counter.value,
        updated_at_micros: counter.updated_at_micros,
    }
}

fn owned_timed_observation(observation: &TimedObservation) -> OwnedTimedObservation {
    OwnedTimedObservation {
        at_micros: observation.at_micros,
        observation: owned_observation(&observation.observation),
    }
}

fn owned_observation(observation: &Observation) -> OwnedObservation {
    match observation {
        Observation::ScopeBegin {
            desc_id,
            name,
            context_id,
            token,
        } => OwnedObservation::ScopeBegin {
            desc_id: *desc_id,
            name: String::from(*name),
            context_id: *context_id,
            token: *token,
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
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Chunking, Consumer, DroppedSummary, Envelope, PROTOCOL_VERSION, Payload, publish_snapshot,
    };
    use spoor_core::{Context, ContextId, CounterDescId, LabelKey};
    use spoor_runtime::{
        ContextSnapshot, CounterKind, CounterSnapshot, Observation, Snapshot, SnapshotMetadata,
        TimedObservation,
    };

    #[test]
    fn publish_snapshot_chunks_contexts_counters_and_events() {
        let snapshot = Snapshot {
            metadata: SnapshotMetadata {
                process_name: String::from("demo"),
                session_name: Some(String::from("capture")),
                session_sequence: 3,
            },
            capture_start_micros: 10,
            captured_at_micros: 30,
            contexts: vec![
                ContextSnapshot {
                    id: ContextId::new(0),
                    context: Context::root(),
                },
                ContextSnapshot {
                    id: ContextId::new(1),
                    context: Context::root().with(LabelKey::Frame, 7_u32),
                },
            ],
            counters: vec![
                CounterSnapshot {
                    desc_id: CounterDescId::new(1),
                    name: "ops",
                    context_id: ContextId::new(1),
                    kind: CounterKind::Counter,
                    value: 5,
                    updated_at_micros: 20,
                },
                CounterSnapshot {
                    desc_id: CounterDescId::new(2),
                    name: "bytes",
                    context_id: ContextId::new(1),
                    kind: CounterKind::Gauge,
                    value: 9,
                    updated_at_micros: 21,
                },
            ],
            events: vec![
                TimedObservation {
                    at_micros: 22,
                    observation: Observation::ScopeEnd {
                        token: spoor_core::ScopeToken::new(1),
                    },
                },
                TimedObservation {
                    at_micros: 23,
                    observation: Observation::ScopeEnd {
                        token: spoor_core::ScopeToken::new(2),
                    },
                },
            ],
            dropped_contexts: 1,
            dropped_counters: 2,
            dropped_events: 3,
        };

        let mut consumer = RecordingConsumer::default();
        publish_snapshot(&snapshot, Chunking::new(1, 1, 1), &mut consumer)
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
        protocol_version: Option<super::ProtocolVersion>,
        payloads: Vec<&'static str>,
        dropped: Option<DroppedSummary>,
    }

    impl Consumer for RecordingConsumer {
        type Error = ();

        fn consume(&mut self, envelope: Envelope<'_>) -> Result<(), Self::Error> {
            self.protocol_version = Some(envelope.header.protocol_version);
            match envelope.payload {
                Payload::SnapshotStart(summary) => {
                    assert_eq!(summary.metadata.process_name, "demo");
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
