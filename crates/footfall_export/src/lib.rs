// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Export bridges for Footfall snapshots.
//!
//! The exporters take an [`OwnedSnapshot`], the form consumers receive from
//! a live session or a capture file. Convert a runtime snapshot with
//! `.into()`:
//!
//! ```
//! use footfall_runtime::Runtime;
//!
//! let runtime = Runtime::default();
//! let json = footfall_export::to_chrome_trace_json(&runtime.snapshot().into());
//! assert!(json.starts_with('{'));
//! ```

use footfall_core::{
    ContextId, DropReason, FlowPhase, FrameNumber, FramePointKind, FrameStream, MarkerKind,
    MarkerPhase, ResourceId, ScopeToken, TimelineId, TimelineKind, Unit,
};
use footfall_runtime::CounterKind;
use footfall_session::{
    Clocks, OwnedArgEntry, OwnedClockTicks, OwnedContextEntry, OwnedCounterSnapshot,
    OwnedHistogramSnapshot, OwnedLabelValue, OwnedObservation, OwnedSnapshot,
    OwnedTimedObservation, OwnedTimelineWork,
};
use hashbrown::HashMap;

mod perfetto;

pub use perfetto::to_perfetto_trace;
use std::{
    collections::{BTreeMap, BTreeSet},
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
/// named after the descriptor unit. A `footfall_contexts` event on track `0`
/// lists every context by id with its labels as typed JSON values, so tools
/// can map track names and `context_id` arguments back to structured labels.
/// A scope whose descriptor records where it is declared carries a
/// `location` argument with its `function`, `file`, and `line`.
#[must_use]
pub fn to_chrome_trace_json(snapshot: &OwnedSnapshot) -> String {
    let mut output = String::new();
    write_chrome_trace_json(snapshot, &mut output)
        .expect("writing Chrome Trace JSON to a string should not fail");
    output
}

/// Writes a snapshot as Chrome Trace Event JSON.
pub fn write_chrome_trace_json(snapshot: &OwnedSnapshot, writer: &mut impl Write) -> fmt::Result {
    let mut events = Vec::new();

    events.push(build_metadata_event(
        "process_name",
        trace_pid(snapshot),
        0,
        "name",
        snapshot.metadata().process_name(),
    ));
    if let Some(session_name) = snapshot.metadata().session_name() {
        events.push(build_metadata_event(
            "process_labels",
            trace_pid(snapshot),
            0,
            "labels",
            session_name,
        ));
    }
    events.push(build_metadata_event(
        "thread_name",
        trace_pid(snapshot),
        AGGREGATE_TID,
        "name",
        "footfall",
    ));
    let threads: BTreeSet<u64> = snapshot
        .events()
        .iter()
        .map(|event| event.thread_id())
        .collect();
    // Tracks that are not threads take the ids after every thread's: Perfetto
    // keeps only 32 bits of a thread id, so they must stay small.
    let mut next_tid = threads.last().map_or(AGGREGATE_TID, |last| *last) + 1;
    let mut track_tid = || {
        let tid = next_tid;
        next_tid += 1;
        tid
    };
    for thread_id in threads {
        let name = snapshot
            .thread(thread_id)
            .and_then(|thread| thread.name())
            .map_or_else(|| format!("thread {thread_id}"), String::from);
        events.push(build_metadata_event(
            "thread_name",
            trace_pid(snapshot),
            thread_id,
            "name",
            &name,
        ));
    }
    // Each timeline that is not a CPU thread, such as a GPU queue, gets a
    // track of its own.
    let timelines: BTreeMap<TimelineId, (&str, TimelineKind)> = snapshot
        .events()
        .iter()
        .filter_map(|event| match event.observation() {
            OwnedObservation::TimelineSpan { span } => Some((
                span.timeline_id(),
                (span.timeline_name(), span.timeline_kind()),
            )),
            _ => None,
        })
        .collect();
    let timeline_tids: BTreeMap<TimelineId, u64> = timelines
        .keys()
        .map(|timeline_id| (*timeline_id, track_tid()))
        .collect();
    for (timeline_id, (name, kind)) in &timelines {
        events.push(build_metadata_event(
            "thread_name",
            trace_pid(snapshot),
            timeline_tids[timeline_id],
            "name",
            &format!("{} {name}", timeline_kind_name(*kind)),
        ));
    }
    // Each frame stream the platform offered opportunities to gets a track
    // of them, like a vsync row.
    let opportunity_streams: BTreeMap<FrameStream, u64> = snapshot
        .events()
        .iter()
        .filter_map(|event| match event.observation() {
            OwnedObservation::FrameOpportunity { stream, .. } => Some(*stream),
            _ => None,
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|stream| (stream, track_tid()))
        .collect();
    let clocks = Clocks::from_snapshot(snapshot);
    let frames = paced_frames(snapshot, &clocks);
    // In the order a frame meets them: built, due, then shown.
    let frame_tids: BTreeMap<FrameStream, FrameTracks> = frames
        .keys()
        .map(|stream| {
            let built = track_tid();
            let targets = track_tid();
            let on_screen = track_tid();
            (
                *stream,
                FrameTracks {
                    built,
                    targets,
                    on_screen,
                },
            )
        })
        .collect();
    for (stream, tracks) in &frame_tids {
        for (tid, what) in [
            (tracks.built, "frames built"),
            (tracks.targets, "frame targets"),
            (tracks.on_screen, "frames on screen"),
        ] {
            events.push(build_metadata_event(
                "thread_name",
                trace_pid(snapshot),
                tid,
                "name",
                &format!("{what}, stream {stream}"),
            ));
        }
    }
    for (stream, tid) in &opportunity_streams {
        events.push(build_metadata_event(
            "thread_name",
            trace_pid(snapshot),
            *tid,
            "name",
            &format!("frame opportunities, stream {stream}"),
        ));
    }
    events.push(build_capture_window_event(snapshot));
    events.push(build_contexts_event(snapshot)?);

    for counter in snapshot.counters() {
        events.push(build_counter_event(snapshot, counter)?);
    }

    for histogram in snapshot.histograms() {
        events.push(build_histogram_event(snapshot, histogram)?);
    }

    build_frame_tracks(snapshot, &frames, &frame_tids, &mut events)?;
    let mut scope_tracks = HashMap::<ScopeToken, u64>::new();
    let mut unmatched_scope_ends = 0_u64;
    let mut unplaced_timeline_spans = 0_u64;
    let mut unplaced_frame_opportunities = 0_u64;
    for event in snapshot.events() {
        let exported = match event.observation() {
            OwnedObservation::TimelineSpan { span } => build_timeline_span(
                snapshot,
                &clocks,
                timeline_tids[&span.timeline_id()],
                span,
                &mut events,
            )?,
            OwnedObservation::FrameOpportunity {
                stream,
                time,
                interval,
                ..
            } => build_frame_opportunity(
                snapshot,
                &clocks,
                opportunity_streams[stream],
                time,
                *interval,
            )?,
            _ => build_timed_observation(snapshot, event, &mut scope_tracks)?,
        };
        match exported {
            Exported::Event(json) => events.push(json),
            Exported::UnmatchedScopeEnd => unmatched_scope_ends += 1,
            Exported::UnplacedTimelineSpan => unplaced_timeline_spans += 1,
            Exported::UnplacedFrameOpportunity => unplaced_frame_opportunities += 1,
            Exported::Skipped => {}
        }
    }

    for (name, count) in [
        ("footfall_unmatched_scope_ends", unmatched_scope_ends),
        ("footfall_unplaced_timeline_spans", unplaced_timeline_spans),
        (
            "footfall_unplaced_frame_opportunities",
            unplaced_frame_opportunities,
        ),
    ] {
        if count > 0 {
            events.push(build_internal_counter_event(
                snapshot,
                snapshot.captured_at_nanos(),
                name,
                count,
            ));
        }
    }

    let drops = snapshot.drops();
    for (name, count) in [
        ("footfall_refused_contexts", drops.refused_contexts()),
        ("footfall_unattributed", drops.unattributed()),
        (
            "footfall_updated_after_release",
            drops.updated_after_release(),
        ),
        ("footfall_dropped_counter_updates", drops.counter_updates()),
        ("footfall_evicted_events", drops.evicted_events()),
        ("footfall_retired_rows", drops.retired_rows()),
    ] {
        if count > 0 {
            events.push(build_internal_counter_event(
                snapshot,
                snapshot.captured_at_nanos(),
                name,
                count,
            ));
        }
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
pub fn write_chrome_trace_file(
    snapshot: &OwnedSnapshot,
    path: impl AsRef<Path>,
) -> std::io::Result<()> {
    fs::write(path, to_chrome_trace_json(snapshot))
}

/// Writes a Perfetto trace, in Perfetto's protobuf format, to a file path;
/// see [`to_perfetto_trace`].
pub fn write_perfetto_trace_file(
    snapshot: &OwnedSnapshot,
    path: impl AsRef<Path>,
) -> std::io::Result<()> {
    fs::write(path, to_perfetto_trace(snapshot))
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

/// Writes a frame's number under `key`, followed by its stream as
/// `"stream"` when it is not the default stream.
fn write_frame_fields(json: &mut String, key: &str, frame: FrameNumber) -> fmt::Result {
    write_json_field_u64(json, key, frame.number())?;
    if frame.stream() != FrameStream::DEFAULT {
        json.push(',');
        write_json_field_str(json, "stream", &frame.stream().to_string())?;
    }
    Ok(())
}

fn build_capture_window_event(snapshot: &OwnedSnapshot) -> String {
    let mut json = String::new();
    json.push('{');
    write_json_field_str(&mut json, "name", "footfall_capture")
        .expect("string write should succeed");
    json.push(',');
    write_json_field_str(&mut json, "cat", "footfall").expect("string write should succeed");
    json.push(',');
    write_json_field_str(&mut json, "ph", "i").expect("string write should succeed");
    json.push_str(",\"s\":\"g\",");
    write_json_field_time(
        &mut json,
        "ts",
        wall_nanos(snapshot, snapshot.captured_at_nanos()),
    )
    .expect("string write should succeed");
    json.push(',');
    write_json_field_u64(&mut json, "pid", trace_pid(snapshot))
        .expect("string write should succeed");
    json.push(',');
    write_json_field_u64(&mut json, "tid", AGGREGATE_TID).expect("string write should succeed");
    json.push_str(",\"args\":{");
    let metadata = snapshot.metadata();
    let capture = metadata.capture();
    let window = snapshot.window();
    // The instance id is a full 64-bit value, which JSON numbers cannot
    // carry exactly, so it travels as hex text.
    write_json_field_str(
        &mut json,
        "instance_id",
        &format!("{:016x}", metadata.instance_id()),
    )
    .expect("string write should succeed");
    json.push(',');
    write_json_field_str(&mut json, "producer_version", metadata.producer_version())
        .expect("string write should succeed");
    json.push(',');
    write_json_field_str(&mut json, "level", capture.level().as_str())
        .expect("string write should succeed");
    // Totals are always present, so a reader can tell "nothing was dropped"
    // from "this export does not say".
    let drops = snapshot.drops();
    for (name, value) in [
        ("captured_at_nanos", snapshot.captured_at_nanos()),
        (
            "wall_clock_anchor_nanos",
            metadata.wall_clock_anchor_nanos(),
        ),
        ("counter_capacity", usize_to_u64(capture.counter_capacity())),
        ("event_capacity", usize_to_u64(capture.event_capacity())),
        ("context_capacity", usize_to_u64(capture.context_capacity())),
        ("window_since", window.since()),
        ("window_next", window.next()),
        ("window_missed", window.missed()),
        ("refused_contexts", drops.refused_contexts()),
        ("unattributed", drops.unattributed()),
        ("updated_after_release", drops.updated_after_release()),
        ("dropped_counter_updates", drops.counter_updates()),
        ("evicted_events", drops.evicted_events()),
        ("retired_rows", drops.retired_rows()),
        ("process_id", u64::from(metadata.process_id())),
        ("counter_samples", u64::from(capture.counter_samples())),
        ("compiled_out", u64::from(capture.compiled_out())),
    ] {
        json.push(',');
        write_json_field_u64(&mut json, name, value).expect("string write should succeed");
    }
    json.push_str("}}");
    json
}

fn build_contexts_event(snapshot: &OwnedSnapshot) -> Result<String, fmt::Error> {
    let mut json = String::new();
    json.push('{');
    write_json_field_str(&mut json, "name", "footfall_contexts")?;
    json.push(',');
    write_json_field_str(&mut json, "cat", "footfall")?;
    json.push(',');
    write_json_field_str(&mut json, "ph", "i")?;
    json.push_str(",\"s\":\"g\",");
    write_json_field_time(
        &mut json,
        "ts",
        wall_nanos(snapshot, snapshot.captured_at_nanos()),
    )?;
    json.push(',');
    write_json_field_u64(&mut json, "pid", trace_pid(snapshot))?;
    json.push(',');
    write_json_field_u64(&mut json, "tid", AGGREGATE_TID)?;
    json.push_str(",\"args\":{");
    for (index, context) in snapshot.contexts().iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        write_json_string(&mut json, &context.id().get().to_string())?;
        json.push(':');
        write_context(&mut json, context.entries())?;
    }
    json.push_str("}}");
    Ok(json)
}

/// The trace `pid`: the producer's process id, so traces from several
/// processes stay apart, or `1` when there is none.
fn trace_pid(snapshot: &OwnedSnapshot) -> u64 {
    match snapshot.metadata().process_id() {
        0 => 1,
        process_id => u64::from(process_id),
    }
}

/// Converts a time since runtime creation into wall-clock nanoseconds
/// since the Unix epoch, so traces from several processes share one
/// timeline.
fn wall_nanos(snapshot: &OwnedSnapshot, at_nanos: u64) -> u64 {
    snapshot
        .metadata()
        .wall_clock_anchor_nanos()
        .saturating_add(at_nanos)
}

/// Writes a counter value as its series: ratios, stored in parts per
/// million, as the fraction they stand for, and everything else as is.
fn write_counter_value(writer: &mut impl Write, unit: Unit, value: u64) -> fmt::Result {
    write_json_string(writer, unit.as_str())?;
    writer.write_char(':')?;
    if unit == Unit::Ratio {
        #[expect(
            clippy::cast_precision_loss,
            reason = "ratios above 2^53 parts per million do not occur"
        )]
        let fraction = value as f64 / 1_000_000.0;
        writer.write_fmt(format_args!("{fraction}"))
    } else {
        writer.write_fmt(format_args!("{value}"))
    }
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn build_counter_event(
    snapshot: &OwnedSnapshot,
    counter: &OwnedCounterSnapshot,
) -> Result<String, fmt::Error> {
    let mut json = String::new();
    let context = snapshot
        .context(counter.context_id())
        .map_or(&[][..], |context| context.entries());

    // Chrome keys counter tracks by name within a process, so the context is
    // part of the name to keep one track per descriptor and context.
    json.push('{');
    write_json_field_str(
        &mut json,
        "name",
        &counter_track_name(counter.name(), context),
    )?;
    json.push(',');
    write_json_field_str(
        &mut json,
        "cat",
        match counter.kind() {
            CounterKind::Gauge => "gauge",
            _ => "counter",
        },
    )?;
    json.push(',');
    write_json_field_str(&mut json, "ph", "C")?;
    json.push(',');
    write_json_field_time(
        &mut json,
        "ts",
        wall_nanos(snapshot, counter.updated_at_nanos()),
    )?;
    json.push(',');
    write_json_field_u64(&mut json, "pid", trace_pid(snapshot))?;
    json.push(',');
    write_json_field_u64(&mut json, "tid", AGGREGATE_TID)?;
    // Counter args become plotted series, so they carry only the value.
    json.push_str(",\"args\":{");
    write_counter_value(&mut json, counter.unit(), counter.value())?;
    json.push_str("}}");

    Ok(json)
}

fn counter_track_name(name: &str, context: &[OwnedContextEntry]) -> String {
    if context.is_empty() {
        return String::from(name);
    }
    format!("{name} [{}]", track_name(context))
}

fn build_histogram_event(
    snapshot: &OwnedSnapshot,
    histogram: &OwnedHistogramSnapshot,
) -> Result<String, fmt::Error> {
    let context = snapshot
        .context(histogram.context_id())
        .map_or(&[][..], |context| context.entries());
    let mut json = String::new();
    json.push('{');
    write_json_field_str(&mut json, "name", histogram.name())?;
    json.push(',');
    write_json_field_str(&mut json, "cat", "histogram")?;
    json.push(',');
    write_json_field_str(&mut json, "ph", "i")?;
    json.push_str(",\"s\":\"t\",");
    write_json_field_time(
        &mut json,
        "ts",
        wall_nanos(snapshot, histogram.updated_at_nanos()),
    )?;
    json.push(',');
    write_json_field_u64(&mut json, "pid", trace_pid(snapshot))?;
    json.push(',');
    write_json_field_u64(&mut json, "tid", AGGREGATE_TID)?;
    json.push_str(",\"args\":{");
    write_json_field_u64(&mut json, "count", histogram.count())?;
    json.push(',');
    write_json_field_u64(&mut json, "sum", histogram.sum())?;
    json.push(',');
    write_json_field_u64(
        &mut json,
        "min",
        if histogram.count() == 0 {
            0
        } else {
            histogram.min()
        },
    )?;
    json.push(',');
    write_json_field_u64(&mut json, "max", histogram.max())?;
    json.push(',');
    write_json_field_u64(&mut json, "p50", histogram_percentile(histogram, 0.50))?;
    json.push(',');
    write_json_field_u64(&mut json, "p90", histogram_percentile(histogram, 0.90))?;
    json.push(',');
    write_json_field_u64(&mut json, "p99", histogram_percentile(histogram, 0.99))?;
    json.push_str(",\"unit\":");
    write_json_string(&mut json, histogram.unit().as_str())?;
    json.push_str(",\"context\":");
    write_context(&mut json, context)?;
    json.push_str("}}");
    Ok(json)
}

/// Estimates a percentile from power-of-two buckets.
///
/// Returns the upper bound of the bucket containing the requested rank,
/// clamped to the observed maximum.
fn histogram_percentile(histogram: &OwnedHistogramSnapshot, quantile: f64) -> u64 {
    if histogram.count() == 0 {
        return 0;
    }
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "rank estimation tolerates precision loss at extreme counts"
    )]
    let rank = ((histogram.count() as f64) * quantile).ceil().max(1.0) as u64;
    let mut cumulative = 0_u64;
    for (index, bucket) in histogram.buckets().iter().enumerate() {
        cumulative = cumulative.saturating_add(*bucket);
        if cumulative >= rank {
            let upper = if index == 0 {
                0
            } else {
                u64::MAX >> (u64::BITS as usize - index)
            };
            return upper.min(histogram.max());
        }
    }
    histogram.max()
}

fn build_internal_counter_event(
    snapshot: &OwnedSnapshot,
    at_nanos: u64,
    name: &str,
    value: u64,
) -> String {
    let mut json = String::new();
    json.push('{');
    write_json_field_str(&mut json, "name", name).expect("string write should succeed");
    json.push(',');
    write_json_field_str(&mut json, "cat", "footfall").expect("string write should succeed");
    json.push(',');
    write_json_field_str(&mut json, "ph", "C").expect("string write should succeed");
    json.push(',');
    write_json_field_time(&mut json, "ts", wall_nanos(snapshot, at_nanos))
        .expect("string write should succeed");
    json.push(',');
    write_json_field_u64(&mut json, "pid", trace_pid(snapshot))
        .expect("string write should succeed");
    json.push(',');
    write_json_field_u64(&mut json, "tid", AGGREGATE_TID).expect("string write should succeed");
    json.push_str(",\"args\":{\"value\":");
    json.write_fmt(format_args!("{value}"))
        .expect("string write should succeed");
    json.push_str("}}");
    json
}

/// What one timed observation became.
enum Exported {
    /// A trace event.
    Event(String),
    /// Nothing: a scope end whose begin is not in the snapshot.
    UnmatchedScopeEnd,
    /// Nothing: a timeline span whose clock has no sync or pair leading to
    /// the producer's clock.
    UnplacedTimelineSpan,
    /// Nothing: a frame opportunity whose clock has no sync or pair leading
    /// to the producer's clock.
    UnplacedFrameOpportunity,
    /// Nothing: an observation kind this exporter does not know.
    Skipped,
}

/// What a stream's frame points say about one frame, placed in the
/// snapshot's time; the latest point of each kind wins.
#[derive(Clone, Copy, Debug, Default)]
struct PacedFrame {
    /// When its CPU work began and ended, from its numbered markers.
    begin: Option<u64>,
    end: Option<u64>,
    scheduled: Option<u64>,
    deadline: Option<u64>,
    target: Option<u64>,
    presented: Option<u64>,
    dropped: Option<DropReason>,
    /// When it asked for a buffer to render into, and when it got one.
    buffer_requested: Option<u64>,
    buffer_acquired: Option<u64>,
}

/// The Chrome trace tracks of one frame stream's frames.
#[derive(Clone, Copy, Debug)]
struct FrameTracks {
    /// Each frame's CPU work, and its deadline.
    built: u64,
    /// Each frame's target.
    targets: u64,
    /// How long each shown frame stayed on screen.
    on_screen: u64,
}

/// Each frame stream's numbered frames, by number, from their numbered
/// frame markers and frame points.
fn paced_frames(
    snapshot: &OwnedSnapshot,
    clocks: &Clocks,
) -> BTreeMap<FrameStream, BTreeMap<u64, PacedFrame>> {
    let mut frames: BTreeMap<FrameStream, BTreeMap<u64, PacedFrame>> = BTreeMap::new();
    for event in snapshot.events() {
        if let OwnedObservation::FrameMarker {
            kind: MarkerKind::Frame,
            phase,
            number: Some(number),
            ..
        } = event.observation()
        {
            let frame = frames
                .entry(number.stream())
                .or_default()
                .entry(number.number())
                .or_default();
            match phase {
                MarkerPhase::Begin => frame.begin = Some(event.at_nanos()),
                MarkerPhase::End => frame.end = Some(event.at_nanos()),
            }
            continue;
        }
        let OwnedObservation::FramePoint {
            number, kind, time, ..
        } = event.observation()
        else {
            continue;
        };
        let at = time
            .as_ref()
            .and_then(|time| clocks.to_runtime(time.clock_id(), time.ticks()))
            .map(|converted| converted.nanos());
        let frame = frames
            .entry(number.stream())
            .or_default()
            .entry(number.number())
            .or_default();
        match kind {
            FramePointKind::Scheduled(_) => frame.scheduled = at.or(frame.scheduled),
            FramePointKind::Deadline => frame.deadline = at.or(frame.deadline),
            FramePointKind::Target => frame.target = at.or(frame.target),
            FramePointKind::Presented => frame.presented = at.or(frame.presented),
            FramePointKind::Dropped(reason) => frame.dropped = Some(*reason),
            FramePointKind::BufferRequested => {
                frame.buffer_requested = at.or(frame.buffer_requested);
            }
            FramePointKind::BufferAcquired => {
                frame.buffer_acquired = at.or(frame.buffer_acquired);
            }
            _ => {}
        }
    }
    // Only streams a frame scheduler reported on: numbered markers alone
    // are already on their threads.
    frames.retain(|_, frames| {
        frames
            .values()
            .any(|frame| frame.target.is_some() || frame.presented.is_some())
    });
    frames
}

/// Builds each stream's frame tracks: a slice per frame from its begin
/// marker to its end marker on the built track, with an instant at its
/// deadline; an instant at each frame's target on the targets track; and a
/// slice per shown frame from its presentation to the next one's on the
/// on-screen track. So a frame built late ends after its deadline, a late
/// frame's slice on screen starts after its target, and a frame left on
/// screen for longer is a longer slice. The targets and on-screen tracks'
/// events never overlap, so Perfetto lays each out on one row. A dropped
/// frame is an instant on the on-screen track at its target.
fn build_frame_tracks(
    snapshot: &OwnedSnapshot,
    frames: &BTreeMap<FrameStream, BTreeMap<u64, PacedFrame>>,
    tids: &BTreeMap<FrameStream, FrameTracks>,
    events: &mut Vec<String>,
) -> fmt::Result {
    for (stream, frames) in frames {
        let FrameTracks {
            built,
            targets: planned,
            on_screen,
        } = tids[stream];
        let instant = |tid: u64, name: &str, number: u64, at: u64, extra: &str| {
            let mut json = String::new();
            json.push('{');
            write_json_field_str(&mut json, "name", name)?;
            json.push(',');
            write_json_field_str(&mut json, "cat", "frame")?;
            json.push(',');
            write_json_field_str(&mut json, "ph", "i")?;
            json.push_str(",\"s\":\"t\",");
            write_json_field_time(&mut json, "ts", wall_nanos(snapshot, at))?;
            json.push(',');
            write_json_field_u64(&mut json, "pid", trace_pid(snapshot))?;
            json.push(',');
            write_json_field_u64(&mut json, "tid", tid)?;
            json.push_str(",\"args\":{");
            write_frame_fields(&mut json, "frame", stream.frame(number))?;
            json.push_str(extra);
            json.push_str("}}");
            Ok::<_, fmt::Error>(json)
        };
        for (&number, frame) in frames {
            if let (Some(begin), Some(end)) = (frame.begin, frame.end) {
                let mut json = String::new();
                json.push('{');
                write_json_field_str(&mut json, "name", &format!("frame {number}"))?;
                json.push(',');
                write_json_field_str(&mut json, "cat", "frame")?;
                json.push(',');
                write_json_field_str(&mut json, "ph", "X")?;
                json.push(',');
                write_json_field_time(&mut json, "ts", wall_nanos(snapshot, begin))?;
                json.push(',');
                write_json_field_time(&mut json, "dur", end.saturating_sub(begin))?;
                json.push(',');
                write_json_field_u64(&mut json, "pid", trace_pid(snapshot))?;
                json.push(',');
                write_json_field_u64(&mut json, "tid", built)?;
                json.push_str(",\"args\":{");
                write_frame_fields(&mut json, "frame", stream.frame(number))?;
                if let Some(deadline) = frame.deadline {
                    // How long before its deadline its work ended, negative
                    // when after, in microseconds.
                    let left = i128::from(deadline) - i128::from(end);
                    write!(
                        json,
                        ",\"left_before_deadline_us\":{:.3}",
                        left as f64 / 1_000.0
                    )?;
                }
                json.push_str("}}");
                events.push(json);
            }
            if let Some(deadline) = frame.deadline {
                events.push(instant(
                    built,
                    &format!("frame {number} deadline"),
                    number,
                    deadline,
                    "",
                )?);
            }
            let Some(target) = frame.target else {
                continue;
            };
            let mut extra = String::new();
            if let Some(scheduled) = frame.scheduled {
                // How long before its target the frame was to begin.
                extra.push(',');
                write_json_field_time(
                    &mut extra,
                    "scheduled_before_target_us",
                    target.saturating_sub(scheduled),
                )?;
            }
            events.push(instant(
                planned,
                &format!("frame {number} target"),
                number,
                target,
                &extra,
            )?);
            if let Some(reason) = frame.dropped {
                let mut extra = String::from(",");
                write_json_field_str(&mut extra, "reason", &reason.to_string())?;
                events.push(instant(
                    on_screen,
                    &format!("frame {number} dropped"),
                    number,
                    target,
                    &extra,
                )?);
            }
        }
        let mut shown: Vec<(u64, u64, Option<u64>)> = frames
            .iter()
            .filter_map(|(&number, frame)| Some((frame.presented?, number, frame.target)))
            .collect();
        shown.sort_unstable();
        for pair in shown.windows(2) {
            let ((presented, number, target), (next, ..)) = (pair[0], pair[1]);
            let mut json = String::new();
            json.push('{');
            write_json_field_str(&mut json, "name", &format!("frame {number}"))?;
            json.push(',');
            write_json_field_str(&mut json, "cat", "frame")?;
            json.push(',');
            write_json_field_str(&mut json, "ph", "X")?;
            json.push(',');
            // Whole microseconds, so one slice's end is exactly the next
            // one's start: Perfetto reads a fractional time as a double,
            // which at wall-clock times keeps only about a quarter of a
            // microsecond, and would read two slices that meet as
            // overlapping.
            let (begin, end) = (
                wall_nanos(snapshot, presented) / 1_000,
                wall_nanos(snapshot, next) / 1_000,
            );
            write_json_field_u64(&mut json, "ts", begin)?;
            json.push(',');
            write_json_field_u64(&mut json, "dur", end - begin)?;
            json.push(',');
            write_json_field_u64(&mut json, "pid", trace_pid(snapshot))?;
            json.push(',');
            write_json_field_u64(&mut json, "tid", on_screen)?;
            json.push_str(",\"args\":{");
            write_frame_fields(&mut json, "frame", stream.frame(number))?;
            if let Some(target) = target {
                // How long after its target it was shown, negative when
                // before, in microseconds.
                let after = i128::from(presented) - i128::from(target);
                write!(json, ",\"after_target_us\":{:.3}", after as f64 / 1_000.0)?;
            }
            json.push_str("}}");
            events.push(json);
        }
    }
    Ok(())
}

/// Builds a frame opportunity as an instant on its stream's track, with its
/// time converted through the snapshot's clocks.
fn build_frame_opportunity(
    snapshot: &OwnedSnapshot,
    clocks: &Clocks,
    tid: u64,
    time: &OwnedClockTicks,
    interval: Option<u64>,
) -> Result<Exported, fmt::Error> {
    let Some(at) = clocks.to_runtime(time.clock_id(), time.ticks()) else {
        return Ok(Exported::UnplacedFrameOpportunity);
    };
    let mut json = String::new();
    json.push('{');
    write_json_field_str(&mut json, "name", "frame opportunity")?;
    json.push(',');
    write_json_field_str(&mut json, "cat", "frame")?;
    json.push(',');
    write_json_field_str(&mut json, "ph", "i")?;
    json.push_str(",\"s\":\"t\",");
    write_json_field_time(&mut json, "ts", wall_nanos(snapshot, at.nanos()))?;
    json.push(',');
    write_json_field_u64(&mut json, "pid", trace_pid(snapshot))?;
    json.push(',');
    write_json_field_u64(&mut json, "tid", tid)?;
    json.push_str(",\"args\":{");
    if let Some(interval) = interval {
        write_json_field_u64(
            &mut json,
            "interval_ns",
            time.period().ticks_to_nanos(interval),
        )?;
    }
    json.push_str("}}");
    Ok(Exported::Event(json))
}

fn timeline_kind_name(kind: TimelineKind) -> &'static str {
    match kind {
        TimelineKind::Gpu => "GPU",
        TimelineKind::Display => "display",
        _ => "timeline",
    }
}

