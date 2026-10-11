// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Export bridges for Footfall snapshots.

use footfall_core::{
    Context, ContextId, FlowPhase, LabelValue, MarkerKind, MarkerPhase, ResourceId, ScopeToken,
};
use footfall_runtime::{
    CounterKind, CounterSnapshot, HistogramSnapshot, Observation, Snapshot, TimedObservation,
};
use std::{
    collections::{BTreeSet, HashMap},
    fmt::{self, Write},
    fs,
    path::Path,
};

/// Converts a snapshot into Chrome Trace Event JSON suitable for inspection in Perfetto.
///
/// Timed observations are placed on one track per recording thread, using the
/// runtime-assigned thread id as `tid`, and keep the snapshot's event order.
/// A scope end is placed on the track of its matching begin; an end whose
/// begin is not in the snapshot is skipped and counted in the
/// `footfall_unmatched_scope_ends` internal counter. Aggregates (counters,
/// gauges, histograms, capture metadata) live on track `0`. Each counter or
/// gauge becomes one Chrome counter track per descriptor and context, named
/// after the descriptor and the context labels, with a single numeric series
/// named after the descriptor unit.
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
    events.push(build_metadata_event(
        "thread_name",
        1,
        AGGREGATE_TID,
        "name",
        "footfall",
    ));
    let threads: BTreeSet<u64> = snapshot
        .events
        .iter()
        .map(|event| event.thread_id)
        .collect();
    for thread_id in threads {
        events.push(build_metadata_event(
            "thread_name",
            1,
            thread_id,
            "name",
            &format!("thread {thread_id}"),
        ));
    }
    events.push(build_capture_window_event(snapshot));

    for counter in &snapshot.counters {
        events.push(build_counter_event(snapshot, counter)?);
    }

    for histogram in &snapshot.histograms {
        events.push(build_histogram_event(snapshot, histogram)?);
    }

    let mut scope_tracks = HashMap::<ScopeToken, u64>::new();
    let mut unmatched_scope_ends = 0_u64;
    for event in &snapshot.events {
        match build_timed_observation(snapshot, event, &mut scope_tracks)? {
            Some(json) => events.push(json),
            None => unmatched_scope_ends += 1,
        }
    }

    if unmatched_scope_ends > 0 {
        events.push(build_internal_counter_event(
            snapshot.captured_at_micros,
            "footfall_unmatched_scope_ends",
            unmatched_scope_ends,
        ));
    }

    if snapshot.dropped_contexts > 0 {
        events.push(build_internal_counter_event(
            snapshot.captured_at_micros,
            "footfall_dropped_contexts",
            snapshot.dropped_contexts,
        ));
    }

    if snapshot.dropped_counters > 0 {
        events.push(build_internal_counter_event(
            snapshot.captured_at_micros,
            "footfall_dropped_counters",
            snapshot.dropped_counters,
        ));
    }

    if snapshot.dropped_events > 0 {
        events.push(build_internal_counter_event(
            snapshot.captured_at_micros,
            "footfall_dropped_events",
            snapshot.dropped_events,
        ));
    }

    // Events stay in construction order. Timed observations follow the
    // snapshot order, which keeps same-timestamp begins and ends on one
    // thread in program order; re-sorting by timestamp would break that.
    writer.write_str("{\"displayTimeUnit\":\"ms\",\"traceEvents\":[")?;
    let mut first = true;
    for event in events {
        if !first {
            writer.write_char(',')?;
        }
        first = false;
        writer.write_str(&event)?;
    }
    writer.write_str("]}")?;
    Ok(())
}

/// Writes Chrome Trace Event JSON to a file path.
pub fn write_chrome_trace_file(snapshot: &Snapshot, path: impl AsRef<Path>) -> std::io::Result<()> {
    fs::write(path, to_chrome_trace_json(snapshot))
}

/// Track id used for aggregates and capture metadata.
///
/// Runtime-assigned thread ids start at `1`, so `0` never collides with a
/// recording thread.
const AGGREGATE_TID: u64 = 0;

fn build_metadata_event(name: &str, pid: u64, tid: u64, arg_key: &str, arg_value: &str) -> String {
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
    json
}

