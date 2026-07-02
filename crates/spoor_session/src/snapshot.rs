// Copyright 2026 the Spoor Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Runtime snapshot publication envelopes.

use alloc::{string::String, vec::Vec};

use spoor_core::{
    ContextId, CounterDescId, FlowId, FlowPhase, LabelKey, LabelValue, MarkerKind, MarkerPhase,
    ResourceId, ResourceKindId, ScopeDescId, ScopeToken, Unit,
};
use spoor_runtime::{
    ContextSnapshot, CounterKind, CounterSnapshot, HISTOGRAM_BUCKET_COUNT, HistogramSnapshot,
    Observation, Snapshot, SnapshotMetadata, TimedObservation,
};

use crate::{PROTOCOL_VERSION, ProtocolVersion};

/// Chunking policy for snapshot publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Chunking {
    /// Maximum number of contexts in one envelope. `0` means "all remaining".
    pub max_contexts: usize,
    /// Maximum number of counters in one envelope. `0` means "all remaining".
    pub max_counters: usize,
    /// Maximum number of histograms in one envelope. `0` means "all remaining".
    pub max_histograms: usize,
    /// Maximum number of events in one envelope. `0` means "all remaining".
    pub max_events: usize,
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
    /// Number of histograms in the snapshot.
    pub histogram_count: usize,
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
    pub process_name: String,
    /// Optional capture-session label.
    pub session_name: Option<String>,
    /// Monotonic sequence number of the capture session.
    pub session_sequence: u64,
    /// Wall-clock time of runtime creation in microseconds since the Unix
    /// epoch; adding it to any snapshot timestamp yields wall-clock time.
    pub wall_clock_anchor_micros: u64,
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
    /// Number of histograms in the snapshot.
    pub histogram_count: usize,
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

/// Owned per-instance argument suitable for transport.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedArgEntry {
    /// Argument key.
    pub key: String,
    /// Argument value.
    pub value: OwnedLabelValue,
}

/// Owned counter row suitable for transport.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedCounterSnapshot {
    /// Counter descriptor id.
    pub desc_id: CounterDescId,
    /// Stable descriptor name.
    pub name: String,
    /// Unit copied from the counter descriptor.
    pub unit: Unit,
    /// Canonical semantic context id.
    pub context_id: ContextId,
    /// Whether the value is additive or gauge-like.
    pub kind: CounterKind,
    /// Aggregated value.
    pub value: u64,
    /// Last update time relative to runtime creation.
    pub updated_at_micros: u64,
}

/// Owned histogram row suitable for transport.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedHistogramSnapshot {
    /// Counter descriptor id.
    pub desc_id: CounterDescId,
    /// Stable descriptor name.
    pub name: String,
    /// Unit copied from the counter descriptor.
    pub unit: Unit,
    /// Canonical semantic context id.
    pub context_id: ContextId,
    /// Number of recorded samples.
    pub count: u64,
    /// Saturating sum of recorded samples.
    pub sum: u64,
    /// Smallest recorded sample. Meaningless while `count` is zero.
    pub min: u64,
    /// Largest recorded sample.
    pub max: u64,
    /// Power-of-two bucket counts indexed by sample bit length.
    pub buckets: [u64; HISTOGRAM_BUCKET_COUNT],
    /// Last update time relative to runtime creation.
    pub updated_at_micros: u64,
}

