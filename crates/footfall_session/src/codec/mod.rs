// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Byte encoding of session envelopes.
//!
//! This is the encoding the [wire-format document] specifies, shared by every
//! transport and by capture files. Encoding fails with
//! [`io::ErrorKind::InvalidInput`] for a value this protocol version has no
//! encoding for or that exceeds a limit; decoding fails with
//! [`io::ErrorKind::InvalidData`] for anything malformed, truncated, too
//! large, or of an unsupported version.
//!
//! [wire-format document]: https://github.com/forest-rs/footfall/blob/main/docs/session-wire-format.md

use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};
use std::collections::{HashMap, HashSet};
use std::io;

use footfall_core::{
    ContextId, CounterDescId, EventDescId, FlowId, FlowPhase, InstrumentationLevel, MarkerKind,
    MarkerPhase, ResourceId, ResourceKindId, ScopeDescId, ScopeToken, Unit,
};
use footfall_runtime::CounterKind;

use crate::{
    CaptureSettings, Drops, Envelope, EnvelopeHeader, EventWindow, OwnedArgEntry,
    OwnedContextEntry, OwnedContextSnapshot, OwnedCounterSnapshot, OwnedHistogramSnapshot,
    OwnedLabelValue, OwnedObservation, OwnedSessionEnvelope, OwnedSessionPayload,
    OwnedSnapshotSummary, OwnedTimedObservation, PROTOCOL_VERSION, Payload, ProtocolVersion,
    RowCounts, SnapshotKind, SnapshotMetadata, ThreadSnapshot,
};

/// Maximum UTF-8 string payload accepted by the codec.
pub const MAX_SESSION_STRING_BYTES: usize = 64 * 1024;
/// Maximum encoded envelope accepted by the codec.
pub const MAX_SESSION_ENVELOPE_BYTES: usize = 16 * 1024 * 1024;
/// Maximum rows accepted in one encoded envelope payload.
pub const MAX_ROWS_PER_ENVELOPE: usize = 65_536;
/// Maximum context labels accepted for one encoded context row.
pub const MAX_CONTEXT_ENTRIES_PER_CONTEXT: usize = footfall_core::CONTEXT_INLINE_CAPACITY;
/// Maximum per-instance arguments accepted for one encoded observation.
pub const MAX_ARGS_PER_OBSERVATION: usize = footfall_core::ARGS_INLINE_CAPACITY;

/// Newest protocol version this codec decodes.
const CODEC_VERSION: ProtocolVersion = PROTOCOL_VERSION;

#[cfg(test)]
mod tests;

// Payload tags, numbered in publication order.
pub(crate) const TAG_SNAPSHOT_START: u8 = 0;
pub(crate) const TAG_THREADS: u8 = 1;
pub(crate) const TAG_CONTEXTS: u8 = 2;
pub(crate) const TAG_RETIRED_CONTEXTS: u8 = 3;
pub(crate) const TAG_COUNTERS: u8 = 4;
pub(crate) const TAG_HISTOGRAMS: u8 = 5;
pub(crate) const TAG_EVENTS: u8 = 6;
pub(crate) const TAG_SNAPSHOT_END: u8 = 7;

/// Encodes one envelope, as published by [`crate::publish_snapshot`].
pub fn encode_envelope(envelope: Envelope<'_>) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    encode_header(&mut bytes, envelope.header());
    match envelope.payload() {
        Payload::SnapshotStart(summary) => {
            encode_u8(&mut bytes, TAG_SNAPSHOT_START);
            encode_snapshot_summary(
                &mut bytes,
                summary.metadata(),
                summary.kind(),
                summary.captured_at_micros(),
                summary.window(),
                summary.counts(),
            )?;
        }
        Payload::Threads(threads) => {
            encode_u8(&mut bytes, TAG_THREADS);
            encode_threads(&mut bytes, threads)?;
        }
        Payload::Contexts(contexts) => {
            encode_u8(&mut bytes, TAG_CONTEXTS);
            encode_borrowed_contexts(&mut bytes, contexts)?;
        }
        Payload::RetiredContexts(ids) => {
            encode_u8(&mut bytes, TAG_RETIRED_CONTEXTS);
            encode_context_ids(&mut bytes, ids)?;
        }
        Payload::Counters(counters) => {
            encode_u8(&mut bytes, TAG_COUNTERS);
            encode_borrowed_counters(&mut bytes, counters)?;
        }
        Payload::Histograms(histograms) => {
            encode_u8(&mut bytes, TAG_HISTOGRAMS);
            encode_borrowed_histograms(&mut bytes, histograms)?;
        }
        Payload::Events(events) => {
            encode_u8(&mut bytes, TAG_EVENTS);
            encode_borrowed_events(&mut bytes, events)?;
        }
        Payload::SnapshotEnd(drops) => {
            encode_u8(&mut bytes, TAG_SNAPSHOT_END);
            encode_drops(&mut bytes, drops);
        }
    }
    Ok(bytes)
}

/// Encodes one owned envelope, such as one received earlier.
pub fn encode_owned_envelope(envelope: &OwnedSessionEnvelope) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    encode_header(&mut bytes, envelope.header());
    match envelope.payload() {
        OwnedSessionPayload::SnapshotStart(summary) => {
            encode_u8(&mut bytes, TAG_SNAPSHOT_START);
            encode_snapshot_summary(
                &mut bytes,
                summary.metadata(),
                summary.kind(),
                summary.captured_at_micros(),
                summary.window(),
                summary.counts(),
            )?;
        }
        OwnedSessionPayload::Threads(threads) => {
            encode_u8(&mut bytes, TAG_THREADS);
            encode_threads(&mut bytes, threads)?;
        }
        OwnedSessionPayload::Contexts(contexts) => {
            encode_u8(&mut bytes, TAG_CONTEXTS);
            encode_contexts(&mut bytes, contexts)?;
        }
        OwnedSessionPayload::RetiredContexts(ids) => {
            encode_u8(&mut bytes, TAG_RETIRED_CONTEXTS);
            encode_context_ids(&mut bytes, ids)?;
        }
        OwnedSessionPayload::Counters(counters) => {
            encode_u8(&mut bytes, TAG_COUNTERS);
            encode_counters(&mut bytes, counters)?;
        }
        OwnedSessionPayload::Histograms(histograms) => {
            encode_u8(&mut bytes, TAG_HISTOGRAMS);
            encode_histograms(&mut bytes, histograms)?;
        }
        OwnedSessionPayload::Events(events) => {
            encode_u8(&mut bytes, TAG_EVENTS);
            encode_events(&mut bytes, events)?;
        }
        OwnedSessionPayload::SnapshotEnd(drops) => {
            encode_u8(&mut bytes, TAG_SNAPSHOT_END);
            encode_drops(&mut bytes, *drops);
        }
    }
    Ok(bytes)
}

