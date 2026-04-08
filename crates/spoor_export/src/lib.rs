// Copyright 2026 the Spoor Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Export bridges for Spoor snapshots.

use spoor_core::{Context, LabelValue, MarkerKind, MarkerPhase, ResourceId};
use spoor_runtime::{CounterKind, CounterSnapshot, Observation, Snapshot, TimedObservation};
use std::fmt::{self, Write};

/// Converts a snapshot into Chrome Trace Event JSON suitable for inspection in Perfetto.
#[must_use]
pub fn to_chrome_trace_json(snapshot: &Snapshot) -> String {
    let mut output = String::new();
    write_chrome_trace_json(snapshot, &mut output)
        .expect("writing Chrome Trace JSON to a string should not fail");
    output
}

/// Writes a snapshot as Chrome Trace Event JSON.
pub fn write_chrome_trace_json(snapshot: &Snapshot, writer: &mut impl Write) -> fmt::Result {
    writer.write_str("{\"displayTimeUnit\":\"ms\",\"traceEvents\":[")?;
    let mut first = true;

    for counter in &snapshot.counters {
        write_event_separator(writer, &mut first)?;
        write_counter_event(writer, counter)?;
    }

    for event in &snapshot.events {
        write_event_separator(writer, &mut first)?;
        write_timed_observation(writer, event)?;
    }

    if snapshot.dropped_counters > 0 {
        write_event_separator(writer, &mut first)?;
        write_internal_counter_event(
            writer,
            snapshot.captured_at_micros,
            "spoor_dropped_counters",
            snapshot.dropped_counters,
        )?;
    }

    if snapshot.dropped_events > 0 {
        write_event_separator(writer, &mut first)?;
        write_internal_counter_event(
            writer,
            snapshot.captured_at_micros,
            "spoor_dropped_events",
            snapshot.dropped_events,
        )?;
    }

    writer.write_str("]}")?;
    Ok(())
}

fn write_event_separator(writer: &mut impl Write, first: &mut bool) -> fmt::Result {
    if *first {
        *first = false;
        return Ok(());
    }

    writer.write_char(',')
}

fn write_counter_event(writer: &mut impl Write, counter: &CounterSnapshot) -> fmt::Result {
    writer.write_char('{')?;
    write_json_field_str(writer, "name", counter.name)?;
    writer.write_char(',')?;
    write_json_field_str(writer, "cat", "counter")?;
    writer.write_char(',')?;
    write_json_field_str(writer, "ph", "C")?;
    writer.write_char(',')?;
    write_json_field_u64(writer, "ts", counter.updated_at_micros)?;
    writer.write_char(',')?;
    write_json_field_u64(writer, "pid", 1)?;
    writer.write_char(',')?;
    write_json_field_u64(writer, "tid", 0)?;
    writer.write_str(",\"args\":{")?;
    write_json_field_u64(writer, "value", counter.value)?;
    writer.write_str(",\"kind\":")?;
    write_json_string(
        writer,
        match counter.kind {
            CounterKind::Counter => "counter",
            CounterKind::Gauge => "gauge",
        },
    )?;
    writer.write_str(",\"context\":")?;
    write_context(writer, counter.context)?;
    writer.write_str("}}")
}

fn write_internal_counter_event(
    writer: &mut impl Write,
    at_micros: u64,
    name: &str,
    value: u64,
) -> fmt::Result {
    writer.write_char('{')?;
    write_json_field_str(writer, "name", name)?;
    writer.write_char(',')?;
    write_json_field_str(writer, "cat", "spoor")?;
    writer.write_char(',')?;
    write_json_field_str(writer, "ph", "C")?;
    writer.write_char(',')?;
    write_json_field_u64(writer, "ts", at_micros)?;
    writer.write_char(',')?;
    write_json_field_u64(writer, "pid", 1)?;
    writer.write_char(',')?;
    write_json_field_u64(writer, "tid", 0)?;
    writer.write_str(",\"args\":{\"value\":")?;
    writer.write_fmt(format_args!("{value}"))?;
    writer.write_str("}}")
}