/// Builds a timeline span as a complete slice on its timeline's track, with
/// its times converted through the snapshot's clocks, and the end of its
/// flow, if it has one, into `events`.
fn build_timeline_span(
    snapshot: &OwnedSnapshot,
    clocks: &Clocks,
    tid: u64,
    span: &OwnedTimelineWork,
    events: &mut Vec<String>,
) -> Result<Exported, fmt::Error> {
    let clock = span.begin().clock_id();
    let (Some(begin), Some(end)) = (
        clocks.to_runtime(clock, span.begin().ticks()),
        clocks.to_runtime(clock, span.end()),
    ) else {
        return Ok(Exported::UnplacedTimelineSpan);
    };
    let (begin, end) = (begin.nanos(), end.nanos().max(begin.nanos()));
    let context = snapshot
        .context(span.context_id())
        .map_or(&[][..], |context| context.entries());
    let mut json = String::new();
    json.push('{');
    write_json_field_str(&mut json, "name", span.name())?;
    json.push(',');
    write_json_field_str(&mut json, "cat", "timeline")?;
    json.push(',');
    write_json_field_str(&mut json, "ph", "X")?;
    json.push(',');
    write_json_field_time(&mut json, "ts", wall_nanos(snapshot, begin))?;
    json.push(',');
    write_json_field_time(&mut json, "dur", end - begin)?;
    json.push(',');
    write_json_field_u64(&mut json, "pid", trace_pid(snapshot))?;
    json.push(',');
    write_json_field_u64(&mut json, "tid", tid)?;
    json.push_str(",\"args\":{");
    if let Some(frame) = span.frame() {
        write_frame_fields(&mut json, "frame", frame)?;
        json.push(',');
    }
    write_json_field_u64(&mut json, "context_id", span.context_id().get())?;
    if !span.args().is_empty() {
        json.push_str(",\"values\":");
        write_args(&mut json, span.args())?;
    }
    json.push_str(",\"context\":");
    write_context(&mut json, context)?;
    json.push_str("}}");
    if let Some(flow) = span.flow() {
        let mut end_of_flow = String::new();
        end_of_flow.push('{');
        write_json_field_str(&mut end_of_flow, "name", "flow")?;
        end_of_flow.push(',');
        write_json_field_str(&mut end_of_flow, "cat", "flow")?;
        end_of_flow.push(',');
        write_json_field_str(&mut end_of_flow, "ph", flow_phase_code(FlowPhase::End))?;
        end_of_flow.push_str(",\"bp\":\"e\",");
        write_json_field_time(&mut end_of_flow, "ts", wall_nanos(snapshot, begin))?;
        end_of_flow.push(',');
        write_json_field_u64(&mut end_of_flow, "pid", trace_pid(snapshot))?;
        end_of_flow.push(',');
        write_json_field_u64(&mut end_of_flow, "tid", tid)?;
        end_of_flow.push(',');
        write_json_field_u64(&mut end_of_flow, "id", flow.get())?;
        end_of_flow.push('}');
        events.push(end_of_flow);
    }
    Ok(Exported::Event(json))
}