/// Decodes one envelope, checking its protocol version.
pub fn decode_envelope(bytes: &[u8]) -> io::Result<OwnedSessionEnvelope> {
    validate_wire_len(
        bytes.len(),
        MAX_SESSION_ENVELOPE_BYTES,
        "session envelope frame",
    )?;
    let mut cursor = Cursor::new(bytes);
    let header = decode_header(&mut cursor)?;
    let payload = match decode_u8(&mut cursor)? {
        TAG_SNAPSHOT_START => {
            OwnedSessionPayload::SnapshotStart(decode_snapshot_summary(&mut cursor)?)
        }
        TAG_THREADS => OwnedSessionPayload::Threads(decode_threads(&mut cursor)?),
        TAG_CONTEXTS => OwnedSessionPayload::Contexts(decode_contexts(&mut cursor)?),
        TAG_RETIRED_CONTEXTS => {
            OwnedSessionPayload::RetiredContexts(decode_context_ids(&mut cursor)?)
        }
        TAG_COUNTERS => OwnedSessionPayload::Counters(decode_counters(&mut cursor)?),
        TAG_HISTOGRAMS => OwnedSessionPayload::Histograms(decode_histograms(&mut cursor)?),
        TAG_EVENTS => OwnedSessionPayload::Events(decode_events(&mut cursor)?),
        TAG_SNAPSHOT_END => OwnedSessionPayload::SnapshotEnd(decode_drops(&mut cursor)?),
        tag => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown session payload tag: {tag}"),
            ));
        }
    };
    if !cursor.is_exhausted() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "trailing bytes after session envelope",
        ));
    }
    Ok(OwnedSessionEnvelope::new(header, payload))
}

pub(crate) fn encode_header(bytes: &mut Vec<u8>, header: EnvelopeHeader) {
    encode_u16(bytes, header.protocol_version().major());
    encode_u16(bytes, header.protocol_version().minor());
    encode_u64(bytes, header.chunk_index());
}

pub(crate) fn decode_header(cursor: &mut Cursor<'_>) -> io::Result<EnvelopeHeader> {
    let protocol_version = ProtocolVersion::new(decode_u16(cursor)?, decode_u16(cursor)?);
    if !CODEC_VERSION.can_decode(protocol_version) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "unsupported session codec version {}.{}",
                protocol_version.major(),
                protocol_version.minor()
            ),
        ));
    }
    Ok(EnvelopeHeader::new(protocol_version, decode_u64(cursor)?))
}

pub(crate) fn encode_snapshot_summary(
    bytes: &mut Vec<u8>,
    metadata: &SnapshotMetadata,
    kind: SnapshotKind,
    captured_at_micros: u64,
    window: EventWindow,
    counts: RowCounts,
) -> io::Result<()> {
    encode_snapshot_metadata(bytes, metadata)?;
    encode_u8(
        bytes,
        match kind {
            SnapshotKind::Full => 0,
            SnapshotKind::Changes => 1,
        },
    );
    encode_u64(bytes, captured_at_micros);
    encode_u64(bytes, window.since());
    encode_u64(bytes, window.next());
    encode_u64(bytes, window.missed());
    encode_usize(bytes, counts.threads())?;
    encode_usize(bytes, counts.contexts())?;
    encode_usize(bytes, counts.retired_contexts())?;
    encode_usize(bytes, counts.counters())?;
    encode_usize(bytes, counts.histograms())?;
    encode_usize(bytes, counts.events())?;
    Ok(())
}

pub(crate) fn decode_snapshot_summary(cursor: &mut Cursor<'_>) -> io::Result<OwnedSnapshotSummary> {
    let metadata = decode_snapshot_metadata(cursor)?;
    let kind = match decode_u8(cursor)? {
        0 => SnapshotKind::Full,
        1 => SnapshotKind::Changes,
        tag => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown snapshot kind tag: {tag}"),
            ));
        }
    };
    let captured_at_micros = decode_u64(cursor)?;
    let window = EventWindow::new(
        decode_u64(cursor)?,
        decode_u64(cursor)?,
        decode_u64(cursor)?,
    );
    let counts = RowCounts::new(
        decode_usize(cursor)?,
        decode_usize(cursor)?,
        decode_usize(cursor)?,
        decode_usize(cursor)?,
        decode_usize(cursor)?,
        decode_usize(cursor)?,
    );
    Ok(OwnedSnapshotSummary::new(
        metadata,
        kind,
        captured_at_micros,
        window,
        counts,
    ))
}

fn decode_bool(cursor: &mut Cursor<'_>, what: &str) -> io::Result<bool> {
    match decode_u8(cursor)? {
        0 => Ok(false),
        1 => Ok(true),
        tag => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid {what}: {tag}"),
        )),
    }
}

fn encode_context_ids(bytes: &mut Vec<u8>, ids: &[ContextId]) -> io::Result<()> {
    encode_len(bytes, ids.len())?;
    for id in ids {
        encode_u64(bytes, id.get());
    }
    Ok(())
}

fn decode_context_ids(cursor: &mut Cursor<'_>) -> io::Result<Vec<ContextId>> {
    let len = decode_bounded_len(cursor, MAX_ROWS_PER_ENVELOPE, "retired context ids")?;
    let mut ids = Vec::with_capacity(len);
    for _ in 0..len {
        ids.push(ContextId::new(decode_u64(cursor)?));
    }
    Ok(ids)
}

pub(crate) fn encode_snapshot_metadata(
    bytes: &mut Vec<u8>,
    metadata: &SnapshotMetadata,
) -> io::Result<()> {
    encode_string(bytes, metadata.process_name())?;
    encode_optional_string(bytes, metadata.session_name())?;
    encode_u64(bytes, metadata.instance_id());
    encode_u32(bytes, metadata.process_id());
    encode_u64(bytes, metadata.wall_clock_anchor_micros());
    encode_string(bytes, metadata.producer_version())?;
    let capture = metadata.capture();
    encode_u8(bytes, encode_level(capture.level())?);
    encode_u64(bytes, usize_to_u64(capture.counter_capacity()));
    encode_u64(bytes, usize_to_u64(capture.event_capacity()));
    encode_u64(bytes, usize_to_u64(capture.context_capacity()));
    encode_u8(
        bytes,
        u8::from(capture.counter_samples()) | (u8::from(capture.compiled_out()) << 1),
    );
    Ok(())
}

