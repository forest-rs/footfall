// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Golden fixtures and decoder limits.

use alloc::{string::String, vec, vec::Vec};

use footfall_core::{
    Context, ContextId, CounterDescId, EventDescId, FlowId, FlowPhase, LabelKey, MarkerKind,
    MarkerPhase, ScopeToken,
};
use footfall_runtime::{CounterKind, Snapshot, SnapshotMetadata, ThreadSnapshot};
use std::io::ErrorKind;

use super::{
    MAX_CONTEXT_ENTRIES_PER_CONTEXT, MAX_ROWS_PER_ENVELOPE, MAX_SESSION_ENVELOPE_BYTES,
    MAX_SESSION_STRING_BYTES, decode_envelope, encode_envelope, encode_header,
    encode_owned_envelope, encode_u8, encode_u32, encode_u64,
};
use crate::{
    Chunking, Envelope, EnvelopeHeader, OwnedArgEntry, OwnedContextEntry, OwnedContextSnapshot,
    OwnedCounterSnapshot, OwnedLabelValue, OwnedObservation, OwnedSessionEnvelope,
    OwnedSessionPayload, OwnedSnapshotSummary, OwnedTimedObservation, PROTOCOL_VERSION, Payload,
    RowCounts, collect_snapshot,
};

static FRAME: LabelKey = LabelKey::new("frame");
static REASON: LabelKey = LabelKey::new("reason");

fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut hex, byte| {
        write!(hex, "{byte:02x}").expect("writing to a String cannot fail");
        hex
    })
}

/// Golden wire fixtures. These freeze the encoded byte layout that external
/// consumers depend on; a change here means the wire format changed and the
/// wire-format document must be updated to match.
#[test]
fn encoded_envelopes_match_golden_fixtures() {
    let counters = OwnedSessionEnvelope::new(
        EnvelopeHeader::new(PROTOCOL_VERSION, 2),
        OwnedSessionPayload::Counters(vec![OwnedCounterSnapshot::new(
            CounterDescId::new(3),
            String::from("ops"),
            footfall_core::Unit::Count,
            ContextId::new(1),
            CounterKind::Counter,
            7,
            14,
        )]),
    );
    let encoded = encode_owned_envelope(&counters).expect("encoding should succeed");
    assert_eq!(
        to_hex(&encoded),
        "01000000020000000000000003010000000300000000000000030000006f707301010000\
         00000000000007000000000000000e00000000000000"
    );

    let events = OwnedSessionEnvelope::new(
        EnvelopeHeader::new(PROTOCOL_VERSION, 3),
        OwnedSessionPayload::Events(vec![OwnedTimedObservation::new(
            15,
            4,
            2,
            OwnedObservation::ScopeBegin {
                desc_id: footfall_core::ScopeDescId::new(5),
                name: String::from("record"),
                context_id: ContextId::new(1),
                token: ScopeToken::new(9),
                args: vec![OwnedArgEntry::new(
                    String::from("items"),
                    OwnedLabelValue::Unsigned(3),
                )],
            },
        )]),
    );
    let encoded = encode_owned_envelope(&events).expect("encoding should succeed");
    assert_eq!(
        to_hex(&encoded),
        "01000000030000000000000005010000000f000000000000000400000000000000020000\
         0000000000000500000000000000060000007265636f7264010000000000000009000000\
         0000000001050000006974656d73000300000000000000"
    );

    // Context rows carry label keys by name. The production encoder for
    // borrowed runtime snapshots must produce the same bytes.
    let header = EnvelopeHeader::new(PROTOCOL_VERSION, 1);
    let contexts = OwnedSessionEnvelope::new(
        header,
        OwnedSessionPayload::Contexts(vec![OwnedContextSnapshot::new(
            ContextId::new(1),
            vec![
                OwnedContextEntry::new(String::from("frame"), OwnedLabelValue::Unsigned(7)),
                OwnedContextEntry::new(
                    String::from("reason"),
                    OwnedLabelValue::String(String::from("invalidate")),
                ),
            ],
        )]),
    );
    let expected = "010000000100000000000000020100000001000000000000000200000005000000667261\
                    6d6500070000000000000006000000726561736f6e030a000000696e76616c6964617465";
    let encoded = encode_owned_envelope(&contexts).expect("encoding should succeed");
    assert_eq!(to_hex(&encoded), expected.replace(' ', ""));

    let borrowed = [footfall_runtime::ContextSnapshot::new(
        ContextId::new(1),
        Context::root()
            .with(&FRAME, 7_u32)
            .with(&REASON, "invalidate"),
    )];
    let encoded = encode_envelope(Envelope::new(header, Payload::Contexts(&borrowed)))
        .expect("encoding should succeed");
    assert_eq!(to_hex(&encoded), expected.replace(' ', ""));
}

