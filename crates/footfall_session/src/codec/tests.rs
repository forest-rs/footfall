// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Golden fixtures and decoder limits.

use alloc::{boxed::Box, format, string::String, vec, vec::Vec};

use footfall_core::{
    Context, ContextId, CounterDescId, EventDescId, FlowId, FlowPhase, LabelKey, MarkerKind,
    MarkerPhase, ScopeToken, SourceLocation,
};
use footfall_runtime::{CounterKind, Snapshot, SnapshotMetadata, ThreadSnapshot};
use std::io::ErrorKind;

use super::{
    Cursor, MAX_CONTEXT_ENTRIES_PER_CONTEXT, MAX_ROWS_PER_ENVELOPE, MAX_SESSION_ENVELOPE_BYTES,
    MAX_SESSION_STRING_BYTES, VarintDecoder, decode_envelope, decode_u64, encode_envelope,
    encode_header, encode_i64, encode_owned_envelope, encode_u8, encode_u32, encode_u64,
    encode_varint, varint_len,
};
use crate::{
    Chunking, Envelope, EnvelopeHeader, OwnedArgEntry, OwnedClockTicks, OwnedContextEntry,
    OwnedContextSnapshot, OwnedCounterSnapshot, OwnedLabelValue, OwnedObservation,
    OwnedSessionEnvelope, OwnedSessionPayload, OwnedSnapshotSummary, OwnedSourceLocation,
    OwnedTimedObservation, OwnedTimelineWork, PROTOCOL_VERSION, Payload, RowCounts,
    collect_snapshot,
};