pub(crate) fn decode_snapshot_metadata(cursor: &mut Cursor<'_>) -> io::Result<SnapshotMetadata> {
    let process_name = decode_string(cursor)?;
    let session_name = decode_optional_string(cursor, "session name")?;
    let instance_id = decode_u64(cursor)?;
    let process_id = decode_u32(cursor)?;
    let wall_clock_anchor_micros = decode_u64(cursor)?;
    let producer_version = decode_string(cursor)?;
    let capture = CaptureSettings::new(
        decode_level(decode_u8(cursor)?)?,
        decode_u64_as_usize(cursor)?,
        decode_u64_as_usize(cursor)?,
        decode_u64_as_usize(cursor)?,
    );
    let flags = decode_u8(cursor)?;
    if flags & !0b11 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unknown capture flags: {flags:#04x}"),
        ));
    }
    let capture = capture
        .with_counter_samples(flags & 0b01 != 0)
        .with_compiled_out(flags & 0b10 != 0);
    let metadata = SnapshotMetadata::new(process_name)
        .with_instance_id(instance_id)
        .with_process_id(process_id)
        .with_wall_clock_anchor_micros(wall_clock_anchor_micros)
        .with_producer_version(producer_version)
        .with_capture(capture);
    Ok(match session_name {
        Some(session_name) => metadata.with_session_name(session_name),
        None => metadata,
    })
}

pub(crate) fn encode_threads(bytes: &mut Vec<u8>, threads: &[ThreadSnapshot]) -> io::Result<()> {
    encode_len(bytes, threads.len())?;
    for thread in threads {
        encode_u64(bytes, thread.id());
        encode_optional_string(bytes, thread.name())?;
    }
    Ok(())
}

pub(crate) fn decode_threads(cursor: &mut Cursor<'_>) -> io::Result<Vec<ThreadSnapshot>> {
    let len = decode_bounded_len(cursor, MAX_ROWS_PER_ENVELOPE, "thread rows")?;
    let mut threads = Vec::with_capacity(len);
    for _ in 0..len {
        let id = decode_u64(cursor)?;
        threads.push(ThreadSnapshot::new(
            id,
            decode_optional_string(cursor, "thread name")?,
        ));
    }
    Ok(threads)
}

pub(crate) fn encode_optional_string(bytes: &mut Vec<u8>, value: Option<&str>) -> io::Result<()> {
    match value {
        Some(value) => {
            encode_u8(bytes, 1);
            encode_string(bytes, value)
        }
        None => {
            encode_u8(bytes, 0);
            Ok(())
        }
    }
}

pub(crate) fn decode_optional_string(
    cursor: &mut Cursor<'_>,
    what: &str,
) -> io::Result<Option<String>> {
    match decode_u8(cursor)? {
        0 => Ok(None),
        1 => Ok(Some(decode_string(cursor)?)),
        tag => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid {what} presence tag: {tag}"),
        )),
    }
}

pub(crate) fn encode_level(level: InstrumentationLevel) -> io::Result<u8> {
    match level {
        InstrumentationLevel::Off => Ok(0),
        InstrumentationLevel::Counters => Ok(1),
        InstrumentationLevel::Scoped => Ok(2),
        InstrumentationLevel::Trace => Ok(3),
        _ => Err(unsupported("instrumentation level")),
    }
}

pub(crate) fn decode_level(tag: u8) -> io::Result<InstrumentationLevel> {
    match tag {
        0 => Ok(InstrumentationLevel::Off),
        1 => Ok(InstrumentationLevel::Counters),
        2 => Ok(InstrumentationLevel::Scoped),
        3 => Ok(InstrumentationLevel::Trace),
        tag => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unknown instrumentation level tag: {tag}"),
        )),
    }
}

pub(crate) fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

pub(crate) fn decode_u64_as_usize(cursor: &mut Cursor<'_>) -> io::Result<usize> {
    usize::try_from(decode_u64(cursor)?)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "u64 value does not fit usize"))
}

pub(crate) fn encode_contexts(
    bytes: &mut Vec<u8>,
    contexts: &[OwnedContextSnapshot],
) -> io::Result<()> {
    encode_len(bytes, contexts.len())?;
    for context in contexts {
        encode_u64(bytes, context.id().get());
        encode_u8(bytes, u8::from(context.is_released()));
        encode_len(bytes, context.entries().len())?;
        for entry in context.entries() {
            encode_string(bytes, entry.key())?;
            encode_label_value(bytes, entry.value())?;
        }
    }
    Ok(())
}

pub(crate) fn encode_borrowed_contexts(
    bytes: &mut Vec<u8>,
    contexts: &[footfall_runtime::ContextSnapshot],
) -> io::Result<()> {
    encode_len(bytes, contexts.len())?;
    for context in contexts {
        encode_u64(bytes, context.id().get());
        encode_u8(bytes, u8::from(context.is_released()));
        encode_len(bytes, context.context().len())?;
        for entry in context.context().iter() {
            encode_string(bytes, entry.key().name())?;
            encode_borrowed_label_value(bytes, entry.value())?;
        }
    }
    Ok(())
}

pub(crate) fn decode_contexts(cursor: &mut Cursor<'_>) -> io::Result<Vec<OwnedContextSnapshot>> {
    let len = decode_bounded_len(cursor, MAX_ROWS_PER_ENVELOPE, "context rows")?;
    let mut contexts = Vec::with_capacity(len);
    for _ in 0..len {
        let id = ContextId::new(decode_u64(cursor)?);
        let released = decode_bool(cursor, "context released flag")?;
        let entry_len =
            decode_bounded_len(cursor, MAX_CONTEXT_ENTRIES_PER_CONTEXT, "context entries")?;
        let mut entries = Vec::with_capacity(entry_len);
        for _ in 0..entry_len {
            entries.push(OwnedContextEntry::new(
                decode_string(cursor)?,
                decode_label_value(cursor)?,
            ));
        }
        contexts.push(OwnedContextSnapshot::new(id, entries).with_released(released));
    }
    Ok(contexts)
}

pub(crate) fn encode_borrowed_label_value(
    bytes: &mut Vec<u8>,
    value: footfall_core::LabelValue,
) -> io::Result<()> {
    match value {
        footfall_core::LabelValue::Unsigned(value) => {
            encode_u8(bytes, 0);
            encode_u64(bytes, value);
        }
        footfall_core::LabelValue::Signed(value) => {
            encode_u8(bytes, 1);
            encode_i64(bytes, value);
        }
        footfall_core::LabelValue::Bool(value) => {
            encode_u8(bytes, 2);
            encode_u8(bytes, u8::from(value));
        }
        footfall_core::LabelValue::StaticStr(value) => {
            encode_u8(bytes, 3);
            encode_string(bytes, value)?;
        }
        _ => return Err(unsupported("label value")),
    }
    Ok(())
}