/// Builds one timed observation.
fn build_timed_observation(
    snapshot: &OwnedSnapshot,
    event: &OwnedTimedObservation,
    scope_tracks: &mut HashMap<ScopeToken, u64>,
) -> Result<Exported, fmt::Error> {
    match event.observation() {
        OwnedObservation::ScopeBegin {
            name,
            location,
            context_id,
            token,
            args,
            ..
        } => {
            scope_tracks.insert(*token, event.thread_id());
            let context = snapshot
                .context(*context_id)
                .map_or(&[][..], |context| context.entries());
            let mut json = String::new();
            json.push('{');
            write_json_field_str(&mut json, "name", name)?;
            json.push(',');
            write_json_field_str(&mut json, "cat", "scope")?;
            json.push(',');
            write_json_field_str(&mut json, "ph", "B")?;
            json.push(',');
            write_json_field_time(&mut json, "ts", wall_nanos(snapshot, event.at_nanos()))?;
            json.push(',');
            write_json_field_u64(&mut json, "pid", trace_pid(snapshot))?;
            json.push(',');
            write_json_field_u64(&mut json, "tid", event.thread_id())?;
            json.push_str(",\"args\":{");
            write_json_field_u64(&mut json, "context_id", context_id.get())?;
            if !args.is_empty() {
                json.push_str(",\"values\":");
                write_args(&mut json, args)?;
            }
            json.push_str(",\"context\":");
            write_context(&mut json, context)?;
            if let Some(location) = location {
                json.push_str(",\"location\":{");
                write_json_field_str(&mut json, "function", location.function())?;
                json.push(',');
                write_json_field_str(&mut json, "file", location.file())?;
                json.push(',');
                write_json_field_u64(&mut json, "line", location.line().into())?;
                json.push('}');
            }
            json.push_str("}}");
            Ok(Exported::Event(json))
        }
        OwnedObservation::ScopeEnd { token } => {
            // Close the slice on its begin's track so B/E pairs stay matched
            // even when a scope ends on another thread. An end without a
            // begin in this snapshot would close an unrelated slice.
            let Some(tid) = scope_tracks.remove(token) else {
                return Ok(Exported::UnmatchedScopeEnd);
            };
            let mut json = String::new();
            json.push('{');
            write_json_field_str(&mut json, "ph", "E")?;
            json.push(',');
            write_json_field_time(&mut json, "ts", wall_nanos(snapshot, event.at_nanos()))?;
            json.push(',');
            write_json_field_u64(&mut json, "pid", trace_pid(snapshot))?;
            json.push(',');
            write_json_field_u64(&mut json, "tid", tid)?;
            json.push('}');
            Ok(Exported::Event(json))
        }
        OwnedObservation::ResourceCreate {
            kind_name,
            resource_id,
            context_id,
            size,
            ..
        } => build_resource_event(
            snapshot,
            event.at_nanos(),
            event.thread_id(),
            "resource_create",
            kind_name,
            *resource_id,
            *context_id,
            Some(*size),
        )
        .map(Exported::Event),
        OwnedObservation::ResourceUpdate {
            kind_name,
            resource_id,
            context_id,
            size,
            ..
        } => build_resource_event(
            snapshot,
            event.at_nanos(),
            event.thread_id(),
            "resource_update",
            kind_name,
            *resource_id,
            *context_id,
            Some(*size),
        )
        .map(Exported::Event),
        OwnedObservation::ResourceDrop {
            kind_name,
            resource_id,
            context_id,
            ..
        } => build_resource_event(
            snapshot,
            event.at_nanos(),
            event.thread_id(),
            "resource_drop",
            kind_name,
            *resource_id,
            *context_id,
            None,
        )
        .map(Exported::Event),
        OwnedObservation::FrameMarker {
            kind,
            phase,
            number,
            context_id,
        } => {
            let context = snapshot
                .context(*context_id)
                .map_or(&[][..], |context| context.entries());
            let mut json = String::new();
            json.push('{');
            write_json_field_str(&mut json, "name", marker_name(*kind, *phase))?;
            json.push(',');
            write_json_field_str(&mut json, "cat", "marker")?;
            json.push(',');
            write_json_field_str(&mut json, "ph", "i")?;
            json.push_str(",\"s\":\"t\",");
            write_json_field_time(&mut json, "ts", wall_nanos(snapshot, event.at_nanos()))?;
            json.push(',');
            write_json_field_u64(&mut json, "pid", trace_pid(snapshot))?;
            json.push(',');
            write_json_field_u64(&mut json, "tid", event.thread_id())?;
            json.push_str(",\"args\":{");
            if let Some(number) = number {
                write_frame_fields(&mut json, "number", *number)?;
                json.push(',');
            }
            write_json_field_u64(&mut json, "context_id", context_id.get())?;
            json.push_str(",\"context\":");
            write_context(&mut json, context)?;
            json.push_str("}}");
            Ok(Exported::Event(json))
        }
        OwnedObservation::Flow {
            flow_id,
            phase,
            context_id,
        } => {
            let context = snapshot
                .context(*context_id)
                .map_or(&[][..], |context| context.entries());
            let mut json = String::new();
            json.push('{');
            write_json_field_str(&mut json, "name", "flow")?;
            json.push(',');
            write_json_field_str(&mut json, "cat", "flow")?;
            json.push(',');
            write_json_field_str(&mut json, "ph", flow_phase_code(*phase))?;
            json.push_str(",\"bp\":\"e\",");
            write_json_field_time(&mut json, "ts", wall_nanos(snapshot, event.at_nanos()))?;
            json.push(',');
            write_json_field_u64(&mut json, "pid", trace_pid(snapshot))?;
            json.push(',');
            write_json_field_u64(&mut json, "tid", event.thread_id())?;
            json.push(',');
            write_json_field_u64(&mut json, "id", flow_id.get())?;
            json.push_str(",\"args\":{");
            write_json_field_u64(&mut json, "context_id", context_id.get())?;
            json.push_str(",\"context\":");
            write_context(&mut json, context)?;
            json.push_str("}}");
            Ok(Exported::Event(json))
        }
        OwnedObservation::Message {
            name,
            context_id,
            text,
            args,
            ..
        } => {
            let context = snapshot
                .context(*context_id)
                .map_or(&[][..], |context| context.entries());
            let mut json = String::new();
            json.push('{');
            write_json_field_str(&mut json, "name", name)?;
            json.push(',');
            write_json_field_str(&mut json, "cat", "message")?;
            json.push(',');
            write_json_field_str(&mut json, "ph", "i")?;
            json.push_str(",\"s\":\"t\",");
            write_json_field_time(&mut json, "ts", wall_nanos(snapshot, event.at_nanos()))?;
            json.push(',');
            write_json_field_u64(&mut json, "pid", trace_pid(snapshot))?;
            json.push(',');
            write_json_field_u64(&mut json, "tid", event.thread_id())?;
            json.push_str(",\"args\":{");
            write_json_field_u64(&mut json, "context_id", context_id.get())?;
            json.push(',');
            write_json_field_str(&mut json, "text", text)?;
            if !args.is_empty() {
                json.push_str(",\"values\":");
                write_args(&mut json, args)?;
            }
            json.push_str(",\"context\":");
            write_context(&mut json, context)?;
            json.push_str("}}");
            Ok(Exported::Event(json))
        }
        OwnedObservation::CounterSample {
            name,
            unit,
            context_id,
            kind,
            value,
            ..
        } => {
            // A point on the same track as the counter's final value, so
            // each counter plots as a series over the capture.
            let context = snapshot
                .context(*context_id)
                .map_or(&[][..], |context| context.entries());
            let mut json = String::new();
            json.push('{');
            write_json_field_str(&mut json, "name", &counter_track_name(name, context))?;
            json.push(',');
            write_json_field_str(
                &mut json,
                "cat",
                match kind {
                    CounterKind::Gauge => "gauge",
                    _ => "counter",
                },
            )?;
            json.push(',');
            write_json_field_str(&mut json, "ph", "C")?;
            json.push(',');
            write_json_field_time(&mut json, "ts", wall_nanos(snapshot, event.at_nanos()))?;
            json.push(',');
            write_json_field_u64(&mut json, "pid", trace_pid(snapshot))?;
            json.push(',');
            write_json_field_u64(&mut json, "tid", AGGREGATE_TID)?;
            json.push_str(",\"args\":{");
            write_counter_value(&mut json, *unit, *value)?;
            json.push_str("}}");
            Ok(Exported::Event(json))
        }
        // Observation kinds this exporter does not know are left out.
        _ => Ok(Exported::Skipped),
    }
}

