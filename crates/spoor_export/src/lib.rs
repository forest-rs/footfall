// Copyright 2026 the Spoor Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Export bridges for Spoor snapshots.

use spoor_core::{
    Context, ContextId, FlowPhase, LabelValue, MarkerKind, MarkerPhase, ResourceId, ScopeToken,
};
use spoor_runtime::{CounterKind, CounterSnapshot, Observation, Snapshot, TimedObservation};
use std::{
    collections::HashMap,
    fmt::{self, Write},
    fs,
    path::Path,
};

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
    let mut events = Vec::new();

    events.push(build_metadata_event(
        "process_name",
        1,
        0,
        "name",
        &snapshot.metadata.process_name,
    ));
    if let Some(session_name) = snapshot.metadata.session_name.as_deref() {
        events.push(build_metadata_event(
            "process_labels",
            1,
            0,
            "labels",
            session_name,
        ));
    }
    events.push(build_capture_window_event(snapshot));

    for context in &snapshot.contexts {
        events.push(build_thread_name_event(
            context.id,
            track_name(context.context),
        ));
    }

    for counter in &snapshot.counters {
        events.push(build_counter_event(snapshot, counter)?);
    }

    let mut scope_tracks = HashMap::<ScopeToken, ContextId>::new();
    for event in &snapshot.events {
        events.push(build_timed_observation(snapshot, event, &mut scope_tracks)?);
    }

    if snapshot.dropped_contexts > 0 {
        events.push(build_internal_counter_event(
            snapshot.captured_at_micros,
            "spoor_dropped_contexts",
            snapshot.dropped_contexts,
        ));
    }

    if snapshot.dropped_counters > 0 {
        events.push(build_internal_counter_event(
            snapshot.captured_at_micros,
            "spoor_dropped_counters",
            snapshot.dropped_counters,
        ));
    }

    if snapshot.dropped_events > 0 {
        events.push(build_internal_counter_event(
            snapshot.captured_at_micros,
            "spoor_dropped_events",
            snapshot.dropped_events,
        ));
    }

    events.sort_by_key(|event| (event.ts_micros, event.order));

    writer.write_str("{\"displayTimeUnit\":\"ms\",\"traceEvents\":[")?;
    let mut first = true;
    for event in events {
        if !first {
            writer.write_char(',')?;
        }
        first = false;
        writer.write_str(&event.json)?;
    }
    writer.write_str("]}")?;
    Ok(())
}

/// Writes Chrome Trace Event JSON to a file path.
pub fn write_chrome_trace_file(snapshot: &Snapshot, path: impl AsRef<Path>) -> std::io::Result<()> {
    fs::write(path, to_chrome_trace_json(snapshot))
}

#[derive(Debug)]
struct ExportEvent {
    ts_micros: u64,
    order: u8,
    json: String,
}

fn build_metadata_event(
    name: &str,
    pid: u64,
    tid: u64,
    arg_key: &str,
    arg_value: &str,
) -> ExportEvent {
    let mut json = String::new();
    json.push('{');
    write_json_field_str(&mut json, "name", name).expect("string write should succeed");
    json.push(',');
    write_json_field_str(&mut json, "ph", "M").expect("string write should succeed");
    json.push(',');
    write_json_field_u64(&mut json, "ts", 0).expect("string write should succeed");
    json.push(',');
    write_json_field_u64(&mut json, "pid", pid).expect("string write should succeed");
    json.push(',');
    write_json_field_u64(&mut json, "tid", tid).expect("string write should succeed");
    json.push_str(",\"args\":{");
    write_json_field_str(&mut json, arg_key, arg_value).expect("string write should succeed");
    json.push_str("}}");
    ExportEvent {
        ts_micros: 0,
        order: 0,
        json,
    }
}

fn build_thread_name_event(context_id: ContextId, name: String) -> ExportEvent {
    build_metadata_event("thread_name", 1, context_id.get(), "name", &name)
}