pub(crate) fn encode_counters(
    bytes: &mut Vec<u8>,
    counters: &[OwnedCounterSnapshot],
) -> io::Result<()> {
    encode_len(bytes, counters.len())?;
    let mut names = NameWriter::default();
    for counter in counters {
        names.encode(bytes, counter.desc_id().get(), counter.name())?;
        encode_u8(bytes, encode_unit(counter.unit())?);
        encode_u64(bytes, counter.context_id().get());
        encode_u8(bytes, encode_counter_kind(counter.kind())?);
        encode_u64(bytes, counter.value());
        encode_u64(bytes, counter.updated_at_micros());
    }
    Ok(())
}

pub(crate) fn encode_borrowed_counters(
    bytes: &mut Vec<u8>,
    counters: &[footfall_runtime::CounterSnapshot],
) -> io::Result<()> {
    encode_len(bytes, counters.len())?;
    let mut names = NameWriter::default();
    for counter in counters {
        names.encode(bytes, counter.desc_id().get(), counter.name())?;
        encode_u8(bytes, encode_unit(counter.unit())?);
        encode_u64(bytes, counter.context_id().get());
        encode_u8(bytes, encode_counter_kind(counter.kind())?);
        encode_u64(bytes, counter.value());
        encode_u64(bytes, counter.updated_at_micros());
    }
    Ok(())
}

pub(crate) fn decode_counters(cursor: &mut Cursor<'_>) -> io::Result<Vec<OwnedCounterSnapshot>> {
    let len = decode_bounded_len(cursor, MAX_ROWS_PER_ENVELOPE, "counter rows")?;
    let mut counters = Vec::with_capacity(len);
    let mut names = NameReader::default();
    for _ in 0..len {
        counters.push(OwnedCounterSnapshot::new(
            CounterDescId::new(names.id(cursor)?),
            names.name(cursor)?,
            decode_unit(decode_u8(cursor)?)?,
            ContextId::new(decode_u64(cursor)?),
            decode_counter_kind(decode_u8(cursor)?)?,
            decode_u64(cursor)?,
            decode_u64(cursor)?,
        ));
    }
    Ok(counters)
}

pub(crate) fn encode_histograms(
    bytes: &mut Vec<u8>,
    histograms: &[OwnedHistogramSnapshot],
) -> io::Result<()> {
    encode_len(bytes, histograms.len())?;
    let mut names = NameWriter::default();
    for histogram in histograms {
        names.encode(bytes, histogram.desc_id().get(), histogram.name())?;
        encode_u8(bytes, encode_unit(histogram.unit())?);
        encode_u64(bytes, histogram.context_id().get());
        encode_histogram_body(
            bytes,
            histogram.count(),
            histogram.sum(),
            histogram.min(),
            histogram.max(),
            histogram.buckets(),
            histogram.updated_at_micros(),
        );
    }
    Ok(())
}

pub(crate) fn encode_borrowed_histograms(
    bytes: &mut Vec<u8>,
    histograms: &[footfall_runtime::HistogramSnapshot],
) -> io::Result<()> {
    encode_len(bytes, histograms.len())?;
    let mut names = NameWriter::default();
    for histogram in histograms {
        names.encode(bytes, histogram.desc_id().get(), histogram.name())?;
        encode_u8(bytes, encode_unit(histogram.unit())?);
        encode_u64(bytes, histogram.context_id().get());
        encode_histogram_body(
            bytes,
            histogram.count(),
            histogram.sum(),
            histogram.min(),
            histogram.max(),
            histogram.buckets(),
            histogram.updated_at_micros(),
        );
    }
    Ok(())
}

pub(crate) fn encode_histogram_body(
    bytes: &mut Vec<u8>,
    count: u64,
    sum: u64,
    min: u64,
    max: u64,
    buckets: &[u64; footfall_runtime::HISTOGRAM_BUCKET_COUNT],
    updated_at_micros: u64,
) {
    encode_u64(bytes, count);
    encode_u64(bytes, sum);
    encode_u64(bytes, min);
    encode_u64(bytes, max);
    for bucket in buckets {
        encode_u64(bytes, *bucket);
    }
    encode_u64(bytes, updated_at_micros);
}

pub(crate) fn decode_histograms(
    cursor: &mut Cursor<'_>,
) -> io::Result<Vec<OwnedHistogramSnapshot>> {
    let len = decode_bounded_len(cursor, MAX_ROWS_PER_ENVELOPE, "histogram rows")?;
    let mut histograms = Vec::with_capacity(len);
    let mut names = NameReader::default();
    for _ in 0..len {
        let desc_id = CounterDescId::new(names.id(cursor)?);
        let name = names.name(cursor)?;
        let unit = decode_unit(decode_u8(cursor)?)?;
        let context_id = ContextId::new(decode_u64(cursor)?);
        let count = decode_u64(cursor)?;
        let sum = decode_u64(cursor)?;
        let min = decode_u64(cursor)?;
        let max = decode_u64(cursor)?;
        let mut buckets = [0_u64; footfall_runtime::HISTOGRAM_BUCKET_COUNT];
        for bucket in &mut buckets {
            *bucket = decode_u64(cursor)?;
        }
        histograms.push(OwnedHistogramSnapshot::new(
            desc_id,
            name,
            unit,
            context_id,
            count,
            sum,
            min,
            max,
            buckets,
            decode_u64(cursor)?,
        ));
    }
    Ok(histograms)
}

pub(crate) fn encode_events(
    bytes: &mut Vec<u8>,
    events: &[OwnedTimedObservation],
) -> io::Result<()> {
    encode_len(bytes, events.len())?;
    // Timestamps and sequence numbers are written as differences from the
    // previous row's, which are small.
    let mut names = NameWriter::default();
    let (mut at_micros, mut sequence) = (0, 0);
    for event in events {
        encode_delta(bytes, at_micros, event.at_micros());
        encode_delta(bytes, sequence, event.sequence());
        encode_u64(bytes, event.thread_id());
        encode_observation(bytes, event.observation(), &mut names)?;
        (at_micros, sequence) = (event.at_micros(), event.sequence());
    }
    Ok(())
}