fn write_timed_observation(writer: &mut impl Write, event: &TimedObservation) -> fmt::Result {
    match &event.observation {
        Observation::ScopeBegin {
            name,
            context,
            token,
            ..
        } => {
            writer.write_char('{')?;
            write_json_field_str(writer, "name", name)?;
            writer.write_char(',')?;
            write_json_field_str(writer, "cat", "scope")?;
            writer.write_char(',')?;
            write_json_field_str(writer, "ph", "B")?;
            writer.write_char(',')?;
            write_json_field_u64(writer, "ts", event.at_micros)?;
            writer.write_char(',')?;
            write_json_field_u64(writer, "pid", 1)?;
            writer.write_char(',')?;
            write_json_field_u64(writer, "tid", 0)?;
            writer.write_str(",\"args\":{")?;
            write_json_field_u64(writer, "scope_token", token.get())?;
            writer.write_str(",\"context\":")?;
            write_context(writer, *context)?;
            writer.write_str("}}")
        }
        Observation::ScopeEnd { .. } => {
            writer.write_char('{')?;
            write_json_field_str(writer, "ph", "E")?;
            writer.write_char(',')?;
            write_json_field_u64(writer, "ts", event.at_micros)?;
            writer.write_char(',')?;
            write_json_field_u64(writer, "pid", 1)?;
            writer.write_char(',')?;
            write_json_field_u64(writer, "tid", 0)?;
            writer.write_char('}')
        }
        Observation::ResourceCreate {
            kind_name,
            resource_id,
            context,
            size,
            ..
        } => write_resource_event(
            writer,
            event.at_micros,
            "resource_create",
            kind_name,
            *resource_id,
            Some(*size),
            *context,
        ),
        Observation::ResourceUpdate {
            kind_name,
            resource_id,
            context,
            size,
            ..
        } => write_resource_event(
            writer,
            event.at_micros,
            "resource_update",
            kind_name,
            *resource_id,
            Some(*size),
            *context,
        ),
        Observation::ResourceDrop {
            kind_name,
            resource_id,
            context,
            ..
        } => write_resource_event(
            writer,
            event.at_micros,
            "resource_drop",
            kind_name,
            *resource_id,
            None,
            *context,
        ),
        Observation::FrameMarker {
            kind,
            phase,
            context,
        } => {
            writer.write_char('{')?;
            write_json_field_str(writer, "name", marker_name(*kind, *phase))?;
            writer.write_char(',')?;
            write_json_field_str(writer, "cat", "marker")?;
            writer.write_char(',')?;
            write_json_field_str(writer, "ph", "i")?;
            writer.write_str(",\"s\":\"t\",")?;
            write_json_field_u64(writer, "ts", event.at_micros)?;
            writer.write_char(',')?;
            write_json_field_u64(writer, "pid", 1)?;
            writer.write_char(',')?;
            write_json_field_u64(writer, "tid", 0)?;
            writer.write_str(",\"args\":{\"context\":")?;
            write_context(writer, *context)?;
            writer.write_str("}}")
        }
    }
}

fn write_resource_event(
    writer: &mut impl Write,
    at_micros: u64,
    name: &str,
    kind_name: &str,
    resource_id: ResourceId,
    size: Option<u64>,
    context: Context,
) -> fmt::Result {
    writer.write_char('{')?;
    write_json_field_str(writer, "name", name)?;
    writer.write_char(',')?;
    write_json_field_str(writer, "cat", kind_name)?;
    writer.write_char(',')?;
    write_json_field_str(writer, "ph", "i")?;
    writer.write_str(",\"s\":\"t\",")?;
    write_json_field_u64(writer, "ts", at_micros)?;
    writer.write_char(',')?;
    write_json_field_u64(writer, "pid", 1)?;
    writer.write_char(',')?;
    write_json_field_u64(writer, "tid", 0)?;
    writer.write_str(",\"args\":{")?;
    write_json_field_str(writer, "kind", kind_name)?;
    writer.write_char(',')?;
    write_json_field_u64(writer, "resource_id", resource_id.get())?;
    if let Some(size) = size {
        writer.write_char(',')?;
        write_json_field_u64(writer, "size", size)?;
    }
    writer.write_str(",\"context\":")?;
    write_context(writer, context)?;
    writer.write_str("}}")
}