fn build_capture_window_event(snapshot: &Snapshot) -> ExportEvent {
    let mut json = String::new();
    json.push('{');
    write_json_field_str(&mut json, "name", "spoor_capture").expect("string write should succeed");
    json.push(',');
    write_json_field_str(&mut json, "cat", "spoor").expect("string write should succeed");
    json.push(',');
    write_json_field_str(&mut json, "ph", "i").expect("string write should succeed");
    json.push_str(",\"s\":\"g\",");
    write_json_field_u64(&mut json, "ts", snapshot.captured_at_micros)
        .expect("string write should succeed");
    json.push(',');
    write_json_field_u64(&mut json, "pid", 1).expect("string write should succeed");
    json.push(',');
    write_json_field_u64(&mut json, "tid", 0).expect("string write should succeed");
    json.push_str(",\"args\":{");
    write_json_field_u64(
        &mut json,
        "capture_start_micros",
        snapshot.capture_start_micros,
    )
    .expect("string write should succeed");
    json.push(',');
    write_json_field_u64(&mut json, "capture_end_micros", snapshot.captured_at_micros)
        .expect("string write should succeed");
    json.push(',');
    write_json_field_u64(
        &mut json,
        "session_sequence",
        snapshot.metadata.session_sequence,
    )
    .expect("string write should succeed");
    json.push(',');
    write_json_field_u64(
        &mut json,
        "wall_clock_anchor_micros",
        snapshot.metadata.wall_clock_anchor_micros,
    )
    .expect("string write should succeed");
    json.push_str("}}");
    ExportEvent {
        ts_micros: snapshot.captured_at_micros,
        order: 4,
        json,
    }
}

fn build_counter_event(
    snapshot: &Snapshot,
    counter: &CounterSnapshot,
) -> Result<ExportEvent, fmt::Error> {
    let mut json = String::new();
    let context = snapshot
        .context(counter.context_id)
        .map(|context| context.context)
        .unwrap_or_else(Context::root);

    json.push('{');
    write_json_field_str(&mut json, "name", counter.name)?;
    json.push(',');
    write_json_field_str(&mut json, "cat", "counter")?;
    json.push(',');
    write_json_field_str(&mut json, "ph", "C")?;
    json.push(',');
    write_json_field_u64(&mut json, "ts", counter.updated_at_micros)?;
    json.push(',');
    write_json_field_u64(&mut json, "pid", 1)?;
    json.push(',');
    write_json_field_u64(&mut json, "tid", counter.context_id.get())?;
    json.push_str(",\"args\":{");
    write_json_field_u64(&mut json, "value", counter.value)?;
    json.push_str(",\"kind\":");
    write_json_string(
        &mut json,
        match counter.kind {
            CounterKind::Counter => "counter",
            CounterKind::Gauge => "gauge",
        },
    )?;
    json.push_str(",\"unit\":");
    write_json_string(&mut json, counter.unit.as_str())?;
    json.push_str(",\"context\":");
    write_context(&mut json, context)?;
    json.push_str("}}");

    Ok(ExportEvent {
        ts_micros: counter.updated_at_micros,
        order: 1,
        json,
    })
}

fn build_internal_counter_event(at_micros: u64, name: &str, value: u64) -> ExportEvent {
    let mut json = String::new();
    json.push('{');
    write_json_field_str(&mut json, "name", name).expect("string write should succeed");
    json.push(',');
    write_json_field_str(&mut json, "cat", "spoor").expect("string write should succeed");
    json.push(',');
    write_json_field_str(&mut json, "ph", "C").expect("string write should succeed");
    json.push(',');
    write_json_field_u64(&mut json, "ts", at_micros).expect("string write should succeed");
    json.push(',');
    write_json_field_u64(&mut json, "pid", 1).expect("string write should succeed");
    json.push(',');
    write_json_field_u64(&mut json, "tid", 0).expect("string write should succeed");
    json.push_str(",\"args\":{\"value\":");
    json.write_fmt(format_args!("{value}"))
        .expect("string write should succeed");
    json.push_str("}}");
    ExportEvent {
        ts_micros: at_micros,
        order: 3,
        json,
    }
}