pub(crate) fn encode_borrowed_events(
    bytes: &mut Vec<u8>,
    events: &[footfall_runtime::TimedObservation],
) -> io::Result<()> {
    encode_len(bytes, events.len())?;
    // Timestamps and sequence numbers are written as differences from the
    // previous row's, which are small.
    let mut names = NameWriter::default();
    let (mut at_micros, mut sequence) = (0, 0);
    for event in events {
        encode_delta(bytes, at_micros, event.at_micros());
        encode_delta(bytes, sequence, event.sequence());
        encode_u64(bytes, event.thread_id());
        encode_borrowed_observation(bytes, event.observation(), &mut names)?;
        (at_micros, sequence) = (event.at_micros(), event.sequence());
    }
    Ok(())
}

pub(crate) fn decode_events(cursor: &mut Cursor<'_>) -> io::Result<Vec<OwnedTimedObservation>> {
    let len = decode_bounded_len(cursor, MAX_ROWS_PER_ENVELOPE, "event rows")?;
    let mut events = Vec::with_capacity(len);
    let mut names = NameReader::default();
    let (mut at_micros, mut sequence) = (0, 0);
    for _ in 0..len {
        at_micros = decode_delta(cursor, at_micros)?;
        sequence = decode_delta(cursor, sequence)?;
        events.push(OwnedTimedObservation::new(
            at_micros,
            sequence,
            decode_u64(cursor)?,
            decode_observation(cursor, &mut names)?,
        ));
    }
    Ok(events)
}

pub(crate) fn encode_borrowed_observation(
    bytes: &mut Vec<u8>,
    observation: &footfall_runtime::Observation,
    names: &mut NameWriter,
) -> io::Result<()> {
    match observation {
        footfall_runtime::Observation::ScopeBegin {
            desc_id,
            name,
            context_id,
            token,
            args,
        } => {
            encode_u8(bytes, 0);
            names.encode(bytes, desc_id.get(), name)?;
            encode_u64(bytes, context_id.get());
            encode_u64(bytes, token.get());
            encode_borrowed_args(bytes, *args)?;
        }
        footfall_runtime::Observation::ScopeEnd { token } => {
            encode_u8(bytes, 1);
            encode_u64(bytes, token.get());
        }
        footfall_runtime::Observation::ResourceCreate {
            kind_id,
            kind_name,
            resource_id,
            context_id,
            size,
        } => {
            encode_u8(bytes, 2);
            names.encode(bytes, kind_id.get(), kind_name)?;
            encode_u64(bytes, resource_id.get());
            encode_u64(bytes, context_id.get());
            encode_u64(bytes, *size);
        }
        footfall_runtime::Observation::ResourceUpdate {
            kind_id,
            kind_name,
            resource_id,
            context_id,
            size,
        } => {
            encode_u8(bytes, 3);
            names.encode(bytes, kind_id.get(), kind_name)?;
            encode_u64(bytes, resource_id.get());
            encode_u64(bytes, context_id.get());
            encode_u64(bytes, *size);
        }
        footfall_runtime::Observation::ResourceDrop {
            kind_id,
            kind_name,
            resource_id,
            context_id,
        } => {
            encode_u8(bytes, 4);
            names.encode(bytes, kind_id.get(), kind_name)?;
            encode_u64(bytes, resource_id.get());
            encode_u64(bytes, context_id.get());
        }
        footfall_runtime::Observation::FrameMarker {
            kind,
            phase,
            context_id,
        } => {
            encode_u8(bytes, 5);
            encode_u8(bytes, encode_marker_kind(*kind)?);
            encode_u8(bytes, encode_marker_phase(*phase));
            encode_u64(bytes, context_id.get());
        }
        footfall_runtime::Observation::Flow {
            flow_id,
            phase,
            context_id,
        } => {
            encode_u8(bytes, 6);
            encode_u64(bytes, flow_id.get());
            encode_u8(bytes, encode_flow_phase(*phase));
            encode_u64(bytes, context_id.get());
        }
        footfall_runtime::Observation::Message {
            desc_id,
            name,
            context_id,
            text,
            args,
        } => {
            encode_u8(bytes, 7);
            names.encode(bytes, desc_id.get(), name)?;
            encode_u64(bytes, context_id.get());
            encode_string(bytes, text)?;
            encode_borrowed_args(bytes, *args)?;
        }
        footfall_runtime::Observation::CounterSample {
            desc_id,
            name,
            unit,
            context_id,
            kind,
            value,
        } => {
            encode_counter_sample(
                bytes,
                names,
                *desc_id,
                name,
                *unit,
                *context_id,
                *kind,
                *value,
            )?;
        }
        _ => return Err(unsupported("observation")),
    }
    Ok(())
}

pub(crate) fn encode_args(bytes: &mut Vec<u8>, args: &[OwnedArgEntry]) -> io::Result<()> {
    let count = u8::try_from(args.len().min(MAX_ARGS_PER_OBSERVATION))
        .expect("bounded args count fits into u8");
    encode_u8(bytes, count);
    for entry in args.iter().take(usize::from(count)) {
        encode_string(bytes, entry.key())?;
        encode_label_value(bytes, entry.value())?;
    }
    Ok(())
}

pub(crate) fn encode_borrowed_args(
    bytes: &mut Vec<u8>,
    args: footfall_core::Args,
) -> io::Result<()> {
    let count = u8::try_from(args.len().min(MAX_ARGS_PER_OBSERVATION))
        .expect("bounded args count fits into u8");
    encode_u8(bytes, count);
    for entry in args.iter().take(usize::from(count)) {
        encode_string(bytes, entry.key())?;
        encode_borrowed_label_value(bytes, entry.value())?;
    }
    Ok(())
}

pub(crate) fn decode_args(cursor: &mut Cursor<'_>) -> io::Result<Vec<OwnedArgEntry>> {
    let count = usize::from(decode_u8(cursor)?);
    if count > MAX_ARGS_PER_OBSERVATION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("too many observation args: {count}"),
        ));
    }
    let mut args = Vec::with_capacity(count);
    for _ in 0..count {
        args.push(OwnedArgEntry::new(
            decode_string(cursor)?,
            decode_label_value(cursor)?,
        ));
    }
    Ok(args)
}

pub(crate) fn encode_drops(bytes: &mut Vec<u8>, drops: Drops) {
    encode_u64(bytes, drops.refused_contexts());
    encode_u64(bytes, drops.unattributed());
    encode_u64(bytes, drops.updated_after_release());
    encode_u64(bytes, drops.counter_updates());
    encode_u64(bytes, drops.evicted_events());
    encode_u64(bytes, drops.retired_rows());
}