/// Golden fixtures for the envelopes that frame a snapshot, built from
/// the layouts in the wire-format document.
#[test]
fn snapshot_framing_envelopes_match_golden_fixtures() {
    let metadata = SnapshotMetadata::new("demo")
        .with_instance_id(0x0102_0304_0506_0708)
        .with_wall_clock_anchor_micros(9)
        .with_producer_version("0.1.0")
        .with_capture(crate::CaptureSettings::new(
            footfall_core::InstrumentationLevel::Scoped,
            16,
            64,
            32,
        ));
    let start = OwnedSessionEnvelope::new(
        EnvelopeHeader::new(PROTOCOL_VERSION, 0),
        OwnedSessionPayload::SnapshotStart(OwnedSnapshotSummary::new(
            metadata,
            20,
            crate::EventWindow::new(1, 3, 1),
            RowCounts::new(1, 2, 0, 0, 1),
        )),
    );
    assert_eq!(
        to_hex(&encode_owned_envelope(&start).expect("encoding should succeed")),
        "010000000000000000000000000400000064656d6f000807060504030201090000000000\
         000005000000302e312e3002100000000000000040000000000000002000000000000000\
         140000000000000001000000000000000300000000000000010000000000000001000000\
         02000000000000000000000001000000"
    );

    let threads = OwnedSessionEnvelope::new(
        EnvelopeHeader::new(PROTOCOL_VERSION, 1),
        OwnedSessionPayload::Threads(vec![
            ThreadSnapshot::new(3, Some(String::from("render"))),
            ThreadSnapshot::new(4, None),
        ]),
    );
    assert_eq!(
        to_hex(&encode_owned_envelope(&threads).expect("encoding should succeed")),
        "01000000010000000000000001020000000300000000000000010600000072656e646572\
         040000000000000000"
    );

    let end = OwnedSessionEnvelope::new(
        EnvelopeHeader::new(PROTOCOL_VERSION, 2),
        OwnedSessionPayload::SnapshotEnd(
            crate::Drops::new()
                .with_refused_contexts(1)
                .with_unattributed(2)
                .with_counter_updates(3)
                .with_evicted_events(4),
        ),
    );
    assert_eq!(
        to_hex(&encode_owned_envelope(&end).expect("encoding should succeed")),
        "010000000200000000000000060100000000000000020000000000000003000000000000\
         000400000000000000"
    );

    for envelope in [start, threads, end] {
        let bytes = encode_owned_envelope(&envelope).expect("encoding should succeed");
        assert_eq!(
            decode_envelope(&bytes).expect("decoding should succeed"),
            envelope
        );
    }
}