fn build_timed_observation(
    snapshot: &Snapshot,
    event: &TimedObservation,
    scope_tracks: &mut HashMap<ScopeToken, ContextId>,
) -> Result<ExportEvent, fmt::Error> {
    match &event.observation {
        Observation::ScopeBegin {
            name,
            context_id,
            token,
            args,
            ..
        } => {
            scope_tracks.insert(*token, *context_id);
            let context = snapshot
                .context(*context_id)
                .map(|context| context.context)
                .unwrap_or_else(Context::root);
            let mut json = String::new();
            json.push('{');
            write_json_field_str(&mut json, "name", name)?;
            json.push(',');
            write_json_field_str(&mut json, "cat", "scope")?;
            json.push(',');
            write_json_field_str(&mut json, "ph", "B")?;
            json.push(',');
            write_json_field_u64(&mut json, "ts", event.at_micros)?;
            json.push(',');
            write_json_field_u64(&mut json, "pid", 1)?;
            json.push(',');
            write_json_field_u64(&mut json, "tid", context_id.get())?;
            json.push_str(",\"args\":{");
            write_json_field_u64(&mut json, "thread", event.thread_id)?;
            if !args.is_empty() {
                json.push_str(",\"values\":");
                write_args(&mut json, *args)?;
            }
            json.push_str(",\"context\":");
            write_context(&mut json, context)?;
            json.push_str("}}");
            Ok(ExportEvent {
                ts_micros: event.at_micros,
                order: 1,
                json,
            })
        }
        Observation::ScopeEnd { token } => {
            let tid = scope_tracks
                .remove(token)
                .unwrap_or(ContextId::new(0))
                .get();
            let mut json = String::new();
            json.push('{');
            write_json_field_str(&mut json, "ph", "E")?;
            json.push(',');
            write_json_field_u64(&mut json, "ts", event.at_micros)?;
            json.push(',');
            write_json_field_u64(&mut json, "pid", 1)?;
            json.push(',');
            write_json_field_u64(&mut json, "tid", tid)?;
            json.push('}');
            Ok(ExportEvent {
                ts_micros: event.at_micros,
                order: 2,
                json,
            })
        }
        Observation::ResourceCreate {
            kind_name,
            resource_id,
            context_id,
            size,
            ..
        } => build_resource_event(
            snapshot,
            event.at_micros,
            event.thread_id,
            "resource_create",
            kind_name,
            *resource_id,
            *context_id,
            Some(*size),
        ),
        Observation::ResourceUpdate {
            kind_name,
            resource_id,
            context_id,
            size,
            ..
        } => build_resource_event(
            snapshot,
            event.at_micros,
            event.thread_id,
            "resource_update",
            kind_name,
            *resource_id,
            *context_id,
            Some(*size),
        ),
        Observation::ResourceDrop {
            kind_name,
            resource_id,
            context_id,
            ..
        } => build_resource_event(
            snapshot,
            event.at_micros,
            event.thread_id,
            "resource_drop",
            kind_name,
            *resource_id,
            *context_id,
            None,
        ),
        Observation::FrameMarker {
            kind,
            phase,
            context_id,
        } => {
            let context = snapshot
                .context(*context_id)
                .map(|context| context.context)
                .unwrap_or_else(Context::root);
            let mut json = String::new();
            json.push('{');
            write_json_field_str(&mut json, "name", marker_name(*kind, *phase))?;
            json.push(',');
            write_json_field_str(&mut json, "cat", "marker")?;
            json.push(',');
            write_json_field_str(&mut json, "ph", "i")?;
            json.push_str(",\"s\":\"t\",");
            write_json_field_u64(&mut json, "ts", event.at_micros)?;
            json.push(',');
            write_json_field_u64(&mut json, "pid", 1)?;
            json.push(',');
            write_json_field_u64(&mut json, "tid", context_id.get())?;
            json.push_str(",\"args\":{");
            write_json_field_u64(&mut json, "thread", event.thread_id)?;
            json.push_str(",\"context\":");
            write_context(&mut json, context)?;
            json.push_str("}}");
            Ok(ExportEvent {
                ts_micros: event.at_micros,
                order: 1,
                json,
            })
        }
        Observation::Flow {
            flow_id,
            phase,
            context_id,
        } => {
            let context = snapshot
                .context(*context_id)
                .map(|context| context.context)
                .unwrap_or_else(Context::root);
            let mut json = String::new();
            json.push('{');
            write_json_field_str(&mut json, "name", "flow")?;
            json.push(',');
            write_json_field_str(&mut json, "cat", "flow")?;
            json.push(',');
            write_json_field_str(&mut json, "ph", flow_phase_code(*phase))?;
            json.push_str(",\"bp\":\"e\",");
            write_json_field_u64(&mut json, "ts", event.at_micros)?;
            json.push(',');
            write_json_field_u64(&mut json, "pid", 1)?;
            json.push(',');
            write_json_field_u64(&mut json, "tid", context_id.get())?;
            json.push(',');
            write_json_field_u64(&mut json, "id", flow_id.get())?;
            json.push_str(",\"args\":{");
            write_json_field_u64(&mut json, "thread", event.thread_id)?;
            json.push_str(",\"context\":");
            write_context(&mut json, context)?;
            json.push_str("}}");
            Ok(ExportEvent {
                ts_micros: event.at_micros,
                order: 1,
                json,
            })
        }
    }
}