pub(crate) fn decode_drops(cursor: &mut Cursor<'_>) -> io::Result<Drops> {
    Ok(Drops::new()
        .with_refused_contexts(decode_u64(cursor)?)
        .with_unattributed(decode_u64(cursor)?)
        .with_updated_after_release(decode_u64(cursor)?)
        .with_counter_updates(decode_u64(cursor)?)
        .with_evicted_events(decode_u64(cursor)?)
        .with_retired_rows(decode_u64(cursor)?))
}

pub(crate) fn encode_label_value(bytes: &mut Vec<u8>, value: &OwnedLabelValue) -> io::Result<()> {
    match value {
        OwnedLabelValue::Unsigned(value) => {
            encode_u8(bytes, 0);
            encode_u64(bytes, *value);
        }
        OwnedLabelValue::Signed(value) => {
            encode_u8(bytes, 1);
            encode_i64(bytes, *value);
        }
        OwnedLabelValue::Bool(value) => {
            encode_u8(bytes, 2);
            encode_u8(bytes, u8::from(*value));
        }
        OwnedLabelValue::String(value) => {
            encode_u8(bytes, 3);
            encode_string(bytes, value)?;
        }
    }
    Ok(())
}

pub(crate) fn decode_label_value(cursor: &mut Cursor<'_>) -> io::Result<OwnedLabelValue> {
    match decode_u8(cursor)? {
        0 => Ok(OwnedLabelValue::Unsigned(decode_u64(cursor)?)),
        1 => Ok(OwnedLabelValue::Signed(decode_i64(cursor)?)),
        2 => Ok(OwnedLabelValue::Bool(decode_u8(cursor)? != 0)),
        3 => Ok(OwnedLabelValue::String(decode_string(cursor)?)),
        tag => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unknown label-value tag: {tag}"),
        )),
    }
}

pub(crate) fn encode_observation(
    bytes: &mut Vec<u8>,
    observation: &OwnedObservation,
    names: &mut NameWriter,
) -> io::Result<()> {
    match observation {
        OwnedObservation::ScopeBegin {
            desc_id,
            name,
            context_id,
            token,
            args,
        } => {
            encode_u8(bytes, 0);
            names.encode(bytes, desc_id.get(), name)?;
            encode_u64(bytes, context_id.get());
            encode_u64(bytes, token.get());
            encode_args(bytes, args)?;
        }
        OwnedObservation::ScopeEnd { token } => {
            encode_u8(bytes, 1);
            encode_u64(bytes, token.get());
        }
        OwnedObservation::ResourceCreate {
            kind_id,
            kind_name,
            resource_id,
            context_id,
            size,
        } => {
            encode_u8(bytes, 2);
            names.encode(bytes, kind_id.get(), kind_name)?;
            encode_u64(bytes, resource_id.get());
            encode_u64(bytes, context_id.get());
            encode_u64(bytes, *size);
        }
        OwnedObservation::ResourceUpdate {
            kind_id,
            kind_name,
            resource_id,
            context_id,
            size,
        } => {
            encode_u8(bytes, 3);
            names.encode(bytes, kind_id.get(), kind_name)?;
            encode_u64(bytes, resource_id.get());
            encode_u64(bytes, context_id.get());
            encode_u64(bytes, *size);
        }
        OwnedObservation::ResourceDrop {
            kind_id,
            kind_name,
            resource_id,
            context_id,
        } => {
            encode_u8(bytes, 4);
            names.encode(bytes, kind_id.get(), kind_name)?;
            encode_u64(bytes, resource_id.get());
            encode_u64(bytes, context_id.get());
        }
        OwnedObservation::FrameMarker {
            kind,
            phase,
            context_id,
        } => {
            encode_u8(bytes, 5);
            encode_u8(bytes, encode_marker_kind(*kind)?);
            encode_u8(bytes, encode_marker_phase(*phase));
            encode_u64(bytes, context_id.get());
        }
        OwnedObservation::Flow {
            flow_id,
            phase,
            context_id,
        } => {
            encode_u8(bytes, 6);
            encode_u64(bytes, flow_id.get());
            encode_u8(bytes, encode_flow_phase(*phase));
            encode_u64(bytes, context_id.get());
        }
        OwnedObservation::Message {
            desc_id,
            name,
            context_id,
            text,
            args,
        } => {
            encode_u8(bytes, 7);
            names.encode(bytes, desc_id.get(), name)?;
            encode_u64(bytes, context_id.get());
            encode_string(bytes, text)?;
            encode_args(bytes, args)?;
        }
        OwnedObservation::CounterSample {
            desc_id,
            name,
            unit,
            context_id,
            kind,
            value,
        } => {
            encode_counter_sample(
                bytes,
                names,
                *desc_id,
                name,
                *unit,
                *context_id,
                *kind,
                *value,
            )?;
        }
    }
    Ok(())
}

pub(crate) fn decode_observation(
    cursor: &mut Cursor<'_>,
    names: &mut NameReader,
) -> io::Result<OwnedObservation> {
    match decode_u8(cursor)? {
        0 => Ok(OwnedObservation::ScopeBegin {
            desc_id: ScopeDescId::new(names.id(cursor)?),
            name: names.name(cursor)?,
            context_id: ContextId::new(decode_u64(cursor)?),
            token: ScopeToken::new(decode_u64(cursor)?),
            args: decode_args(cursor)?,
        }),
        1 => Ok(OwnedObservation::ScopeEnd {
            token: ScopeToken::new(decode_u64(cursor)?),
        }),
        2 => Ok(OwnedObservation::ResourceCreate {
            kind_id: ResourceKindId::new(names.id(cursor)?),
            kind_name: names.name(cursor)?,
            resource_id: ResourceId::new(decode_u64(cursor)?),
            context_id: ContextId::new(decode_u64(cursor)?),
            size: decode_u64(cursor)?,
        }),
        3 => Ok(OwnedObservation::ResourceUpdate {
            kind_id: ResourceKindId::new(names.id(cursor)?),
            kind_name: names.name(cursor)?,
            resource_id: ResourceId::new(decode_u64(cursor)?),
            context_id: ContextId::new(decode_u64(cursor)?),
            size: decode_u64(cursor)?,
        }),
        4 => Ok(OwnedObservation::ResourceDrop {
            kind_id: ResourceKindId::new(names.id(cursor)?),
            kind_name: names.name(cursor)?,
            resource_id: ResourceId::new(decode_u64(cursor)?),
            context_id: ContextId::new(decode_u64(cursor)?),
        }),
        5 => Ok(OwnedObservation::FrameMarker {
            kind: decode_marker_kind(decode_u8(cursor)?)?,
            phase: decode_marker_phase(decode_u8(cursor)?)?,
            context_id: ContextId::new(decode_u64(cursor)?),
        }),
        6 => Ok(OwnedObservation::Flow {
            flow_id: FlowId::new(decode_u64(cursor)?),
            phase: decode_flow_phase(decode_u8(cursor)?)?,
            context_id: ContextId::new(decode_u64(cursor)?),
        }),
        7 => Ok(OwnedObservation::Message {
            desc_id: EventDescId::new(names.id(cursor)?),
            name: names.name(cursor)?,
            context_id: ContextId::new(decode_u64(cursor)?),
            text: decode_string(cursor)?,
            args: decode_args(cursor)?,
        }),
        8 => Ok(OwnedObservation::CounterSample {
            desc_id: CounterDescId::new(names.id(cursor)?),
            name: names.name(cursor)?,
            unit: decode_unit(decode_u8(cursor)?)?,
            context_id: ContextId::new(decode_u64(cursor)?),
            kind: decode_counter_kind(decode_u8(cursor)?)?,
            value: decode_u64(cursor)?,
        }),
        tag => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unknown observation tag: {tag}"),
        )),
    }
}