fn build_resource_event(
    snapshot: &OwnedSnapshot,
    at_nanos: u64,
    thread_id: u64,
    name: &str,
    kind_name: &str,
    resource_id: ResourceId,
    context_id: ContextId,
    size: Option<u64>,
) -> Result<String, fmt::Error> {
    let context = snapshot
        .context(context_id)
        .map_or(&[][..], |context| context.entries());
    let mut json = String::new();
    json.push('{');
    write_json_field_str(&mut json, "name", name)?;
    json.push(',');
    write_json_field_str(&mut json, "cat", kind_name)?;
    json.push(',');
    write_json_field_str(&mut json, "ph", "i")?;
    json.push_str(",\"s\":\"t\",");
    write_json_field_time(&mut json, "ts", wall_nanos(snapshot, at_nanos))?;
    json.push(',');
    write_json_field_u64(&mut json, "pid", trace_pid(snapshot))?;
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

fn write_args(writer: &mut impl Write, args: &[OwnedArgEntry]) -> fmt::Result {
    writer.write_char('{')?;
    let mut first = true;
    for entry in args {
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

fn write_context(writer: &mut impl Write, context: &[OwnedContextEntry]) -> fmt::Result {
    writer.write_char('{')?;
    let mut first = true;
    for entry in context {
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

fn write_label_value(writer: &mut impl Write, value: &OwnedLabelValue) -> fmt::Result {
    match value {
        OwnedLabelValue::Unsigned(value) => writer.write_fmt(format_args!("{value}")),
        OwnedLabelValue::Signed(value) => writer.write_fmt(format_args!("{value}")),
        OwnedLabelValue::Bool(value) => writer.write_str(if *value { "true" } else { "false" }),
        OwnedLabelValue::String(value) => write_json_string(writer, value),
        // Value kinds this exporter does not know are written as null.
        _ => writer.write_str("null"),
    }
}

/// Names a context for a track: `key=value` pairs joined by ` / `, with
/// string values quoted so `pass="7"` and `pass=7` stay distinct.
fn track_name(context: &[OwnedContextEntry]) -> String {
    if context.is_empty() {
        return String::from("root");
    }

    let mut name = String::new();
    for (index, entry) in context.iter().enumerate() {
        if index > 0 {
            name.push_str(" / ");
        }
        name.push_str(entry.key());
        name.push('=');
        match entry.value() {
            OwnedLabelValue::Unsigned(value) => name.push_str(&value.to_string()),
            OwnedLabelValue::Signed(value) => name.push_str(&value.to_string()),
            OwnedLabelValue::Bool(value) => name.push_str(if *value { "true" } else { "false" }),
            OwnedLabelValue::String(value) => name.push_str(&format!("{value:?}")),
            _ => name.push('?'),
        }
    }
    name
}

fn marker_name(kind: MarkerKind, phase: MarkerPhase) -> &'static str {
    match (kind, phase) {
        (MarkerKind::Frame, MarkerPhase::Begin) => "frame_begin",
        (MarkerKind::Frame, MarkerPhase::End) => "frame_end",
        (MarkerKind::Transaction, MarkerPhase::Begin) => "transaction_begin",
        (MarkerKind::Transaction, MarkerPhase::End) => "transaction_end",
        (MarkerKind::Pass, MarkerPhase::Begin) => "pass_begin",
        (MarkerKind::Pass, MarkerPhase::End) => "pass_end",
        (_, MarkerPhase::Begin) => "marker_begin",
        (_, MarkerPhase::End) => "marker_end",
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

/// Writes a time in nanoseconds as the microseconds Chrome traces use,
/// with the nanoseconds as three decimal places.
fn write_json_field_time(writer: &mut impl Write, key: &str, nanos: u64) -> fmt::Result {
    write_json_string(writer, key)?;
    writer.write_char(':')?;
    writer.write_fmt(format_args!("{}.{:03}", nanos / 1000, nanos % 1000))
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
        Args, Context, ContextId, CounterDescId, FlowId, FlowPhase, FrameNumber, LabelKey,
        MarkerKind, MarkerPhase, ScopeToken, SourceLocation, Unit,
    };
    use footfall_runtime::{
        ContextSnapshot, CounterKind, CounterSnapshot, Drops, EventWindow, Observation, Snapshot,
        SnapshotMetadata, ThreadSnapshot, TimedObservation,
    };
    use footfall_session::{OwnedContextEntry, OwnedLabelValue};

    static FRAME: LabelKey = LabelKey::new("frame");
    static PASS: LabelKey = LabelKey::new("pass");

    #[test]
    fn timeline_spans_become_slices_on_their_own_track() {
        use footfall_core::{
            ClockDesc, ClockReading, ContextHandle, ScopeDesc, TickPeriod, TimelineDesc,
            TimelineSpan,
        };
        use footfall_runtime::{ClockTicks, TimelineWork};
        static GRAPHICS: TimelineDesc = TimelineDesc::gpu("graphics");
        static GPU_CLOCK: ClockDesc = ClockDesc::new("gpu");
        static NEVER_SYNCED: ClockDesc = ClockDesc::new("never synced");
        static SHADOWS: ScopeDesc = ScopeDesc::new("shadows");
        let gpu = |clock: &'static ClockDesc, begin, end| Observation::TimelineSpan {
            span: Box::new(TimelineWork::new(
                &TimelineSpan::new(
                    &GRAPHICS,
                    &SHADOWS,
                    ContextHandle::ROOT,
                    clock,
                    TickPeriod::NANOSECOND,
                    begin,
                    end,
                )
                .in_frame(7)
                .after_flow(FlowId::new(3)),
            )),
        };
        let snapshot = Snapshot::builder(SnapshotMetadata::new("app"))
            .event(TimedObservation::new(
                1_000,
                0,
                1,
                Observation::ClockSync {
                    clock: ClockTicks::from(ClockReading::new(
                        &GPU_CLOCK,
                        50_000,
                        TickPeriod::NANOSECOND,
                    )),
                },
            ))
            .event(TimedObservation::new(
                2_000,
                1,
                1,
                gpu(&GPU_CLOCK, 50_250, 50_750),
            ))
            .event(TimedObservation::new(2_000, 2, 1, gpu(&NEVER_SYNCED, 0, 1)))
            .build();
        let json = to_chrome_trace_json(&snapshot.into());
        // The track after the one thread's.
        let tid = 2;
        assert!(
            json.contains(&format!(
                "\"name\":\"shadows\",\"cat\":\"timeline\",\"ph\":\"X\",\"ts\":1.250,\"dur\":0.500,\"pid\":1,\"tid\":{tid},\"args\":{{\"frame\":7,"
            )),
            "{json}"
        );
        assert!(json.contains(&format!(
            "\"tid\":{tid},\"args\":{{\"name\":\"GPU graphics\"}}"
        )));
        assert!(json.contains(&format!(
            "\"ph\":\"f\",\"bp\":\"e\",\"ts\":1.250,\"pid\":1,\"tid\":{tid},\"id\":3"
        )));
        assert!(json.contains("footfall_unplaced_timeline_spans"));
        assert_thread_ids_fit(&json);
    }

    /// Asserts that every thread id in `json` fits in the 32 bits Perfetto
    /// keeps of one, so no track lands on another's.
    fn assert_thread_ids_fit(json: &str) {
        for tid in json.split("\"tid\":").skip(1) {
            let digits: String = tid.chars().take_while(char::is_ascii_digit).collect();
            let tid: u64 = digits.parse().expect("a thread id");
            assert!(u32::try_from(tid).is_ok(), "thread id {tid} does not fit");
        }
    }
    #[test]
    fn frames_become_targets_and_slices_on_screen() {
        use footfall_core::{
            BeginsAt, ClockDesc, ClockReading, DropReason, FramePointKind, TickPeriod,
        };
        use footfall_runtime::ClockTicks;
        static HOST: ClockDesc = ClockDesc::new("host");
        let point = |number: u64, kind, ticks: Option<u64>| Observation::FramePoint {
            number: FrameNumber::from(number),
            kind,
            time: ticks.map(|ticks| {
                ClockTicks::from(ClockReading::new(&HOST, ticks, TickPeriod::NANOSECOND))
            }),
            basis: footfall_core::TimingBasis::Unknown,
            args: Args::none(),
        };
        let mut builder =
            Snapshot::builder(SnapshotMetadata::new("app")).event(TimedObservation::new(
                0,
                0,
                1,
                Observation::ClockSync {
                    clock: ClockTicks::from(ClockReading::new(&HOST, 0, TickPeriod::NANOSECOND)),
                },
            ));
        let scheduled = FramePointKind::Scheduled(BeginsAt::Opportunity);
        let points = [
            // Frame 1, planned from 1 µs to be shown at 10 µs, and shown then.
            point(1, scheduled, Some(1_000)),
            point(1, FramePointKind::Target, Some(10_000)),
            point(1, FramePointKind::Presented, Some(10_000)),
            // Frame 2, shown 4 µs after its target.
            point(2, scheduled, Some(11_000)),
            point(2, FramePointKind::Target, Some(20_000)),
            point(2, FramePointKind::Presented, Some(24_000)),
            // Frame 3, dropped.
            point(3, FramePointKind::Target, Some(30_000)),
            point(3, FramePointKind::Dropped(DropReason::MissedDeadline), None),
            // Frame 4 ends frame 2's time on screen.
            point(4, FramePointKind::Presented, Some(40_000)),
        ];
        // Frame 2's work ran from 12 µs to 17 µs, 1 µs before its deadline.
        let marker = |phase| Observation::FrameMarker {
            kind: MarkerKind::Frame,
            phase,
            number: Some(FrameNumber::from(2)),
            context_id: ContextId::new(0),
        };
        builder = builder
            .event(TimedObservation::new(
                12_000,
                1,
                1,
                marker(MarkerPhase::Begin),
            ))
            .event(TimedObservation::new(
                17_000,
                2,
                1,
                marker(MarkerPhase::End),
            ));
        let points = points
            .into_iter()
            .chain([point(2, FramePointKind::Deadline, Some(18_000))]);
        for (sequence, observation) in (3..).zip(points) {
            builder = builder.event(TimedObservation::new(50_000, sequence, 1, observation));
        }
        let json = to_chrome_trace_json(&builder.build().into());
        // The tracks after the one thread's.
        let (built, targets, on_screen) = (2, 3, 4);
        for expected in [
            format!("\"tid\":{built},\"args\":{{\"name\":\"frames built, stream 0\"}}"),
            format!(
                "\"name\":\"frame 2\",\"cat\":\"frame\",\"ph\":\"X\",\"ts\":12.000,\"dur\":5.000,\"pid\":1,\"tid\":{built},\"args\":{{\"frame\":2,\"left_before_deadline_us\":1.000}}"
            ),
            format!(
                "\"name\":\"frame 2 deadline\",\"cat\":\"frame\",\"ph\":\"i\",\"s\":\"t\",\"ts\":18.000,\"pid\":1,\"tid\":{built}"
            ),
            format!("\"tid\":{targets},\"args\":{{\"name\":\"frame targets, stream 0\"}}"),
            format!("\"tid\":{on_screen},\"args\":{{\"name\":\"frames on screen, stream 0\"}}"),
            format!(
                "\"name\":\"frame 2 target\",\"cat\":\"frame\",\"ph\":\"i\",\"s\":\"t\",\"ts\":20.000,\"pid\":1,\"tid\":{targets},\"args\":{{\"frame\":2,\"scheduled_before_target_us\":9.000}}"
            ),
            format!(
                "\"name\":\"frame 1\",\"cat\":\"frame\",\"ph\":\"X\",\"ts\":10,\"dur\":14,\"pid\":1,\"tid\":{on_screen},\"args\":{{\"frame\":1,\"after_target_us\":0.000}}"
            ),
            // On screen from its late presentation until frame 4 replaced
            // it, since frame 3 was dropped.
            format!(
                "\"name\":\"frame 2\",\"cat\":\"frame\",\"ph\":\"X\",\"ts\":24,\"dur\":16,\"pid\":1,\"tid\":{on_screen},\"args\":{{\"frame\":2,\"after_target_us\":4.000}}"
            ),
            format!(
                "\"name\":\"frame 3 dropped\",\"cat\":\"frame\",\"ph\":\"i\",\"s\":\"t\",\"ts\":30.000,\"pid\":1,\"tid\":{on_screen},\"args\":{{\"frame\":3,\"reason\":\"missed its deadline\"}}"
            ),
        ] {
            assert!(json.contains(&expected), "{expected} missing from {json}");
        }
        assert_thread_ids_fit(&json);
    }

    #[test]
    fn frame_opportunities_become_instants_on_a_track_per_stream() {
        use footfall_core::{ClockDesc, ClockReading, FrameStream, TickPeriod};
        use footfall_runtime::ClockTicks;
        static HOST: ClockDesc = ClockDesc::new("host");
        static NEVER_SYNCED: ClockDesc = ClockDesc::new("never synced");
        let opportunity =
            |clock: &'static ClockDesc, stream, ticks| Observation::FrameOpportunity {
                stream,
                time: ClockTicks::from(ClockReading::new(clock, ticks, TickPeriod::NANOSECOND)),
                interval: Some(8_333_333),
                args: Args::none(),
            };
        let window = FrameStream::new(4).with_generation(1);
        let snapshot = Snapshot::builder(SnapshotMetadata::new("app"))
            .event(TimedObservation::new(
                1_000,
                0,
                1,
                Observation::ClockSync {
                    clock: ClockTicks::from(ClockReading::new(
                        &HOST,
                        50_000,
                        TickPeriod::NANOSECOND,
                    )),
                },
            ))
            .event(TimedObservation::new(
                2_000,
                1,
                1,
                opportunity(&HOST, window, 51_000),
            ))
            .event(TimedObservation::new(
                2_000,
                2,
                1,
                opportunity(&NEVER_SYNCED, window, 0),
            ))
            .build();
        let json = to_chrome_trace_json(&snapshot.into());
        // The track after the one thread's.
        let tid = 2;
        assert!(
            json.contains(&format!(
                "\"name\":\"frame opportunity\",\"cat\":\"frame\",\"ph\":\"i\",\"s\":\"t\",\"ts\":2.000,\"pid\":1,\"tid\":{tid},\"args\":{{\"interval_ns\":8333333}}"
            )),
            "{json}"
        );
        assert!(
            json.contains(&format!(
                "\"tid\":{tid},\"args\":{{\"name\":\"frame opportunities, stream {window}\"}}"
            )),
            "{json}"
        );
        assert!(json.contains("footfall_unplaced_frame_opportunities"));
        assert_thread_ids_fit(&json);
    }

    static RECORD_AT: SourceLocation = SourceLocation::new("app::record", "src/app.rs", 42);

    #[test]
    fn exporter_emits_metadata_context_tracks_and_scope_pairing() {
        let metadata = SnapshotMetadata::new("footfall")
            .with_session_name("recording")
            .with_instance_id(0xabc)
            .with_wall_clock_anchor_nanos(1_700_000_000_000_000);
        let snapshot = Snapshot::builder(metadata)
            .captured_at_nanos(30)
            .window(EventWindow::new(0, 9, 1))
            .thread(ThreadSnapshot::new(3, Some(String::from("render"))))
            .context(ContextSnapshot::new(ContextId::new(0), Context::root()))
            .context(ContextSnapshot::new(
                ContextId::new(1),
                Context::root().with(&FRAME, 7_u32).with(&PASS, 1_u32),
            ))
            .counter(CounterSnapshot::new(
                CounterDescId::new(1),
                "upload_bytes",
                Unit::Bytes,
                ContextId::new(1),
                CounterKind::Counter,
                4096,
                10,
            ))
            .event(TimedObservation::new(
                20,
                0,
                3,
                Observation::ScopeBegin {
                    desc_id: footfall_core::ScopeDescId::new(2),
                    name: "record",
                    location: Some(&RECORD_AT),
                    context_id: ContextId::new(1),
                    token: ScopeToken::new(9),
                    args: Args::none().with("items", 3_u32),
                },
            ))
            .event(TimedObservation::new(
                22,
                1,
                3,
                Observation::ScopeEnd {
                    token: ScopeToken::new(9),
                },
            ))
            .event(TimedObservation::new(
                24,
                2,
                3,
                Observation::FrameMarker {
                    kind: MarkerKind::Frame,
                    phase: MarkerPhase::Begin,
                    number: Some(FrameNumber::from(1234)),
                    context_id: ContextId::new(1),
                },
            ))
            .event(TimedObservation::new(
                26,
                3,
                3,
                Observation::Flow {
                    flow_id: FlowId::new(8),
                    phase: FlowPhase::Begin,
                    context_id: ContextId::new(1),
                },
            ))
            .drops(
                Drops::new()
                    .with_refused_contexts(1)
                    .with_counter_updates(1)
                    .with_evicted_events(2),
            )
            .build();

        let json = to_chrome_trace_json(&snapshot.into());
        assert!(json.contains("\"thread_name\""));
        assert!(json.contains("\"process_labels\""));
        assert!(json.contains("frame=7 / pass=1"));
        assert!(json.contains("\"tid\":3"));
        assert!(json.contains("\"args\":{\"number\":1234,"), "{json}");
        assert!(json.contains("\"name\":\"render\""));
        assert!(json.contains("\"instance_id\":\"0000000000000abc\""));
        assert!(json.contains("\"window_next\":9,\"window_missed\":1"));
        assert!(json.contains("\"upload_bytes [frame=7 / pass=1]\""));
        assert!(json.contains("\"ph\":\"B\""));
        assert!(json.contains("\"values\":{\"items\":3}"));
        assert!(json.contains(
            "\"location\":{\"function\":\"app::record\",\"file\":\"src/app.rs\",\"line\":42}}"
        ));
        assert!(json.contains("\"ph\":\"E\""));
        assert!(json.contains("\"ph\":\"s\""));
        assert!(json.contains("\"footfall_capture\""));
        assert!(json.contains("\"footfall_refused_contexts\""));
        assert!(!json.contains("\"footfall_unattributed\""));
        assert!(json.contains("\"refused_contexts\":1,\"unattributed\":0,"));
    }

    fn snapshot_with(counters: Vec<CounterSnapshot>, events: Vec<TimedObservation>) -> Snapshot {
        let builder = Snapshot::builder(SnapshotMetadata::default())
            .context(ContextSnapshot::new(ContextId::new(0), Context::root()))
            .context(ContextSnapshot::new(
                ContextId::new(1),
                Context::root().with(&FRAME, 7_u32),
            ));
        let builder = counters
            .into_iter()
            .fold(builder, |builder, counter| builder.counter(counter));
        events
            .into_iter()
            .fold(builder, |builder, event| builder.event(event))
            .build()
    }

    #[test]
    fn ratios_export_as_fractions() {
        let snapshot = snapshot_with(
            vec![CounterSnapshot::new(
                CounterDescId::new(1),
                "hit_rate",
                Unit::Ratio,
                ContextId::new(0),
                CounterKind::Gauge,
                250_000,
                3,
            )],
            vec![],
        );
        let json = to_chrome_trace_json(&snapshot.into());
        assert!(json.contains("\"args\":{\"ratio\":0.25}"), "{json}");
    }

    #[test]
    fn traces_use_the_process_id_and_wall_clock_time() {
        let snapshot = Snapshot::builder(
            SnapshotMetadata::new("app")
                .with_process_id(42)
                .with_wall_clock_anchor_nanos(1_000_000_000),
        )
        .event(timed(0, 5_250, 1, begin("work", 1)))
        .build();

        let json = to_chrome_trace_json(&snapshot.into());
        // Chrome traces count microseconds; nanoseconds are the decimals.
        assert!(json.contains("\"ts\":1000005.250,\"pid\":42,"), "{json}");
        assert!(json.contains("\"process_id\":42"));
    }

    #[test]
    fn counter_samples_plot_on_their_counter_track() {
        let sample = Observation::CounterSample {
            desc_id: CounterDescId::new(1),
            name: "upload_bytes",
            unit: Unit::Bytes,
            context_id: ContextId::new(1),
            kind: CounterKind::Counter,
            value: 4096,
        };
        let snapshot = snapshot_with(vec![], vec![timed(0, 12_000, 1, sample)]);

        let json = to_chrome_trace_json(&snapshot.into());
        assert!(json.contains(
            "{\"name\":\"upload_bytes [frame=7]\",\"cat\":\"counter\",\"ph\":\"C\",\"ts\":12.000,"
        ));
        assert!(json.contains("\"args\":{\"bytes\":4096}"));
        assert!(
            !json.contains("footfall_unmatched_scope_ends"),
            "a sample is not an unmatched scope end"
        );
    }

    fn timed(
        sequence: u64,
        at_nanos: u64,
        thread_id: u64,
        observation: Observation,
    ) -> TimedObservation {
        TimedObservation::new(at_nanos, sequence, thread_id, observation)
    }

    fn begin(name: &'static str, token: u64) -> Observation {
        Observation::ScopeBegin {
            desc_id: footfall_core::ScopeDescId::new(1),
            name,
            location: None,
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
                timed(0, 10_000, 1, begin("left", 1)),
                timed(1, 11_000, 2, begin("right", 2)),
                timed(2, 12_000, 1, end(1)),
                timed(3, 13_000, 2, end(2)),
                timed(4, 14_000, 1, begin("hop", 3)),
                timed(5, 15_000, 2, end(3)),
            ],
        );

        let json = to_chrome_trace_json(&snapshot.into());
        assert!(json.contains("\"name\":\"thread 1\""));
        assert!(json.contains("\"name\":\"thread 2\""));
        assert!(json.contains(
            "\"name\":\"left\",\"cat\":\"scope\",\"ph\":\"B\",\"ts\":10.000,\"pid\":1,\"tid\":1"
        ));
        assert!(json.contains(
            "\"name\":\"right\",\"cat\":\"scope\",\"ph\":\"B\",\"ts\":11.000,\"pid\":1,\"tid\":2"
        ));
        assert!(json.contains("{\"ph\":\"E\",\"ts\":12.000,\"pid\":1,\"tid\":1}"));
        assert!(json.contains("{\"ph\":\"E\",\"ts\":13.000,\"pid\":1,\"tid\":2}"));
        assert!(
            json.contains("{\"ph\":\"E\",\"ts\":15.000,\"pid\":1,\"tid\":1}"),
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

        let json = to_chrome_trace_json(&snapshot.into());
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

        let json = to_chrome_trace_json(&snapshot.into());
        assert!(!json.contains("\"ph\":\"E\""));
        assert!(json.contains("\"name\":\"footfall_unmatched_scope_ends\""));
        assert!(json.contains("\"args\":{\"value\":1}"));
    }

    #[test]
    fn counters_are_keyed_per_descriptor_and_context() {
        let counter = |context_id: u64, value: u64| {
            CounterSnapshot::new(
                CounterDescId::new(1),
                "ops",
                Unit::Count,
                ContextId::new(context_id),
                CounterKind::Counter,
                value,
                10,
            )
        };
        let snapshot = snapshot_with(vec![counter(0, 5), counter(1, 9)], vec![]);

        let json = to_chrome_trace_json(&snapshot.into());
        assert!(json.contains("\"name\":\"ops\",\"cat\":\"counter\",\"ph\":\"C\""));
        assert!(json.contains("\"name\":\"ops [frame=7]\",\"cat\":\"counter\",\"ph\":\"C\""));
        assert!(
            json.contains("\"args\":{\"count\":5}") && json.contains("\"args\":{\"count\":9}"),
            "counter args should carry only the numeric value"
        );
    }

    #[test]
    fn track_name_uses_context_entries_and_quotes_strings() {
        let context = [
            OwnedContextEntry::new(String::from("frame"), OwnedLabelValue::Unsigned(3)),
            OwnedContextEntry::new(
                String::from("reason"),
                OwnedLabelValue::String(String::from("invalidate")),
            ),
            OwnedContextEntry::new(
                String::from("pass"),
                OwnedLabelValue::String(String::from("7")),
            ),
        ];
        assert_eq!(
            track_name(&context),
            "frame=3 / reason=\"invalidate\" / pass=\"7\""
        );
    }

    #[test]
    fn json_strings_escape_control_characters() {
        let mut out = String::new();
        super::write_json_string(&mut out, "a\u{1}b\u{1f}c\"d\\e\nf").expect("write succeeds");
        assert_eq!(out, "\"a\\u0001b\\u001fc\\\"d\\\\e\\nf\"");
    }
}