fn build_resource_event(
    snapshot: &Snapshot,
    at_micros: u64,
    thread_id: u64,
    name: &str,
    kind_name: &str,
    resource_id: ResourceId,
    context_id: ContextId,
    size: Option<u64>,
) -> Result<ExportEvent, fmt::Error> {
    let context = snapshot
        .context(context_id)
        .map(|context| context.context)
        .unwrap_or_else(Context::root);
    let mut json = String::new();
    json.push('{');
    write_json_field_str(&mut json, "name", name)?;
    json.push(',');
    write_json_field_str(&mut json, "cat", kind_name)?;
    json.push(',');
    write_json_field_str(&mut json, "ph", "i")?;
    json.push_str(",\"s\":\"t\",");
    write_json_field_u64(&mut json, "ts", at_micros)?;
    json.push(',');
    write_json_field_u64(&mut json, "pid", 1)?;
    json.push(',');
    write_json_field_u64(&mut json, "tid", context_id.get())?;
    json.push_str(",\"args\":{");
    write_json_field_u64(&mut json, "thread", thread_id)?;
    json.push(',');
    write_json_field_str(&mut json, "kind", kind_name)?;
    json.push(',');
    write_json_field_u64(&mut json, "resource_id", resource_id.get())?;
    if let Some(size) = size {
        json.push(',');
        write_json_field_u64(&mut json, "size", size)?;
    }
    json.push_str(",\"context\":");
    write_context(&mut json, context)?;
    json.push_str("}}");
    Ok(ExportEvent {
        ts_micros: at_micros,
        order: 1,
        json,
    })
}

fn write_args(writer: &mut impl Write, args: spoor_core::Args) -> fmt::Result {
    writer.write_char('{')?;
    let mut first = true;
    for entry in args.iter() {
        if !first {
            writer.write_char(',')?;
        }
        first = false;
        write_json_string(writer, entry.key())?;
        writer.write_char(':')?;
        write_label_value(writer, entry.value())?;
    }
    writer.write_char('}')
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

fn track_name(context: Context) -> String {
    if context.is_empty() {
        return String::from("root");
    }

    let mut name = String::new();
    for (index, entry) in context.iter().enumerate() {
        if index > 0 {
            name.push_str(" / ");
        }
        name.push_str(entry.key().as_str());
        name.push('=');
        append_label_value(&mut name, entry.value());
    }
    name
}

fn append_label_value(output: &mut String, value: LabelValue) {
    if let Some(value) = value.as_unsigned() {
        output.push_str(&value.to_string());
        return;
    }
    if let Some(value) = value.as_signed() {
        output.push_str(&value.to_string());
        return;
    }
    if let Some(value) = value.as_bool() {
        output.push_str(if value { "true" } else { "false" });
        return;
    }
    if let Some(value) = value.as_static_str() {
        output.push_str(value);
    }
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

fn flow_phase_code(phase: FlowPhase) -> &'static str {
    match phase {
        FlowPhase::Begin => "s",
        FlowPhase::Step => "t",
        FlowPhase::End => "f",
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
            ch if ch < '\u{20}' => {
                writer.write_fmt(format_args!("\\u{:04x}", u32::from(ch)))?;
            }
            _ => writer.write_char(ch)?,
        }
    }
    writer.write_char('"')
}