/// Owned timestamped observation suitable for transport.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedTimedObservation {
    /// Observation time in microseconds since runtime creation.
    pub at_micros: u64,
    /// Runtime-assigned id of the thread that recorded the observation.
    pub thread_id: u64,
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
            session_sequence: snapshot.metadata.session_sequence,
            chunk_index,
        },
        payload: Payload::SnapshotStart(SnapshotSummary {
            metadata: &snapshot.metadata,
            capture_start_micros: snapshot.capture_start_micros,
            captured_at_micros: snapshot.captured_at_micros,
            context_count: snapshot.contexts.len(),
            counter_count: snapshot.counters.len(),
            histogram_count: snapshot.histograms.len(),
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

    for histograms in snapshot
        .histograms
        .chunks(normalized_chunk_size(chunking.max_histograms))
    {
        consumer.consume(Envelope {
            header: envelope_header(snapshot, chunk_index),
            payload: Payload::Histograms(histograms),
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
                            wall_clock_anchor_micros: summary.metadata.wall_clock_anchor_micros,
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

fn owned_args(args: spoor_core::Args) -> Vec<OwnedArgEntry> {
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
    }
}

fn owned_counter_snapshot(counter: &CounterSnapshot) -> OwnedCounterSnapshot {
    OwnedCounterSnapshot {
        desc_id: counter.desc_id,
        name: String::from(counter.name),
        unit: counter.unit,
        context_id: counter.context_id,
        kind: counter.kind,
        value: counter.value,
        updated_at_micros: counter.updated_at_micros,
    }
}

fn owned_histogram_snapshot(histogram: &HistogramSnapshot) -> OwnedHistogramSnapshot {
    OwnedHistogramSnapshot {
        desc_id: histogram.desc_id,
        name: String::from(histogram.name),
        unit: histogram.unit,
        context_id: histogram.context_id,
        count: histogram.count,
        sum: histogram.sum,
        min: histogram.min,
        max: histogram.max,
        buckets: histogram.buckets,
        updated_at_micros: histogram.updated_at_micros,
    }
}

fn owned_timed_observation(observation: &TimedObservation) -> OwnedTimedObservation {
    OwnedTimedObservation {
        at_micros: observation.at_micros,
        thread_id: observation.thread_id,
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
    }
}

#[cfg(test)]
mod tests {
    use alloc::{string::String, vec, vec::Vec};

    use spoor_core::{Context, ContextId, CounterDescId, LabelKey, Unit};
    use spoor_runtime::{
        ContextSnapshot, CounterKind, CounterSnapshot, Observation, Snapshot, SnapshotMetadata,
        TimedObservation,
    };

    use super::{Chunking, Consumer, DroppedSummary, Envelope, Payload, publish_snapshot};
    use crate::PROTOCOL_VERSION;

    #[test]
    fn publish_snapshot_chunks_contexts_counters_and_events() {
        let snapshot = Snapshot {
            metadata: SnapshotMetadata {
                process_name: String::from("demo"),
                session_name: Some(String::from("capture")),
                session_sequence: 3,
                wall_clock_anchor_micros: 1_700_000_000_000_000,
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
                    unit: Unit::Count,
                    context_id: ContextId::new(1),
                    kind: CounterKind::Counter,
                    value: 5,
                    updated_at_micros: 20,
                },
                CounterSnapshot {
                    desc_id: CounterDescId::new(2),
                    name: "bytes",
                    unit: Unit::Bytes,
                    context_id: ContextId::new(1),
                    kind: CounterKind::Gauge,
                    value: 9,
                    updated_at_micros: 21,
                },
            ],
            histograms: vec![spoor_runtime::HistogramSnapshot {
                desc_id: CounterDescId::new(3),
                name: "frame_micros",
                unit: Unit::Micros,
                context_id: ContextId::new(1),
                count: 2,
                sum: 24,
                min: 8,
                max: 16,
                buckets: {
                    let mut buckets = [0; spoor_runtime::HISTOGRAM_BUCKET_COUNT];
                    buckets[4] = 1;
                    buckets[5] = 1;
                    buckets
                },
                updated_at_micros: 21,
            }],
            events: vec![
                TimedObservation {
                    at_micros: 22,
                    thread_id: 1,
                    observation: Observation::ScopeEnd {
                        token: spoor_core::ScopeToken::new(1),
                    },
                },
                TimedObservation {
                    at_micros: 23,
                    thread_id: 2,
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