#[test]
fn codec_round_trips_owned_session_envelopes() {
    let mut histogram = footfall_runtime::HistogramSnapshot::new(
        CounterDescId::new(4),
        "frame_micros",
        footfall_core::Unit::Micros,
        ContextId::new(0),
    );
    histogram.record(8, 14);
    histogram.record(16, 15);
    let metadata = SnapshotMetadata::new("demo")
        .with_session_name("wt")
        .with_instance_id(0x1234_5678_9abc_def0)
        .with_wall_clock_anchor_micros(1_700_000_000_000_000)
        .with_producer_version("0.1.0")
        .with_capture(crate::CaptureSettings::new(
            footfall_core::InstrumentationLevel::Scoped,
            16,
            64,
            32,
        ));
    let snapshot = Snapshot::builder(metadata)
        .captured_at_micros(20)
        .window(crate::EventWindow::new(0, 3, 1))
        .thread(ThreadSnapshot::new(2, Some(String::from("worker"))))
        .context(footfall_runtime::ContextSnapshot::new(
            ContextId::new(0),
            Context::root()
                .with(&FRAME, 9_u32)
                .with(&REASON, "invalidate"),
        ))
        .counter(footfall_runtime::CounterSnapshot::new(
            CounterDescId::new(3),
            "ops",
            footfall_core::Unit::Count,
            ContextId::new(0),
            CounterKind::Counter,
            7,
            14,
        ))
        .histogram(histogram)
        .event(footfall_runtime::TimedObservation::new(
            15,
            0,
            2,
            footfall_runtime::Observation::Flow {
                flow_id: FlowId::new(8),
                phase: FlowPhase::Step,
                context_id: ContextId::new(0),
            },
        ))
        .event(footfall_runtime::TimedObservation::new(
            16,
            1,
            2,
            footfall_runtime::Observation::Message {
                desc_id: EventDescId::new(9),
                name: "app_log",
                context_id: ContextId::new(0),
                text: String::from("cache warmed"),
                args: footfall_core::Args::none().with("level", "info"),
            },
        ))
        .drops(
            footfall_runtime::Drops::new()
                .with_refused_contexts(0)
                .with_counter_updates(1)
                .with_evicted_events(2),
        )
        .build();

    let envelopes = collect_snapshot(&snapshot, Chunking::new(8, 8, 8, 8));
    let round_tripped = envelopes
        .iter()
        .map(|envelope| {
            decode_envelope(&encode_owned_envelope(envelope).expect("encoding should succeed"))
                .expect("decoding should succeed")
        })
        .collect::<Vec<_>>();

    assert_eq!(round_tripped, envelopes);
    assert!(matches!(
        &round_tripped[1].payload(),
        OwnedSessionPayload::Threads(threads) if threads[0].name() == Some("worker")
    ));
    assert!(matches!(
        &round_tripped[4].payload(),
        OwnedSessionPayload::Histograms(histograms)
            if histograms[0].name() == "frame_micros" && histograms[0].buckets()[4] == 1
    ));
    assert!(matches!(
        &round_tripped[5].payload(),
        OwnedSessionPayload::Events(events)
            if matches!(&events[0].observation(), OwnedObservation::Flow {
                flow_id,
                phase: FlowPhase::Step,
                context_id
            } if *flow_id == FlowId::new(8) && *context_id == ContextId::new(0))
    ));
}

#[test]
fn cursor_detects_trailing_bytes() {
    let envelope = OwnedSessionEnvelope::new(
        EnvelopeHeader::new(PROTOCOL_VERSION, 0),
        OwnedSessionPayload::Events(vec![OwnedTimedObservation::new(
            2,
            0,
            1,
            OwnedObservation::FrameMarker {
                kind: MarkerKind::Frame,
                phase: MarkerPhase::Begin,
                context_id: ContextId::new(0),
            },
        )]),
    );
    let mut encoded = encode_owned_envelope(&envelope).expect("encoding should succeed");
    encoded.push(99);
    let error = decode_envelope(&encoded).expect_err("decoder should reject trailing bytes");
    assert_eq!(error.kind(), ErrorKind::InvalidData);
}

#[test]
fn decoder_rejects_oversized_envelope_frames() {
    let bytes = vec![0_u8; MAX_SESSION_ENVELOPE_BYTES + 1];
    let error = decode_envelope(&bytes).expect_err("decoder should reject oversized frames");

    assert_eq!(error.kind(), ErrorKind::InvalidData);
}

#[test]
fn decoder_rejects_oversized_strings_before_payload_allocation() {
    let mut bytes = Vec::new();
    encode_header(&mut bytes, EnvelopeHeader::new(PROTOCOL_VERSION, 0));
    encode_u8(&mut bytes, 0);
    encode_u32(
        &mut bytes,
        u32::try_from(MAX_SESSION_STRING_BYTES + 1).expect("test string bound should fit u32"),
    );

    let error = decode_envelope(&bytes).expect_err("decoder should reject oversized strings");

    assert_eq!(error.kind(), ErrorKind::InvalidData);
}