fn write_context(writer: &mut impl Write, context: Context) -> fmt::Result {
    writer.write_char('{')?;
    let mut first = true;
    for entry in context.iter() {
        if !first {
            writer.write_char(',')?;
        }
        first = false;
        write_json_string(writer, entry.key().as_str())?;
        writer.write_char(':')?;
        write_label_value(writer, entry.value())?;
    }
    writer.write_char('}')
}

fn write_label_value(writer: &mut impl Write, value: LabelValue) -> fmt::Result {
    if let Some(value) = value.as_unsigned() {
        return writer.write_fmt(format_args!("{value}"));
    }
    if let Some(value) = value.as_signed() {
        return writer.write_fmt(format_args!("{value}"));
    }
    if let Some(value) = value.as_bool() {
        return writer.write_str(if value { "true" } else { "false" });
    }
    if let Some(value) = value.as_static_str() {
        return write_json_string(writer, value);
    }

    Ok(())
}

fn marker_name(kind: MarkerKind, phase: MarkerPhase) -> &'static str {
    match (kind, phase) {
        (MarkerKind::Frame, MarkerPhase::Begin) => "frame_begin",
        (MarkerKind::Frame, MarkerPhase::End) => "frame_end",
        (MarkerKind::Transaction, MarkerPhase::Begin) => "transaction_begin",
        (MarkerKind::Transaction, MarkerPhase::End) => "transaction_end",
        (MarkerKind::Pass, MarkerPhase::Begin) => "pass_begin",
        (MarkerKind::Pass, MarkerPhase::End) => "pass_end",
    }
}

fn write_json_field_str(writer: &mut impl Write, key: &str, value: &str) -> fmt::Result {
    write_json_string(writer, key)?;
    writer.write_char(':')?;
    write_json_string(writer, value)
}

fn write_json_field_u64(writer: &mut impl Write, key: &str, value: u64) -> fmt::Result {
    write_json_string(writer, key)?;
    writer.write_char(':')?;
    writer.write_fmt(format_args!("{value}"))
}

fn write_json_string(writer: &mut impl Write, value: &str) -> fmt::Result {
    writer.write_char('"')?;
    for ch in value.chars() {
        match ch {
            '"' => writer.write_str("\\\"")?,
            '\\' => writer.write_str("\\\\")?,
            '\n' => writer.write_str("\\n")?,
            '\r' => writer.write_str("\\r")?,
            '\t' => writer.write_str("\\t")?,
            _ => writer.write_char(ch)?,
        }
    }
    writer.write_char('"')
}

#[cfg(test)]
mod tests {
    use super::to_chrome_trace_json;
    use spoor_core::{Context, CounterDesc, CounterDescId, LabelKey, MarkerKind, MarkerPhase};
    use spoor_runtime::{CounterKind, CounterSnapshot, Observation, Snapshot, TimedObservation};

    #[test]
    fn exporter_emits_trace_events_and_context() {
        let snapshot = Snapshot {
            captured_at_micros: 30,
            counters: vec![CounterSnapshot {
                desc_id: CounterDescId::new(1),
                name: CounterDesc::bytes(CounterDescId::new(1), "upload_bytes").name(),
                context: Context::root().with(LabelKey::Frame, 7_u32),
                kind: CounterKind::Counter,
                value: 4096,
                updated_at_micros: 10,
            }],
            events: vec![TimedObservation {
                at_micros: 20,
                observation: Observation::FrameMarker {
                    kind: MarkerKind::Frame,
                    phase: MarkerPhase::Begin,
                    context: Context::root().with(LabelKey::Pass, 1_u32),
                },
            }],
            dropped_counters: 1,
            dropped_events: 2,
        };

        let json = to_chrome_trace_json(&snapshot);
        assert!(json.contains("\"traceEvents\""));
        assert!(json.contains("\"upload_bytes\""));
        assert!(json.contains("\"frame_begin\""));
        assert!(json.contains("\"frame\":7"));
        assert!(json.contains("\"spoor_dropped_events\""));
    }
}