fn encode_counter_sample(
    bytes: &mut Vec<u8>,
    names: &mut NameWriter,
    desc_id: CounterDescId,
    name: &str,
    unit: Unit,
    context_id: ContextId,
    kind: CounterKind,
    value: u64,
) -> io::Result<()> {
    encode_u8(bytes, 8);
    names.encode(bytes, desc_id.get(), name)?;
    encode_u8(bytes, encode_unit(unit)?);
    encode_u64(bytes, context_id.get());
    encode_u8(bytes, encode_counter_kind(kind)?);
    encode_u64(bytes, value);
    Ok(())
}

pub(crate) fn encode_counter_kind(kind: CounterKind) -> io::Result<u8> {
    match kind {
        CounterKind::Counter => Ok(0),
        CounterKind::Gauge => Ok(1),
        _ => Err(unsupported("counter kind")),
    }
}

pub(crate) fn decode_counter_kind(tag: u8) -> io::Result<CounterKind> {
    match tag {
        0 => Ok(CounterKind::Counter),
        1 => Ok(CounterKind::Gauge),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unknown counter kind tag: {tag}"),
        )),
    }
}

pub(crate) fn encode_unit(unit: Unit) -> io::Result<u8> {
    match unit {
        Unit::Bytes => Ok(0),
        Unit::Count => Ok(1),
        Unit::Nanos => Ok(2),
        Unit::Micros => Ok(3),
        Unit::Ratio => Ok(4),
        Unit::Items => Ok(5),
        _ => Err(unsupported("unit")),
    }
}

pub(crate) fn decode_unit(tag: u8) -> io::Result<Unit> {
    match tag {
        0 => Ok(Unit::Bytes),
        1 => Ok(Unit::Count),
        2 => Ok(Unit::Nanos),
        3 => Ok(Unit::Micros),
        4 => Ok(Unit::Ratio),
        5 => Ok(Unit::Items),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unknown unit tag: {tag}"),
        )),
    }
}

pub(crate) fn encode_marker_kind(kind: MarkerKind) -> io::Result<u8> {
    match kind {
        MarkerKind::Frame => Ok(0),
        MarkerKind::Transaction => Ok(1),
        MarkerKind::Pass => Ok(2),
        _ => Err(unsupported("marker kind")),
    }
}

/// Error for a value this protocol version has no encoding for.
pub(crate) fn unsupported(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{what} has no encoding in this session protocol version"),
    )
}

pub(crate) fn decode_marker_kind(tag: u8) -> io::Result<MarkerKind> {
    match tag {
        0 => Ok(MarkerKind::Frame),
        1 => Ok(MarkerKind::Transaction),
        2 => Ok(MarkerKind::Pass),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unknown marker kind tag: {tag}"),
        )),
    }
}

pub(crate) fn encode_marker_phase(phase: MarkerPhase) -> u8 {
    match phase {
        MarkerPhase::Begin => 0,
        MarkerPhase::End => 1,
    }
}

pub(crate) fn decode_marker_phase(tag: u8) -> io::Result<MarkerPhase> {
    match tag {
        0 => Ok(MarkerPhase::Begin),
        1 => Ok(MarkerPhase::End),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unknown marker phase tag: {tag}"),
        )),
    }
}

pub(crate) fn encode_flow_phase(phase: FlowPhase) -> u8 {
    match phase {
        FlowPhase::Begin => 0,
        FlowPhase::Step => 1,
        FlowPhase::End => 2,
    }
}

pub(crate) fn decode_flow_phase(tag: u8) -> io::Result<FlowPhase> {
    match tag {
        0 => Ok(FlowPhase::Begin),
        1 => Ok(FlowPhase::Step),
        2 => Ok(FlowPhase::End),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unknown flow phase tag: {tag}"),
        )),
    }
}

pub(crate) fn encode_len(bytes: &mut Vec<u8>, len: usize) -> io::Result<()> {
    encode_u32(
        bytes,
        u32::try_from(len)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "length exceeds u32"))?,
    );
    Ok(())
}

pub(crate) fn encode_usize(bytes: &mut Vec<u8>, value: usize) -> io::Result<()> {
    encode_len(bytes, value)
}

pub(crate) fn decode_usize(cursor: &mut Cursor<'_>) -> io::Result<usize> {
    usize::try_from(decode_u32(cursor)?)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "u32 length does not fit usize"))
}

pub(crate) fn decode_bounded_len(
    cursor: &mut Cursor<'_>,
    max_len: usize,
    what: &'static str,
) -> io::Result<usize> {
    let len = decode_usize(cursor)?;
    validate_wire_len(len, max_len, what)?;
    if len > cursor.remaining() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!("{what} length exceeds remaining envelope bytes"),
        ));
    }
    Ok(len)
}

pub(crate) fn validate_wire_len(len: usize, max_len: usize, what: &'static str) -> io::Result<()> {
    if len <= max_len {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{what} length {len} exceeds maximum {max_len}"),
    ))
}

pub(crate) fn encode_string(bytes: &mut Vec<u8>, value: &str) -> io::Result<()> {
    validate_wire_len(value.len(), MAX_SESSION_STRING_BYTES, "session string")?;
    encode_len(bytes, value.len())?;
    bytes.extend_from_slice(value.as_bytes());
    Ok(())
}