fn build_capture_window_event(snapshot: &Snapshot) -> String {
    let mut json = String::new();
    json.push('{');
    write_json_field_str(&mut json, "name", "footfall_capture")
        .expect("string write should succeed");
    json.push(',');
    write_json_field_str(&mut json, "cat", "footfall").expect("string write should succeed");
    json.push(',');
    write_json_field_str(&mut json, "ph", "i").expect("string write should succeed");
    json.push_str(",\"s\":\"g\",");
    write_json_field_u64(&mut json, "ts", snapshot.captured_at_micros)
        .expect("string write should succeed");
    json.push(',');
    write_json_field_u64(&mut json, "pid", 1).expect("string write should succeed");
    json.push(',');
    write_json_field_u64(&mut json, "tid", AGGREGATE_TID).expect("string write should succeed");
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
    json
}

fn build_counter_event(
    snapshot: &Snapshot,
    counter: &CounterSnapshot,
) -> Result<String, fmt::Error> {
    let mut json = String::new();
    let context = snapshot
        .context(counter.context_id)
        .map(|context| context.context)
        .unwrap_or_else(Context::root);

    // Chrome keys counter tracks by name within a process, so the context is
    // part of the name to keep one track per descriptor and context.
    json.push('{');
    write_json_field_str(
        &mut json,
        "name",
        &counter_track_name(counter.name, context),
    )?;
    json.push(',');
    write_json_field_str(
        &mut json,
        "cat",
        match counter.kind {
            CounterKind::Counter => "counter",
            CounterKind::Gauge => "gauge",
        },
    )?;
    json.push(',');
    write_json_field_str(&mut json, "ph", "C")?;
    json.push(',');
    write_json_field_u64(&mut json, "ts", counter.updated_at_micros)?;
    json.push(',');
    write_json_field_u64(&mut json, "pid", 1)?;
    json.push(',');
    write_json_field_u64(&mut json, "tid", AGGREGATE_TID)?;
    // Counter args become plotted series, so they carry only the value.
    json.push_str(",\"args\":{");
    write_json_field_u64(&mut json, counter.unit.as_str(), counter.value)?;
    json.push_str("}}");

    Ok(json)
}

fn counter_track_name(name: &str, context: Context) -> String {
    if context.is_empty() {
        return String::from(name);
    }
    format!("{name} [{}]", track_name(context))
}

fn build_histogram_event(
    snapshot: &Snapshot,
    histogram: &HistogramSnapshot,
) -> Result<String, fmt::Error> {
    let context = snapshot
        .context(histogram.context_id)
        .map(|context| context.context)
        .unwrap_or_else(Context::root);
    let mut json = String::new();
    json.push('{');
    write_json_field_str(&mut json, "name", histogram.name)?;
    json.push(',');
    write_json_field_str(&mut json, "cat", "histogram")?;
    json.push(',');
    write_json_field_str(&mut json, "ph", "i")?;
    json.push_str(",\"s\":\"t\",");
    write_json_field_u64(&mut json, "ts", histogram.updated_at_micros)?;
    json.push(',');
    write_json_field_u64(&mut json, "pid", 1)?;
    json.push(',');
    write_json_field_u64(&mut json, "tid", AGGREGATE_TID)?;
    json.push_str(",\"args\":{");
    write_json_field_u64(&mut json, "count", histogram.count)?;
    json.push(',');
    write_json_field_u64(&mut json, "sum", histogram.sum)?;
    json.push(',');
    write_json_field_u64(
        &mut json,
        "min",
        if histogram.count == 0 {
            0
        } else {
            histogram.min
        },
    )?;
    json.push(',');
    write_json_field_u64(&mut json, "max", histogram.max)?;
    json.push(',');
    write_json_field_u64(&mut json, "p50", histogram_percentile(histogram, 0.50))?;
    json.push(',');
    write_json_field_u64(&mut json, "p90", histogram_percentile(histogram, 0.90))?;
    json.push(',');
    write_json_field_u64(&mut json, "p99", histogram_percentile(histogram, 0.99))?;
    json.push_str(",\"unit\":");
    write_json_string(&mut json, histogram.unit.as_str())?;
    json.push_str(",\"context\":");
    write_context(&mut json, context)?;
    json.push_str("}}");
    Ok(json)
}