#[cfg(test)]
mod tests {
    use super::{to_chrome_trace_json, track_name};
    use spoor_core::{
        Args, Context, ContextId, CounterDesc, CounterDescId, FlowId, FlowPhase, LabelKey,
        MarkerKind, MarkerPhase, ScopeToken, Unit,
    };
    use spoor_runtime::{
        ContextSnapshot, CounterKind, CounterSnapshot, Observation, Snapshot, TimedObservation,
    };

    #[test]
    fn exporter_emits_metadata_context_tracks_and_scope_pairing() {
        let snapshot = Snapshot {
            metadata: spoor_runtime::SnapshotMetadata {
                process_name: String::from("spoor"),
                session_name: Some(String::from("recording")),
                session_sequence: 3,
                wall_clock_anchor_micros: 1_700_000_000_000_000,
            },
            capture_start_micros: 0,
            captured_at_micros: 30,
            contexts: vec![
                ContextSnapshot {
                    id: ContextId::new(0),
                    context: Context::root(),
                },
                ContextSnapshot {
                    id: ContextId::new(1),
                    context: Context::root()
                        .with(LabelKey::Frame, 7_u32)
                        .with(LabelKey::Pass, 1_u32),
                },
            ],
            counters: vec![CounterSnapshot {
                desc_id: CounterDescId::new(1),
                name: CounterDesc::bytes(CounterDescId::new(1), "upload_bytes").name(),
                unit: Unit::Bytes,
                context_id: ContextId::new(1),
                kind: CounterKind::Counter,
                value: 4096,
                updated_at_micros: 10,
            }],
            events: vec![
                TimedObservation {
                    at_micros: 20,
                    thread_id: 3,
                    observation: Observation::ScopeBegin {
                        desc_id: spoor_core::ScopeDescId::new(2),
                        name: "record",
                        context_id: ContextId::new(1),
                        token: ScopeToken::new(9),
                        args: Args::none().with("items", 3_u32),
                    },
                },
                TimedObservation {
                    at_micros: 22,
                    thread_id: 3,
                    observation: Observation::ScopeEnd {
                        token: ScopeToken::new(9),
                    },
                },
                TimedObservation {
                    at_micros: 24,
                    thread_id: 3,
                    observation: Observation::FrameMarker {
                        kind: MarkerKind::Frame,
                        phase: MarkerPhase::Begin,
                        context_id: ContextId::new(1),
                    },
                },
                TimedObservation {
                    at_micros: 26,
                    thread_id: 3,
                    observation: Observation::Flow {
                        flow_id: FlowId::new(8),
                        phase: FlowPhase::Begin,
                        context_id: ContextId::new(1),
                    },
                },
            ],
            dropped_contexts: 1,
            dropped_counters: 1,
            dropped_events: 2,
        };

        let json = to_chrome_trace_json(&snapshot);
        assert!(json.contains("\"thread_name\""));
        assert!(json.contains("\"process_labels\""));
        assert!(json.contains("frame=7 / pass=1"));
        assert!(json.contains("\"tid\":1"));
        assert!(json.contains("\"ph\":\"B\""));
        assert!(json.contains("\"values\":{\"items\":3}"));
        assert!(json.contains("\"ph\":\"E\""));
        assert!(json.contains("\"ph\":\"s\""));
        assert!(json.contains("\"spoor_capture\""));
        assert!(json.contains("\"spoor_dropped_contexts\""));
    }

    #[test]
    fn track_name_uses_context_entries() {
        let context = Context::root()
            .with(LabelKey::Frame, 3_u32)
            .with(LabelKey::Reason, "invalidate");
        assert_eq!(track_name(context), "frame=3 / reason=invalidate");
    }

    #[test]
    fn json_strings_escape_control_characters() {
        let mut out = String::new();
        super::write_json_string(&mut out, "a\u{1}b\u{1f}c\"d\\e\nf").expect("write succeeds");
        assert_eq!(out, "\"a\\u0001b\\u001fc\\\"d\\\\e\\nf\"");
    }
}