pub(crate) fn decode_string(cursor: &mut Cursor<'_>) -> io::Result<String> {
    let len = decode_bounded_len(cursor, MAX_SESSION_STRING_BYTES, "session string")?;
    let bytes = cursor.read_exact(len)?;
    String::from_utf8(bytes.to_vec())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))
}

pub(crate) fn encode_u8(bytes: &mut Vec<u8>, value: u8) {
    bytes.push(value);
}

pub(crate) fn decode_u8(cursor: &mut Cursor<'_>) -> io::Result<u8> {
    Ok(cursor.read_exact(1)?[0])
}

pub(crate) fn encode_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

pub(crate) fn decode_u16(cursor: &mut Cursor<'_>) -> io::Result<u16> {
    let bytes = cursor.read_exact(2)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

pub(crate) fn encode_u32(bytes: &mut Vec<u8>, value: u32) {
    encode_u64(bytes, u64::from(value));
}

pub(crate) fn decode_u32(cursor: &mut Cursor<'_>) -> io::Result<u32> {
    u32::try_from(decode_u64(cursor)?)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "integer does not fit u32"))
}

pub(crate) fn encode_u64(bytes: &mut Vec<u8>, value: u64) {
    encode_varint(bytes, value);
}

pub(crate) fn decode_u64(cursor: &mut Cursor<'_>) -> io::Result<u64> {
    let mut decoder = VarintDecoder::new();
    loop {
        if let Some(value) = decoder.push(decode_u8(cursor)?)? {
            return Ok(value);
        }
    }
}

/// Appends `value` as unsigned LEB128, the integer encoding the session
/// protocol uses: seven bits per byte, least significant group first, with
/// the high bit set on every byte but the last.
pub fn encode_varint(bytes: &mut Vec<u8>, mut value: u64) {
    loop {
        let low = (value & 0x7f).to_le_bytes()[0];
        value >>= 7;
        if value == 0 {
            bytes.push(low);
            return;
        }
        bytes.push(low | 0x80);
    }
}

/// Returns how many bytes [`encode_varint`] writes for `value`.
#[must_use]
pub const fn varint_len(value: u64) -> usize {
    let bits = 64 - value.leading_zeros() as usize;
    if bits == 0 { 1 } else { bits.div_ceil(7) }
}

/// Decodes one unsigned LEB128 integer a byte at a time, for readers that
/// receive bytes from a stream.
///
/// ```
/// use footfall_session::codec::{VarintDecoder, encode_varint};
///
/// let mut bytes = Vec::new();
/// encode_varint(&mut bytes, 300);
/// let mut decoder = VarintDecoder::new();
/// assert_eq!(decoder.push(bytes[0])?, None);
/// assert_eq!(decoder.push(bytes[1])?, Some(300));
/// # Ok::<(), std::io::Error>(())
/// ```
#[derive(Clone, Copy, Debug, Default)]
pub struct VarintDecoder {
    value: u64,
    shift: u32,
}

impl VarintDecoder {
    /// Starts decoding an integer.
    #[must_use]
    pub const fn new() -> Self {
        Self { value: 0, shift: 0 }
    }

    /// Takes the next byte. Returns the integer after its last byte, `None`
    /// while more bytes are needed, and an error for an encoding longer than
    /// ten bytes or a value that does not fit a `u64`.
    pub fn push(&mut self, byte: u8) -> io::Result<Option<u64>> {
        let bits = u64::from(byte & 0x7f);
        if self.shift == 63 && bits > 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "integer does not fit u64",
            ));
        }
        self.value |= bits << self.shift;
        if byte & 0x80 == 0 {
            return Ok(Some(self.value));
        }
        self.shift += 7;
        if self.shift > 63 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "integer longer than ten bytes",
            ));
        }
        Ok(None)
    }
}

/// Writes a signed integer zigzag-encoded, so small magnitudes of either
/// sign take few bytes.
pub(crate) fn encode_i64(bytes: &mut Vec<u8>, value: i64) {
    encode_u64(bytes, ((value << 1) ^ (value >> 63)).cast_unsigned());
}

pub(crate) fn decode_i64(cursor: &mut Cursor<'_>) -> io::Result<i64> {
    let value = decode_u64(cursor)?;
    Ok((value >> 1).cast_signed() ^ -(value & 1).cast_signed())
}

/// Writes `value` as its signed difference from `previous`.
pub(crate) fn encode_delta(bytes: &mut Vec<u8>, previous: u64, value: u64) {
    encode_i64(bytes, value.wrapping_sub(previous).cast_signed());
}

pub(crate) fn decode_delta(cursor: &mut Cursor<'_>, previous: u64) -> io::Result<u64> {
    Ok(previous.wrapping_add(decode_i64(cursor)?.cast_unsigned()))
}

/// Descriptor names already written in the payload being encoded.
///
/// A row that names a descriptor writes its id, then its name only the first
/// time that id appears in the payload; ids are unique across descriptor
/// kinds, so one set covers scopes, events, counters, and resource kinds.
#[derive(Debug, Default)]
pub(crate) struct NameWriter(HashSet<u64>);

impl NameWriter {
    pub(crate) fn encode(&mut self, bytes: &mut Vec<u8>, id: u64, name: &str) -> io::Result<()> {
        encode_u64(bytes, id);
        if self.0.insert(id) {
            encode_string(bytes, name)?;
        }
        Ok(())
    }
}

/// Descriptor names already read in the payload being decoded; see
/// [`NameWriter`]. Read the id with [`id`](Self::id), then its name with
/// [`name`](Self::name).
#[derive(Debug, Default)]
pub(crate) struct NameReader {
    names: HashMap<u64, String>,
    last_id: u64,
}

impl NameReader {
    pub(crate) fn id(&mut self, cursor: &mut Cursor<'_>) -> io::Result<u64> {
        self.last_id = decode_u64(cursor)?;
        Ok(self.last_id)
    }

    pub(crate) fn name(&mut self, cursor: &mut Cursor<'_>) -> io::Result<String> {
        if let Some(name) = self.names.get(&self.last_id) {
            return Ok(name.clone());
        }
        let name = decode_string(cursor)?;
        self.names.insert(self.last_id, name.clone());
        Ok(name)
    }
}

#[derive(Debug)]
pub(crate) struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn read_exact(&mut self, len: usize) -> io::Result<&'a [u8]> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "cursor overflow"))?;
        let bytes = self.bytes.get(self.offset..end).ok_or_else(|| {
            io::Error::new(io::ErrorKind::UnexpectedEof, "truncated session envelope")
        })?;
        self.offset = end;
        Ok(bytes)
    }

    pub(crate) fn is_exhausted(&self) -> bool {
        self.offset == self.bytes.len()
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }
}