/// Estimates a percentile from power-of-two buckets.
///
/// Returns the upper bound of the bucket containing the requested rank,
/// clamped to the observed maximum.
fn histogram_percentile(histogram: &HistogramSnapshot, quantile: f64) -> u64 {
    if histogram.count == 0 {
        return 0;
    }
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "rank estimation tolerates precision loss at extreme counts"
    )]
    let rank = ((histogram.count as f64) * quantile).ceil().max(1.0) as u64;
    let mut cumulative = 0_u64;
    for (index, bucket) in histogram.buckets.iter().enumerate() {
        cumulative = cumulative.saturating_add(*bucket);
        if cumulative >= rank {
            let upper = if index == 0 {
                0
            } else {
                u64::MAX >> (u64::BITS as usize - index)
            };
            return upper.min(histogram.max);
        }
    }
    histogram.max
}

fn build_internal_counter_event(at_micros: u64, name: &str, value: u64) -> String {
    let mut json = String::new();
    json.push('{');
    write_json_field_str(&mut json, "name", name).expect("string write should succeed");
    json.push(',');
    write_json_field_str(&mut json, "cat", "footfall").expect("string write should succeed");
    json.push(',');
    write_json_field_str(&mut json, "ph", "C").expect("string write should succeed");
    json.push(',');
    write_json_field_u64(&mut json, "ts", at_micros).expect("string write should succeed");
    json.push(',');
    write_json_field_u64(&mut json, "pid", 1).expect("string write should succeed");
    json.push(',');
    write_json_field_u64(&mut json, "tid", AGGREGATE_TID).expect("string write should succeed");
    json.push_str(",\"args\":{\"value\":");
    json.write_fmt(format_args!("{value}"))
        .expect("string write should succeed");
    json.push_str("}}");
    json
}

/// Builds one timed observation, or returns `None` for a scope end whose
/// begin is not in the snapshot.
fn build_timed_observation(
    snapshot: &Snapshot,
    event: &TimedObservation,
    scope_tracks: &mut HashMap<ScopeToken, u64>,
) -> Result<Option<String>, fmt::Error> {
    match &event.observation {
        Observation::ScopeBegin {
            name,
            context_id,
            token,
            args,
            ..
        } => {
            scope_tracks.insert(*token, event.thread_id);
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
            write_json_field_u64(&mut json, "tid", event.thread_id)?;
            json.push_str(",\"args\":{");
            write_json_field_u64(&mut json, "context_id", context_id.get())?;
            if !args.is_empty() {
                json.push_str(",\"values\":");
                write_args(&mut json, *args)?;
            }
            json.push_str(",\"context\":");
            write_context(&mut json, context)?;
            json.push_str("}}");
            Ok(Some(json))
        }
        Observation::ScopeEnd { token } => {
            // Close the slice on its begin's track so B/E pairs stay matched
            // even when a scope ends on another thread. An end without a
            // begin in this snapshot would close an unrelated slice.
            let Some(tid) = scope_tracks.remove(token) else {
                return Ok(None);
            };
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
            Ok(Some(json))
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
        )
        .map(Some),
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
        )
        .map(Some),
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
        )
        .map(Some),
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
            write_json_field_u64(&mut json, "tid", event.thread_id)?;
            json.push_str(",\"args\":{");
            write_json_field_u64(&mut json, "context_id", context_id.get())?;
            json.push_str(",\"context\":");
            write_context(&mut json, context)?;
            json.push_str("}}");
            Ok(Some(json))
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
            write_json_field_u64(&mut json, "tid", event.thread_id)?;
            json.push(',');
            write_json_field_u64(&mut json, "id", flow_id.get())?;
            json.push_str(",\"args\":{");
            write_json_field_u64(&mut json, "context_id", context_id.get())?;
            json.push_str(",\"context\":");
            write_context(&mut json, context)?;
            json.push_str("}}");
            Ok(Some(json))
        }
        Observation::Message {
            name,
            context_id,
            text,
            args,
            ..
        } => {
            let context = snapshot
                .context(*context_id)
                .map(|context| context.context)
                .unwrap_or_else(Context::root);
            let mut json = String::new();
            json.push('{');
            write_json_field_str(&mut json, "name", name)?;
            json.push(',');
            write_json_field_str(&mut json, "cat", "message")?;
            json.push(',');
            write_json_field_str(&mut json, "ph", "i")?;
            json.push_str(",\"s\":\"t\",");
            write_json_field_u64(&mut json, "ts", event.at_micros)?;
            json.push(',');
            write_json_field_u64(&mut json, "pid", 1)?;
            json.push(',');
            write_json_field_u64(&mut json, "tid", event.thread_id)?;
            json.push_str(",\"args\":{");
            write_json_field_u64(&mut json, "context_id", context_id.get())?;
            json.push(',');
            write_json_field_str(&mut json, "text", text)?;
            if !args.is_empty() {
                json.push_str(",\"values\":");
                write_args(&mut json, *args)?;
            }
            json.push_str(",\"context\":");
            write_context(&mut json, context)?;
            json.push_str("}}");
            Ok(Some(json))
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
) -> Result<String, fmt::Error> {
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
    write_json_field_u64(&mut json, "tid", thread_id)?;
    json.push_str(",\"args\":{");
    write_json_field_u64(&mut json, "context_id", context_id.get())?;
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
    Ok(json)
}