#[test]
fn decoder_rejects_oversized_row_counts_before_allocation() {
    let mut bytes = Vec::new();
    encode_header(&mut bytes, EnvelopeHeader::new(PROTOCOL_VERSION, 0));
    encode_u8(&mut bytes, 1);
    encode_u32(
        &mut bytes,
        u32::try_from(MAX_ROWS_PER_ENVELOPE + 1).expect("row bound should fit u32"),
    );

    let error = decode_envelope(&bytes).expect_err("decoder should reject oversized row counts");

    assert_eq!(error.kind(), ErrorKind::InvalidData);
}

#[test]
fn decoder_rejects_oversized_context_entry_counts_before_allocation() {
    let mut bytes = Vec::new();
    encode_header(&mut bytes, EnvelopeHeader::new(PROTOCOL_VERSION, 0));
    encode_u8(&mut bytes, 1);
    encode_u32(&mut bytes, 1);
    encode_u64(&mut bytes, 0);
    encode_u32(
        &mut bytes,
        u32::try_from(MAX_CONTEXT_ENTRIES_PER_CONTEXT + 1)
            .expect("context entry bound should fit u32"),
    );

    let error =
        decode_envelope(&bytes).expect_err("decoder should reject oversized context entries");

    assert_eq!(error.kind(), ErrorKind::InvalidData);
}

#[test]
fn counter_samples_match_their_golden_fixture() {
    let envelope = OwnedSessionEnvelope::new(
        EnvelopeHeader::new(PROTOCOL_VERSION, 1),
        OwnedSessionPayload::Events(vec![OwnedTimedObservation::new(
            9,
            2,
            1,
            OwnedObservation::CounterSample {
                desc_id: CounterDescId::new(3),
                name: String::from("ops"),
                unit: footfall_core::Unit::Count,
                context_id: ContextId::new(4),
                kind: CounterKind::Gauge,
                value: 7,
            },
        )]),
    );
    let encoded = encode_owned_envelope(&envelope).expect("encoding should succeed");
    // Event row: timestamp, sequence, thread, then tag 8, descriptor id,
    // name, unit (count), context id, kind (gauge), and value.
    assert_eq!(
        to_hex(&encoded),
        "0100000001000000000000000501000000090000000000000002000000000000000100000000\
         000000080300000000000000030000006f70730104000000000000000107000000000000\
         00"
    );
    assert_eq!(
        decode_envelope(&encoded).expect("decoding should succeed"),
        envelope
    );
}

#[test]
fn scope_token_observation_round_trips() {
    let envelope = OwnedSessionEnvelope::new(
        EnvelopeHeader::new(PROTOCOL_VERSION, 1),
        OwnedSessionPayload::Events(vec![OwnedTimedObservation::new(
            3,
            0,
            4,
            OwnedObservation::ScopeEnd {
                token: ScopeToken::new(11),
            },
        )]),
    );
    let decoded =
        decode_envelope(&encode_owned_envelope(&envelope).expect("encoding should succeed"))
            .expect("decoding should succeed");
    assert_eq!(decoded, envelope);
}

#[test]
fn decoder_rejects_incompatible_codec_versions() {
    let envelope = OwnedSessionEnvelope::new(
        EnvelopeHeader::new(PROTOCOL_VERSION, 0),
        OwnedSessionPayload::SnapshotEnd(crate::Drops::new()),
    );
    let encoded = encode_owned_envelope(&envelope).expect("encoding should succeed");

    // The frame starts with little-endian u16 major and u16 minor.
    let mut different_major = encoded.clone();
    different_major[0..2].copy_from_slice(&(super::CODEC_VERSION.major() + 1).to_le_bytes());
    let error = decode_envelope(&different_major)
        .expect_err("decoder should reject a different major version");
    assert_eq!(error.kind(), ErrorKind::InvalidData);

    let mut newer_minor = encoded.clone();
    newer_minor[2..4].copy_from_slice(&(super::CODEC_VERSION.minor() + 1).to_le_bytes());
    let error =
        decode_envelope(&newer_minor).expect_err("decoder should reject a newer minor version");
    assert_eq!(error.kind(), ErrorKind::InvalidData);

    decode_envelope(&encoded).expect("decoder should accept the supported version");
}