static FRAME: LabelKey = LabelKey::new("frame");
static REASON: LabelKey = LabelKey::new("reason");
static RECORD_AT: SourceLocation = SourceLocation::new("record", "app.rs", 300);

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
    assert_eq!(to_hex(&encoded), "0100000002040103036f7073010100070e");

    // Timestamps and sequence numbers are deltas from the previous row, and a
    // descriptor's name, with a scope's source location, is written only the
    // first time its id appears.
    let events = OwnedSessionEnvelope::new(
        EnvelopeHeader::new(PROTOCOL_VERSION, 3),
        OwnedSessionPayload::Events(vec![
            OwnedTimedObservation::new(
                15,
                4,
                2,
                OwnedObservation::ScopeBegin {
                    desc_id: footfall_core::ScopeDescId::new(5),
                    name: String::from("record"),
                    location: Some(OwnedSourceLocation::new("record", "app.rs", 300)),
                    context_id: ContextId::new(1),
                    token: ScopeToken::new(9),
                    args: vec![OwnedArgEntry::new(
                        String::from("items"),
                        OwnedLabelValue::Unsigned(3),
                    )],
                },
            ),
            OwnedTimedObservation::new(
                16,
                5,
                2,
                OwnedObservation::ScopeBegin {
                    desc_id: footfall_core::ScopeDescId::new(5),
                    name: String::from("record"),
                    location: Some(OwnedSourceLocation::from(&RECORD_AT)),
                    context_id: ContextId::new(1),
                    token: ScopeToken::new(10),
                    args: Vec::new(),
                },
            ),
            OwnedTimedObservation::new(
                13,
                6,
                3,
                OwnedObservation::ScopeEnd {
                    token: ScopeToken::new(9),
                },
            ),
        ]),
    );
    // The first row's location is present (`01`), then its function
    // "record", file "app.rs", and line 300 (`ac02`); the second row, of the
    // same descriptor, carries neither name nor location.
    let expected = "010000000306031e08020005067265636f7264\
                    01067265636f7264066170702e7273ac02\
                    010901056974656d7300030202020005010a000502030109";
    let encoded = encode_owned_envelope(&events).expect("encoding should succeed");
    assert_eq!(to_hex(&encoded), expected);
    assert_eq!(
        decode_envelope(&encoded).expect("decoding should succeed"),
        events
    );

    // The production encoder for borrowed runtime snapshots, whose scope
    // begins refer to the descriptor's static location, writes the same.
    let begin = |token, args| footfall_runtime::Observation::ScopeBegin {
        desc_id: footfall_core::ScopeDescId::new(5),
        name: "record",
        location: Some(&RECORD_AT),
        context_id: ContextId::new(1),
        token: ScopeToken::new(token),
        args,
    };
    let borrowed = [
        footfall_runtime::TimedObservation::new(
            15,
            4,
            2,
            begin(9, footfall_core::Args::none().with("items", 3_u32)),
        ),
        footfall_runtime::TimedObservation::new(16, 5, 2, begin(10, footfall_core::Args::none())),
        footfall_runtime::TimedObservation::new(
            13,
            6,
            3,
            footfall_runtime::Observation::ScopeEnd {
                token: ScopeToken::new(9),
            },
        ),
    ];
    let encoded = encode_envelope(Envelope::new(
        EnvelopeHeader::new(PROTOCOL_VERSION, 3),
        Payload::Events(&borrowed),
    ))
    .expect("encoding should succeed");
    assert_eq!(to_hex(&encoded), expected);

    // A scope without a location writes one absent tag (`00`) after its name.
    let unlocated = OwnedSessionEnvelope::new(
        EnvelopeHeader::new(PROTOCOL_VERSION, 0),
        OwnedSessionPayload::Events(vec![OwnedTimedObservation::new(
            0,
            0,
            1,
            OwnedObservation::ScopeBegin {
                desc_id: footfall_core::ScopeDescId::new(1),
                name: String::from("record"),
                location: None,
                context_id: ContextId::new(0),
                token: ScopeToken::new(1),
                args: Vec::new(),
            },
        )]),
    );
    let encoded = encode_owned_envelope(&unlocated).expect("encoding should succeed");
    assert_eq!(
        to_hex(&encoded),
        "010000000006010000010001067265636f726400000100"
    );
    assert_eq!(
        decode_envelope(&encoded).expect("decoding should succeed"),
        unlocated
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
    let expected = "01000000010201010002056672616d65000706726561736f6e030a696e76616c69646174\
                    65";
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
        .with_wall_clock_anchor_nanos(9)
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
            crate::SnapshotKind::Full,
            20,
            crate::EventWindow::new(1, 3, 1),
            RowCounts::new(1, 2, 0, 0, 0, 1),
        )),
    );
    assert_eq!(
        to_hex(&encode_owned_envelope(&start).expect("encoding should succeed")),
        "0100000000000464656d6f00888e98a8c0e0808101000905302e312e3002104020000014\
         010301010200000001"
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
        "0100000001010203010672656e6465720400"
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
        "010000000207010200030400"
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
        .with_wall_clock_anchor_nanos(1_700_000_000_000_000)
        .with_producer_version("0.1.0")
        .with_capture(crate::CaptureSettings::new(
            footfall_core::InstrumentationLevel::Scoped,
            16,
            64,
            32,
        ));
    let snapshot = Snapshot::builder(metadata)
        .captured_at_nanos(20)
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

/// Markers carry the program's number as a `u64` one greater than it, or
/// `0` without one.
#[test]
fn markers_match_their_golden_fixture() {
    let marker = |at, sequence, phase, number| {
        OwnedTimedObservation::new(
            at,
            sequence,
            1,
            OwnedObservation::FrameMarker {
                kind: MarkerKind::Frame,
                phase,
                number,
                context_id: ContextId::new(0),
            },
        )
    };
    let envelope = OwnedSessionEnvelope::new(
        EnvelopeHeader::new(PROTOCOL_VERSION, 0),
        OwnedSessionPayload::Events(vec![
            marker(0, 0, MarkerPhase::Begin, None),
            marker(5, 1, MarkerPhase::End, Some(41)),
        ]),
    );
    let encoded = encode_owned_envelope(&envelope).expect("encoding should succeed");
    // Header, events tag and count; then per row: time and sequence
    // deltas, thread, tag `05`, kind, phase, number, context.
    assert_eq!(
        to_hex(&encoded),
        "01000000000602\
         0000010500000000\
         0a02010500012a00"
    );
    assert_eq!(
        decode_envelope(&encoded).expect("decoding should succeed"),
        envelope
    );
}

/// Clock readings carry the clock's id, its name the first time, the ticks,
/// and the tick period as nanoseconds per ticks.
#[test]
fn clock_readings_match_their_golden_fixture() {
    let host = |ticks| {
        OwnedClockTicks::new(
            footfall_core::ClockId::new(0),
            String::from("host"),
            ticks,
            footfall_core::TickPeriod::new(125, 3),
        )
    };
    let envelope = OwnedSessionEnvelope::new(
        EnvelopeHeader::new(PROTOCOL_VERSION, 0),
        OwnedSessionPayload::Events(vec![
            OwnedTimedObservation::new(
                0,
                0,
                1,
                OwnedObservation::ClockSync {
                    clock: host(24_000_000),
                },
            ),
            OwnedTimedObservation::new(
                9,
                1,
                1,
                OwnedObservation::ClockPair {
                    first: OwnedClockTicks::new(
                        footfall_core::ClockId::new(1),
                        String::from("gpu"),
                        7_000,
                        footfall_core::TickPeriod::NANOSECOND,
                    ),
                    second: host(24_000_240),
                },
            ),
        ]),
    );
    let encoded = encode_owned_envelope(&envelope).expect("encoding should succeed");
    // Sync: tag `09`, clock `00` named "host", 24,000,000 ticks, 125/3.
    // Pair: tag `0a`, clock `01` named "gpu", 7,000 ticks, 1/1; then the
    // host, whose name was already written, 24,000,240 ticks, 125/3.
    assert_eq!(
        to_hex(&encoded),
        "01000000000602\
         0000010900\
         04686f737480ecb80b7d03\
         1202010a0103677075d8360101\
         00f0edb80b7d03"
    );
    assert_eq!(
        decode_envelope(&encoded).expect("decoding should succeed"),
        envelope
    );
}

/// Timeline spans carry their timeline, scope, context, the clock reading
/// they began at, their length in ticks, frame, flow, and args.
#[test]
fn timeline_spans_match_their_golden_fixture() {
    let work = OwnedTimelineWork::new(
        footfall_core::TimelineId::new(0),
        String::from("gpu"),
        footfall_core::TimelineKind::Gpu,
        footfall_core::ScopeDescId::new(1),
        String::from("shadow"),
        ContextId::new(0),
        OwnedClockTicks::new(
            footfall_core::ClockId::new(2),
            String::from("ts"),
            1_000,
            footfall_core::TickPeriod::NANOSECOND,
        ),
        1_400,
    )
    .with_frame(Some(7))
    .with_flow(Some(FlowId::new(3)));
    let envelope = OwnedSessionEnvelope::new(
        EnvelopeHeader::new(PROTOCOL_VERSION, 0),
        OwnedSessionPayload::Events(vec![OwnedTimedObservation::new(
            0,
            0,
            1,
            OwnedObservation::TimelineSpan {
                span: Box::new(work),
            },
        )]),
    );
    let encoded = encode_owned_envelope(&envelope).expect("encoding should succeed");
    // Tag `0b`; timeline `00` "gpu", kind `00`; scope `01` "shadow", no
    // location; context `00`; clock `02` "ts" at 1,000 ticks of 1/1 ns;
    // 400 ticks long; frame 7 and flow 3, each written one more; no args.
    assert_eq!(
        to_hex(&encoded),
        "01000000000601\
         0000010b\
         000367707500\
         0106736861646f7700\
         00\
         02027473e8070101\
         9003\
         08\
         04\
         00"
    );
    assert_eq!(
        decode_envelope(&encoded).expect("decoding should succeed"),
        envelope
    );
}

/// Frame points carry the frame's number, the kind, and a tagged clock
/// reading when they have a time.
#[test]
fn frame_points_match_their_golden_fixture() {
    let envelope = OwnedSessionEnvelope::new(
        EnvelopeHeader::new(PROTOCOL_VERSION, 0),
        OwnedSessionPayload::Events(vec![
            OwnedTimedObservation::new(
                0,
                0,
                1,
                OwnedObservation::FramePoint {
                    number: 7,
                    kind: footfall_core::FramePointKind::Target,
                    time: Some(OwnedClockTicks::new(
                        footfall_core::ClockId::new(0),
                        String::from("host"),
                        1_000,
                        footfall_core::TickPeriod::NANOSECOND,
                    )),
                },
            ),
            OwnedTimedObservation::new(
                5,
                1,
                1,
                OwnedObservation::FramePoint {
                    number: 8,
                    kind: footfall_core::FramePointKind::Dropped,
                    time: None,
                },
            ),
        ]),
    );
    let encoded = encode_owned_envelope(&envelope).expect("encoding should succeed");
    // Tag `0c`; frame 7, target `01`, time `01`, clock `00` "host" at
    // 1,000 ticks of 1/1 ns; then frame 8, dropped `03`, no time `00`.
    assert_eq!(
        to_hex(&encoded),
        "01000000000602\
         0000010c070101\
         0004686f7374e8070101\
         0a02010c080300"
    );
    assert_eq!(
        decode_envelope(&encoded).expect("decoding should succeed"),
        envelope
    );
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
                number: None,
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
    // Event row: timestamp and sequence deltas, thread, then tag 8,
    // descriptor id, name, unit (count), context id, kind (gauge), and value.
    assert_eq!(to_hex(&encoded), "010000000106011204010803036f707301040107");
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

#[test]
fn integers_are_leb128_and_reject_overflow() {
    for value in [0, 127, 128, 300, u64::MAX] {
        let mut bytes = Vec::new();
        encode_varint(&mut bytes, value);
        assert_eq!(bytes.len(), varint_len(value));
        let mut cursor = Cursor::new(&bytes);
        assert_eq!(decode_u64(&mut cursor).expect("a valid integer"), value);
        assert!(cursor.is_exhausted());
    }
    let mut bytes = Vec::new();
    encode_varint(&mut bytes, 300);
    assert_eq!(to_hex(&bytes), "ac02");
    let mut bytes = Vec::new();
    encode_i64(&mut bytes, -3);
    assert_eq!(
        to_hex(&bytes),
        "05",
        "zigzag puts small negatives in one byte"
    );

    // Eleven bytes, and a tenth byte carrying more than the top bit, are
    // both too long for a u64.
    for bad in [
        &[0xff_u8; 11][..],
        &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x02],
    ] {
        let mut decoder = VarintDecoder::new();
        let error = bad
            .iter()
            .find_map(|&byte| decoder.push(byte).err())
            .expect("too long");
        assert_eq!(error.kind(), ErrorKind::InvalidData);
    }
}

/// Events are the bulk of a subscription's traffic; a typical scope's
/// begin and end take a few bytes each, not the dozens fixed-width integers
/// and repeated names or source locations would cost.
#[test]
fn typical_events_stay_compact() {
    let location = |scope: u64| {
        OwnedSourceLocation::new(
            format!("app::render::scope_{scope}"),
            "src/render.rs",
            u32::try_from(100 * (scope + 1)).expect("a small line"),
        )
    };
    let encode = |located: bool| {
        let mut events = Vec::new();
        for index in 0..1000_u64 {
            let at = 1_000_000 + index * 250;
            events.push(OwnedTimedObservation::new(
                at,
                2 * index,
                1 + index % 4,
                OwnedObservation::ScopeBegin {
                    desc_id: footfall_core::ScopeDescId::new(1 + index % 8),
                    name: format!("scope_{}", index % 8),
                    location: located.then(|| location(index % 8)),
                    context_id: ContextId::new(100 + index % 16),
                    token: ScopeToken::new(10_000 + index),
                    args: Vec::new(),
                },
            ));
            events.push(OwnedTimedObservation::new(
                at + 120,
                2 * index + 1,
                1 + index % 4,
                OwnedObservation::ScopeEnd {
                    token: ScopeToken::new(10_000 + index),
                },
            ));
        }
        let envelope = OwnedSessionEnvelope::new(
            EnvelopeHeader::new(PROTOCOL_VERSION, 0),
            OwnedSessionPayload::Events(events),
        );
        let encoded = encode_owned_envelope(&envelope).expect("encoding should succeed");
        assert_eq!(
            decode_envelope(&encoded).expect("decoding should succeed"),
            envelope
        );
        encoded.len()
    };
    let (unlocated, located) = (encode(false), encode(true));
    for bytes in [unlocated, located] {
        let per_event = bytes / 2000;
        assert!(per_event <= 8, "{per_event} bytes per event");
    }
    // Each of the eight scopes' locations is written once, with its first
    // begin, in place of an absent tag: no event pays for it again.
    let once: usize = (0..8)
        .map(|scope| {
            let location = location(scope);
            location.function().len()
                + 1
                + location.file().len()
                + 1
                + varint_len(u64::from(location.line()))
        })
        .sum();
    assert_eq!(located - unlocated, once);
}