fn write_args(writer: &mut impl Write, args: footfall_core::Args) -> fmt::Result {
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
    use footfall_core::{
        Args, Context, ContextId, CounterDesc, CounterDescId, FlowId, FlowPhase, LabelKey,
        MarkerKind, MarkerPhase, ScopeToken, Unit,
    };
    use footfall_runtime::{
        ContextSnapshot, CounterKind, CounterSnapshot, Observation, Snapshot, TimedObservation,
    };

    #[test]
    fn exporter_emits_metadata_context_tracks_and_scope_pairing() {
        let snapshot = Snapshot {
            metadata: footfall_runtime::SnapshotMetadata {
                process_name: String::from("footfall"),
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
            histograms: vec![],
            events: vec![
                TimedObservation {
                    at_micros: 20,
                    sequence: 0,
                    thread_id: 3,
                    observation: Observation::ScopeBegin {
                        desc_id: footfall_core::ScopeDescId::new(2),
                        name: "record",
                        context_id: ContextId::new(1),
                        token: ScopeToken::new(9),
                        args: Args::none().with("items", 3_u32),
                    },
                },
                TimedObservation {
                    at_micros: 22,
                    sequence: 1,
                    thread_id: 3,
                    observation: Observation::ScopeEnd {
                        token: ScopeToken::new(9),
                    },
                },
                TimedObservation {
                    at_micros: 24,
                    sequence: 2,
                    thread_id: 3,
                    observation: Observation::FrameMarker {
                        kind: MarkerKind::Frame,
                        phase: MarkerPhase::Begin,
                        context_id: ContextId::new(1),
                    },
                },
                TimedObservation {
                    at_micros: 26,
                    sequence: 3,
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
        assert!(json.contains("\"tid\":3"));
        assert!(json.contains("\"name\":\"thread 3\""));
        assert!(json.contains("\"upload_bytes [frame=7 / pass=1]\""));
        assert!(json.contains("\"ph\":\"B\""));
        assert!(json.contains("\"values\":{\"items\":3}"));
        assert!(json.contains("\"ph\":\"E\""));
        assert!(json.contains("\"ph\":\"s\""));
        assert!(json.contains("\"footfall_capture\""));
        assert!(json.contains("\"footfall_dropped_contexts\""));
    }

    fn snapshot_with(counters: Vec<CounterSnapshot>, events: Vec<TimedObservation>) -> Snapshot {
        Snapshot {
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
            counters,
            events,
            ..Snapshot::default()
        }
    }

    fn timed(
        sequence: u64,
        at_micros: u64,
        thread_id: u64,
        observation: Observation,
    ) -> TimedObservation {
        TimedObservation {
            at_micros,
            sequence,
            thread_id,
            observation,
        }
    }

    fn begin(name: &'static str, token: u64) -> Observation {
        Observation::ScopeBegin {
            desc_id: footfall_core::ScopeDescId::new(1),
            name,
            context_id: ContextId::new(1),
            token: ScopeToken::new(token),
            args: Args::none(),
        }
    }

    fn end(token: u64) -> Observation {
        Observation::ScopeEnd {
            token: ScopeToken::new(token),
        }
    }

    #[test]
    fn scopes_use_recording_thread_tracks() {
        // Two threads share one context; each scope must stay on its own
        // thread's track, and an end recorded on another thread must close
        // the slice on the track where it began.
        let snapshot = snapshot_with(
            vec![],
            vec![
                timed(0, 10, 1, begin("left", 1)),
                timed(1, 11, 2, begin("right", 2)),
                timed(2, 12, 1, end(1)),
                timed(3, 13, 2, end(2)),
                timed(4, 14, 1, begin("hop", 3)),
                timed(5, 15, 2, end(3)),
            ],
        );

        let json = to_chrome_trace_json(&snapshot);
        assert!(json.contains("\"name\":\"thread 1\""));
        assert!(json.contains("\"name\":\"thread 2\""));
        assert!(json.contains(
            "\"name\":\"left\",\"cat\":\"scope\",\"ph\":\"B\",\"ts\":10,\"pid\":1,\"tid\":1"
        ));
        assert!(json.contains(
            "\"name\":\"right\",\"cat\":\"scope\",\"ph\":\"B\",\"ts\":11,\"pid\":1,\"tid\":2"
        ));
        assert!(json.contains("{\"ph\":\"E\",\"ts\":12,\"pid\":1,\"tid\":1}"));
        assert!(json.contains("{\"ph\":\"E\",\"ts\":13,\"pid\":1,\"tid\":2}"));
        assert!(
            json.contains("{\"ph\":\"E\",\"ts\":15,\"pid\":1,\"tid\":1}"),
            "a scope ended on another thread should close on its begin track"
        );
        assert!(json.contains("\"context_id\":1"));
    }

    #[test]
    fn same_timestamp_events_keep_snapshot_order() {
        let snapshot = snapshot_with(
            vec![],
            vec![
                timed(0, 5, 1, begin("first", 1)),
                timed(1, 5, 1, end(1)),
                timed(2, 5, 1, begin("second", 2)),
                timed(3, 5, 1, end(2)),
            ],
        );

        let json = to_chrome_trace_json(&snapshot);
        let first_end = json
            .find("\"ph\":\"E\"")
            .expect("first end should be exported");
        let second_begin = json
            .find("\"name\":\"second\"")
            .expect("second begin should be exported");
        assert!(
            first_end < second_begin,
            "the first scope must end before the second begins"
        );
    }

    #[test]
    fn unmatched_scope_end_is_skipped_and_counted() {
        let snapshot = snapshot_with(vec![], vec![timed(0, 5, 1, end(42))]);

        let json = to_chrome_trace_json(&snapshot);
        assert!(!json.contains("\"ph\":\"E\""));
        assert!(json.contains("\"name\":\"footfall_unmatched_scope_ends\""));
        assert!(json.contains("\"args\":{\"value\":1}"));
    }

    #[test]
    fn counters_are_keyed_per_descriptor_and_context() {
        let counter = |context_id: u64, value: u64| CounterSnapshot {
            desc_id: CounterDescId::new(1),
            name: "ops",
            unit: Unit::Count,
            context_id: ContextId::new(context_id),
            kind: CounterKind::Counter,
            value,
            updated_at_micros: 10,
        };
        let snapshot = snapshot_with(vec![counter(0, 5), counter(1, 9)], vec![]);

        let json = to_chrome_trace_json(&snapshot);
        assert!(json.contains("\"name\":\"ops\",\"cat\":\"counter\",\"ph\":\"C\""));
        assert!(json.contains("\"name\":\"ops [frame=7]\",\"cat\":\"counter\",\"ph\":\"C\""));
        assert!(
            json.contains("\"args\":{\"count\":5}") && json.contains("\"args\":{\"count\":9}"),
            "counter args should carry only the numeric value"
        );
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
