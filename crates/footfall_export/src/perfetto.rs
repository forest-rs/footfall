// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Perfetto's native protobuf trace format.
//!
//! The Chrome JSON export works in more tools, but Perfetto reads its times
//! as doubles, which at wall-clock times keep about a quarter of a
//! microsecond, and lays every slice of a track on one stack. This format
//! keeps every time to the nanosecond, gives each kind of work a track of
//! its own, joins flows to the events they connect, and draws frames whose
//! lives overlap on stacked lanes of one track group.

use std::collections::BTreeMap;

use footfall_core::{FlowPhase, FrameNumber, FrameStream, Unit};
use footfall_session::{
    Clocks, OwnedArgEntry, OwnedContextEntry, OwnedLabelValue, OwnedObservation, OwnedSnapshot,
};

use super::{
    PacedFrame, counter_track_name, histogram_percentile, marker_name, paced_frames,
    timeline_kind_name, trace_pid, usize_to_u64, wall_nanos,
};

/// The packet sequence every packet is written on.
const SEQUENCE: u64 = 1;

// Field numbers, from Perfetto's `protos/perfetto/trace`.
const TRACE_PACKET: u32 = 1;
const PACKET_TIMESTAMP: u32 = 8;
const PACKET_SEQUENCE_ID: u32 = 10;
const PACKET_TRACK_EVENT: u32 = 11;
const PACKET_SEQUENCE_FLAGS: u32 = 13;
const PACKET_TRACK_DESCRIPTOR: u32 = 60;
const SEQ_INCREMENTAL_STATE_CLEARED: u64 = 1;
const TRACK_UUID: u32 = 1;
const TRACK_NAME: u32 = 2;
const TRACK_PROCESS: u32 = 3;
const TRACK_THREAD: u32 = 4;
const TRACK_PARENT_UUID: u32 = 5;
const TRACK_COUNTER: u32 = 8;
const TRACK_CHILD_ORDERING: u32 = 11;
const TRACK_SIBLING_ORDER_RANK: u32 = 12;
const TRACK_DESCRIPTION: u32 = 14;
const CHILD_ORDERING_EXPLICIT: u64 = 3;
const PROCESS_PID: u32 = 1;
const PROCESS_NAME: u32 = 6;
const PROCESS_LABELS: u32 = 8;
const THREAD_PID: u32 = 1;
const THREAD_TID: u32 = 2;
const THREAD_NAME: u32 = 5;
const COUNTER_UNIT: u32 = 3;
const COUNTER_UNIT_NAME: u32 = 6;
const UNIT_TIME_NS: u64 = 1;
const UNIT_COUNT: u64 = 2;
const UNIT_SIZE_BYTES: u64 = 3;
const EVENT_DEBUG_ANNOTATIONS: u32 = 4;
const EVENT_TYPE: u32 = 9;
const EVENT_TRACK_UUID: u32 = 11;
const EVENT_CATEGORIES: u32 = 22;
const EVENT_NAME: u32 = 23;
const EVENT_COUNTER_VALUE: u32 = 30;
const EVENT_DOUBLE_COUNTER_VALUE: u32 = 44;
const EVENT_FLOW_IDS: u32 = 47;
const EVENT_TERMINATING_FLOW_IDS: u32 = 48;
const EVENT_CORRELATION_ID_STR: u32 = 53;
const TYPE_SLICE_BEGIN: u64 = 1;
const TYPE_SLICE_END: u64 = 2;
const TYPE_INSTANT: u64 = 3;
const TYPE_COUNTER: u64 = 4;
const ANNOTATION_BOOL: u32 = 2;
const ANNOTATION_UINT: u32 = 3;
const ANNOTATION_INT: u32 = 4;
const ANNOTATION_DOUBLE: u32 = 5;
const ANNOTATION_STRING: u32 = 6;
const ANNOTATION_NAME: u32 = 10;
const ANNOTATION_DICT_ENTRIES: u32 = 11;

/// A protobuf message being written.
#[derive(Default)]
struct Message(Vec<u8>);

impl Message {
    fn key(&mut self, field: u32, wire: u8) {
        self.raw_varint((u64::from(field) << 3) | u64::from(wire));
    }

    fn raw_varint(&mut self, mut value: u64) {
        while value >= 0x80 {
            self.0.push(value.to_le_bytes()[0] | 0x80);
            value >>= 7;
        }
        self.0.push(value.to_le_bytes()[0]);
    }

    fn varint(&mut self, field: u32, value: u64) -> &mut Self {
        self.key(field, 0);
        self.raw_varint(value);
        self
    }

    /// An `int64`, negative values as ten-byte two's complement.
    fn int(&mut self, field: u32, value: i64) -> &mut Self {
        self.varint(field, value as u64)
    }

    fn fixed64(&mut self, field: u32, value: u64) -> &mut Self {
        self.key(field, 1);
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    fn double(&mut self, field: u32, value: f64) -> &mut Self {
        self.fixed64(field, value.to_bits())
    }

    fn bytes(&mut self, field: u32, value: &[u8]) -> &mut Self {
        self.key(field, 2);
        self.raw_varint(usize_to_u64(value.len()));
        self.0.extend_from_slice(value);
        self
    }

    fn string(&mut self, field: u32, value: &str) -> &mut Self {
        self.bytes(field, value.as_bytes())
    }

    fn message(&mut self, field: u32, message: &Self) -> &mut Self {
        self.bytes(field, &message.0)
    }
}

/// A debug annotation named `name` with a label value.
fn annotation(name: &str, value: &OwnedLabelValue) -> Message {
    let mut annotation = Message::default();
    annotation.string(ANNOTATION_NAME, name);
    match value {
        OwnedLabelValue::Unsigned(value) => annotation.varint(ANNOTATION_UINT, *value),
        OwnedLabelValue::Signed(value) => annotation.int(ANNOTATION_INT, *value),
        OwnedLabelValue::Bool(value) => annotation.varint(ANNOTATION_BOOL, u64::from(*value)),
        OwnedLabelValue::String(value) => annotation.string(ANNOTATION_STRING, value),
        other => annotation.string(ANNOTATION_STRING, &other.to_string()),
    };
    annotation
}

fn uint_annotation(name: &str, value: u64) -> Message {
    annotation(name, &OwnedLabelValue::Unsigned(value))
}

fn string_annotation(name: &str, value: &str) -> Message {
    annotation(name, &OwnedLabelValue::String(value.to_owned()))
}

/// How far `at` is from `from`, in microseconds, negative when before.
fn micros_annotation(name: &str, at: u64, from: u64) -> Message {
    let mut annotation = Message::default();
    annotation.string(ANNOTATION_NAME, name);
    let difference = i128::from(at) - i128::from(from);
    #[expect(
        clippy::cast_precision_loss,
        reason = "differences between frame times are far below 2^53 nanoseconds"
    )]
    annotation.double(ANNOTATION_DOUBLE, difference as f64 / 1_000.0);
    annotation
}

/// A dictionary annotation of `entries`, such as a scope's arguments or its
/// context's labels.
fn dict_annotation<'a>(
    name: &str,
    entries: impl IntoIterator<Item = (&'a str, &'a OwnedLabelValue)>,
) -> Message {
    let mut dict = Message::default();
    dict.string(ANNOTATION_NAME, name);
    for (key, value) in entries {
        dict.message(ANNOTATION_DICT_ENTRIES, &annotation(key, value));
    }
    dict
}

fn args_annotation(args: &[OwnedArgEntry]) -> Message {
    dict_annotation("values", args.iter().map(|arg| (arg.key(), arg.value())))
}

fn context_annotation(context: &[OwnedContextEntry]) -> Message {
    dict_annotation(
        "context",
        context.iter().map(|entry| (entry.key(), entry.value())),
    )
}

/// One track event to write: its type, track, name, and what it carries.
struct Event {
    kind: u64,
    track: u64,
    name: Option<String>,
    category: Option<&'static str>,
    annotations: Vec<Message>,
    flows: Vec<u64>,
    terminating_flows: Vec<u64>,
    counter: Option<CounterValue>,
    /// The frame the event belongs to, which Perfetto colors every event
    /// of alike.
    correlation: Option<String>,
}

#[derive(Clone, Copy)]
enum CounterValue {
    Int(i64),
    Double(f64),
}

impl Event {
    fn new(kind: u64, track: u64) -> Self {
        Self {
            kind,
            track,
            name: None,
            category: None,
            annotations: Vec::new(),
            flows: Vec::new(),
            terminating_flows: Vec::new(),
            counter: None,
            correlation: None,
        }
    }

    fn named(kind: u64, track: u64, name: impl Into<String>, category: &'static str) -> Self {
        let mut event = Self::new(kind, track);
        event.name = Some(name.into());
        event.category = Some(category);
        event
    }

    fn with(mut self, annotation: Message) -> Self {
        self.annotations.push(annotation);
        self
    }

    /// Marks the event as part of `frame`, so Perfetto colors it like every
    /// other event of the frame: by default it colors by name, ignoring
    /// numbers, which makes every frame alike.
    fn of_frame(mut self, frame: FrameNumber) -> Self {
        self.correlation = Some(frame_correlation(frame));
        self
    }

    fn encode(&self) -> Message {
        let mut event = Message::default();
        event.varint(EVENT_TYPE, self.kind);
        event.varint(EVENT_TRACK_UUID, self.track);
        if let Some(category) = self.category {
            event.string(EVENT_CATEGORIES, category);
        }
        if let Some(name) = &self.name {
            event.string(EVENT_NAME, name);
        }
        for annotation in &self.annotations {
            event.message(EVENT_DEBUG_ANNOTATIONS, annotation);
        }
        for flow in &self.flows {
            event.fixed64(EVENT_FLOW_IDS, *flow);
        }
        for flow in &self.terminating_flows {
            event.fixed64(EVENT_TERMINATING_FLOW_IDS, *flow);
        }
        if let Some(correlation) = &self.correlation {
            event.string(EVENT_CORRELATION_ID_STR, correlation);
        }
        match self.counter {
            Some(CounterValue::Int(value)) => {
                event.int(EVENT_COUNTER_VALUE, value);
            }
            Some(CounterValue::Double(value)) => {
                event.double(EVENT_DOUBLE_COUNTER_VALUE, value);
            }
            None => {}
        }
        event
    }
}

/// The trace being written: its packets, and the tracks it has declared.
struct Writer<'a> {
    snapshot: &'a OwnedSnapshot,
    trace: Message,
    /// The first packet clears the sequence's state.
    first: bool,
    next_track: u64,
    process: u64,
    aggregate: u64,
    threads: BTreeMap<u64, u64>,
    counters: BTreeMap<String, u64>,
}

impl<'a> Writer<'a> {
    fn new(snapshot: &'a OwnedSnapshot) -> Self {
        let mut writer = Self {
            snapshot,
            trace: Message::default(),
            first: true,
            next_track: 0,
            process: 0,
            aggregate: 0,
            threads: BTreeMap::new(),
            counters: BTreeMap::new(),
        };
        let metadata = snapshot.metadata();
        writer.process = writer.track_uuid();
        let mut process = Message::default();
        process.int(PROCESS_PID, i64::from(trace_pid_i32(snapshot)));
        process.string(PROCESS_NAME, metadata.process_name());
        if let Some(session) = metadata.session_name() {
            process.string(PROCESS_LABELS, session);
        }
        let mut track = Message::default();
        track
            .varint(TRACK_UUID, writer.process)
            .message(TRACK_PROCESS, &process);
        writer.descriptor(&track);
        writer.aggregate = writer.named_track(
            writer.process,
            "footfall",
            Some("the capture, its contexts, and what it could not keep"),
            None,
        );
        writer
    }

    /// A track uuid unique to this runtime instance, so traces from several
    /// runs can be opened together.
    fn track_uuid(&mut self) -> u64 {
        self.next_track += 1;
        self.snapshot
            .metadata()
            .instance_id()
            .wrapping_add(self.next_track.wrapping_mul(0x9e37_79b9_7f4a_7c15))
    }

    fn packet(&mut self, packet: &mut Message) {
        packet.varint(PACKET_SEQUENCE_ID, SEQUENCE);
        if self.first {
            packet.varint(PACKET_SEQUENCE_FLAGS, SEQ_INCREMENTAL_STATE_CLEARED);
            self.first = false;
        }
        self.trace.message(TRACE_PACKET, packet);
    }

    fn descriptor(&mut self, track: &Message) {
        let mut packet = Message::default();
        packet.message(PACKET_TRACK_DESCRIPTOR, track);
        self.packet(&mut packet);
    }

    /// Declares a track named `name` under `parent`, ranked `rank` among its
    /// siblings when the parent orders them.
    fn named_track(
        &mut self,
        parent: u64,
        name: &str,
        description: Option<&str>,
        rank: Option<i64>,
    ) -> u64 {
        let uuid = self.track_uuid();
        let mut track = Message::default();
        track
            .varint(TRACK_UUID, uuid)
            .varint(TRACK_PARENT_UUID, parent)
            .string(TRACK_NAME, name);
        if let Some(description) = description {
            track.string(TRACK_DESCRIPTION, description);
        }
        if let Some(rank) = rank {
            track.int(TRACK_SIBLING_ORDER_RANK, rank);
        }
        self.descriptor(&track);
        uuid
    }

    /// Declares a group track under the process whose children keep the
    /// order of their ranks.
    fn ordered_group(&mut self, name: &str) -> u64 {
        let uuid = self.track_uuid();
        let mut track = Message::default();
        track
            .varint(TRACK_UUID, uuid)
            .varint(TRACK_PARENT_UUID, self.process)
            .string(TRACK_NAME, name)
            .varint(TRACK_CHILD_ORDERING, CHILD_ORDERING_EXPLICIT);
        self.descriptor(&track);
        uuid
    }

    /// The track of the thread the runtime numbered `thread_id`.
    fn thread(&mut self, thread_id: u64) -> u64 {
        if let Some(uuid) = self.threads.get(&thread_id) {
            return *uuid;
        }
        let uuid = self.track_uuid();
        let name = self
            .snapshot
            .thread(thread_id)
            .and_then(|thread| thread.name())
            .map_or_else(|| format!("thread {thread_id}"), String::from);
        let mut thread = Message::default();
        thread
            .int(THREAD_PID, i64::from(trace_pid_i32(self.snapshot)))
            .int(THREAD_TID, i64::try_from(thread_id).unwrap_or(i64::MAX))
            .string(THREAD_NAME, &name);
        let mut track = Message::default();
        track
            .varint(TRACK_UUID, uuid)
            .varint(TRACK_PARENT_UUID, self.process)
            .message(TRACK_THREAD, &thread);
        self.descriptor(&track);
        self.threads.insert(thread_id, uuid);
        uuid
    }

    /// The counter track named `name`, measured in `unit`.
    fn counter(&mut self, name: &str, unit: Option<Unit>) -> u64 {
        if let Some(uuid) = self.counters.get(name) {
            return *uuid;
        }
        let uuid = self.track_uuid();
        let mut counter = Message::default();
        match unit {
            Some(Unit::Nanos) => {
                counter.varint(COUNTER_UNIT, UNIT_TIME_NS);
            }
            Some(Unit::Bytes) => {
                counter.varint(COUNTER_UNIT, UNIT_SIZE_BYTES);
            }
            Some(Unit::Count) | None => {
                counter.varint(COUNTER_UNIT, UNIT_COUNT);
            }
            Some(other) => {
                counter.string(COUNTER_UNIT_NAME, other.as_str());
            }
        }
        let mut track = Message::default();
        track
            .varint(TRACK_UUID, uuid)
            .varint(TRACK_PARENT_UUID, self.process)
            .string(TRACK_NAME, name)
            .message(TRACK_COUNTER, &counter);
        self.descriptor(&track);
        self.counters.insert(name.to_owned(), uuid);
        uuid
    }

    /// Writes `event` at `at` nanoseconds since the runtime began.
    fn event(&mut self, at: u64, event: &Event) {
        let mut packet = Message::default();
        packet
            .varint(PACKET_TIMESTAMP, wall_nanos(self.snapshot, at))
            .message(PACKET_TRACK_EVENT, &event.encode());
        self.packet(&mut packet);
    }

    fn counter_value(&mut self, at: u64, track: u64, unit: Unit, value: u64) {
        let mut event = Event::new(TYPE_COUNTER, track);
        event.counter = Some(if unit == Unit::Ratio {
            #[expect(
                clippy::cast_precision_loss,
                reason = "ratios above 2^53 parts per million do not occur"
            )]
            CounterValue::Double(value as f64 / 1_000_000.0)
        } else {
            CounterValue::Int(i64::try_from(value).unwrap_or(i64::MAX))
        });
        self.event(at, &event);
    }

    fn context(&self, context_id: footfall_core::ContextId) -> &'a [OwnedContextEntry] {
        self.snapshot
            .context(context_id)
            .map_or(&[][..], |context| context.entries())
    }
}

/// The correlation id every event of `frame` carries.
fn frame_correlation(frame: FrameNumber) -> String {
    format!("stream {} frame {}", frame.stream(), frame.number())
}

/// The process id as Perfetto's `int32`.
fn trace_pid_i32(snapshot: &OwnedSnapshot) -> i32 {
    i32::try_from(trace_pid(snapshot)).unwrap_or(i32::MAX)
}

/// Converts a snapshot into a Perfetto trace, in Perfetto's protobuf
/// format.
///
/// Each recording thread is a thread track of the producer's process, with
/// scopes as slices and markers, messages, and resource events as instants.
/// Counters and gauges are counter tracks, one per descriptor and context.
/// Timelines that are not CPU threads, such as GPU queues, are tracks of
/// their own, and flows join the events they connect. Each frame stream a
/// frame scheduler reported on is a group of tracks: the frame
/// opportunities the platform offered, each frame from when it was
/// scheduled to begin until it was shown, split into waiting, building, and
/// in flight, with its deadline and target, on as many stacked lanes as
/// frames in flight at once, and how long each shown frame stayed on
/// screen. Every event of a frame, on those tracks, its numbered markers, and
/// its GPU work, carries the frame as its correlation id, which Perfetto
/// colors alike, so one frame is one color wherever it appears; by name it
/// would color every frame alike, as it ignores numbers. The capture's
/// metadata and contexts are instants on a `footfall` track. Times keep
/// their nanoseconds.
#[must_use]
pub fn to_perfetto_trace(snapshot: &OwnedSnapshot) -> Vec<u8> {
    let mut writer = Writer::new(snapshot);
    let clocks = Clocks::from_snapshot(snapshot);
    let captured = snapshot.captured_at_nanos();

    capture_events(&mut writer);

    // Timelines and frame streams declare their tracks before any event.
    let mut timelines = BTreeMap::new();
    for event in snapshot.events() {
        if let OwnedObservation::TimelineSpan { span } = event.observation() {
            timelines
                .entry(span.timeline_id())
                .or_insert((span.timeline_name(), span.timeline_kind()));
        }
    }
    let timelines: BTreeMap<_, _> = timelines
        .into_iter()
        .map(|(id, (name, kind))| {
            let track = writer.named_track(
                writer.process,
                &format!("{} {name}", timeline_kind_name(kind)),
                None,
                None,
            );
            (id, track)
        })
        .collect();
    let frames = paced_frames(snapshot, &clocks);
    let mut streams: BTreeMap<FrameStream, StreamTracks> = BTreeMap::new();
    for event in snapshot.events() {
        if let OwnedObservation::FrameOpportunity { stream, .. } = event.observation() {
            streams.entry(*stream).or_default();
        }
    }
    for stream in frames.keys() {
        streams.entry(*stream).or_default();
    }
    for (stream, tracks) in &mut streams {
        let group = writer.ordered_group(&format!("frames, stream {stream}"));
        tracks.opportunities = writer.named_track(
            group,
            "frame opportunities",
            Some("each chance the platform gave to begin a frame"),
            Some(0),
        );
        tracks.group = group;
    }

    for counter in snapshot.counters() {
        let context = writer.context(counter.context_id());
        let track = writer.counter(
            &counter_track_name(counter.name(), context),
            Some(counter.unit()),
        );
        writer.counter_value(
            counter.updated_at_nanos(),
            track,
            counter.unit(),
            counter.value(),
        );
    }
    for histogram in snapshot.histograms() {
        let context = writer.context(histogram.context_id());
        let event = Event::named(
            TYPE_INSTANT,
            writer.aggregate,
            histogram.name(),
            "histogram",
        )
        .with(uint_annotation("count", histogram.count()))
        .with(uint_annotation("sum", histogram.sum()))
        .with(uint_annotation(
            "min",
            if histogram.count() == 0 {
                0
            } else {
                histogram.min()
            },
        ))
        .with(uint_annotation("max", histogram.max()))
        .with(uint_annotation(
            "p50",
            histogram_percentile(histogram, 0.50),
        ))
        .with(uint_annotation(
            "p90",
            histogram_percentile(histogram, 0.90),
        ))
        .with(uint_annotation(
            "p99",
            histogram_percentile(histogram, 0.99),
        ))
        .with(string_annotation("unit", histogram.unit().as_str()))
        .with(context_annotation(context));
        writer.event(histogram.updated_at_nanos(), &event);
    }

    for (stream, frames) in &frames {
        frame_tracks(
            &mut writer,
            *stream,
            frames,
            streams.get_mut(stream).expect("declared"),
        );
    }

    let mut scopes = std::collections::HashMap::new();
    let (mut unmatched_ends, mut unplaced_spans, mut unplaced_opportunities) = (0, 0, 0);
    for event in snapshot.events() {
        let at = event.at_nanos();
        match event.observation() {
            OwnedObservation::ScopeBegin {
                name,
                location,
                context_id,
                token,
                args,
                ..
            } => {
                let track = writer.thread(event.thread_id());
                scopes.insert(*token, track);
                let mut slice = Event::named(TYPE_SLICE_BEGIN, track, name.clone(), "scope")
                    .with(uint_annotation("context_id", context_id.get()))
                    .with(context_annotation(writer.context(*context_id)));
                if !args.is_empty() {
                    slice = slice.with(args_annotation(args));
                }
                if let Some(location) = location {
                    let mut dict = Message::default();
                    dict.string(ANNOTATION_NAME, "location")
                        .message(
                            ANNOTATION_DICT_ENTRIES,
                            &string_annotation("function", location.function()),
                        )
                        .message(
                            ANNOTATION_DICT_ENTRIES,
                            &string_annotation("file", location.file()),
                        )
                        .message(
                            ANNOTATION_DICT_ENTRIES,
                            &uint_annotation("line", location.line().into()),
                        );
                    slice = slice.with(dict);
                }
                writer.event(at, &slice);
            }
            OwnedObservation::ScopeEnd { token } => {
                // On its begin's track, even when the scope ended on another
                // thread; an end whose begin is not here would close an
                // unrelated slice.
                match scopes.remove(token) {
                    Some(track) => writer.event(at, &Event::new(TYPE_SLICE_END, track)),
                    None => unmatched_ends += 1,
                }
            }
            OwnedObservation::ResourceCreate {
                kind_name,
                resource_id,
                context_id,
                size,
                ..
            }
            | OwnedObservation::ResourceUpdate {
                kind_name,
                resource_id,
                context_id,
                size,
                ..
            } => {
                let name = if matches!(event.observation(), OwnedObservation::ResourceCreate { .. })
                {
                    "resource_create"
                } else {
                    "resource_update"
                };
                let track = writer.thread(event.thread_id());
                let instant = Event::named(TYPE_INSTANT, track, name, "resource")
                    .with(string_annotation("kind", kind_name))
                    .with(uint_annotation("resource_id", resource_id.get()))
                    .with(uint_annotation("size", *size))
                    .with(uint_annotation("context_id", context_id.get()))
                    .with(context_annotation(writer.context(*context_id)));
                writer.event(at, &instant);
            }
            OwnedObservation::ResourceDrop {
                kind_name,
                resource_id,
                context_id,
                ..
            } => {
                let track = writer.thread(event.thread_id());
                let instant = Event::named(TYPE_INSTANT, track, "resource_drop", "resource")
                    .with(string_annotation("kind", kind_name))
                    .with(uint_annotation("resource_id", resource_id.get()))
                    .with(uint_annotation("context_id", context_id.get()))
                    .with(context_annotation(writer.context(*context_id)));
                writer.event(at, &instant);
            }
            OwnedObservation::FrameMarker {
                kind,
                phase,
                number,
                context_id,
            } => {
                let track = writer.thread(event.thread_id());
                let mut instant =
                    Event::named(TYPE_INSTANT, track, marker_name(*kind, *phase), "marker")
                        .with(uint_annotation("context_id", context_id.get()))
                        .with(context_annotation(writer.context(*context_id)));
                if let Some(number) = number {
                    instant = instant
                        .with(uint_annotation("number", number.number()))
                        .of_frame(*number);
                    if number.stream() != FrameStream::DEFAULT {
                        instant =
                            instant.with(string_annotation("stream", &number.stream().to_string()));
                    }
                }
                writer.event(at, &instant);
            }
            OwnedObservation::Flow {
                flow_id,
                phase,
                context_id,
            } => {
                let track = writer.thread(event.thread_id());
                let mut instant = Event::named(TYPE_INSTANT, track, "flow", "flow")
                    .with(uint_annotation("context_id", context_id.get()))
                    .with(context_annotation(writer.context(*context_id)));
                match phase {
                    FlowPhase::End => instant.terminating_flows.push(flow_id.get()),
                    _ => instant.flows.push(flow_id.get()),
                }
                writer.event(at, &instant);
            }
            OwnedObservation::Message {
                name,
                context_id,
                text,
                args,
                ..
            } => {
                let track = writer.thread(event.thread_id());
                let mut instant = Event::named(TYPE_INSTANT, track, name.clone(), "message")
                    .with(string_annotation("text", text))
                    .with(uint_annotation("context_id", context_id.get()))
                    .with(context_annotation(writer.context(*context_id)));
                if !args.is_empty() {
                    instant = instant.with(args_annotation(args));
                }
                writer.event(at, &instant);
            }
            OwnedObservation::CounterSample {
                name,
                unit,
                context_id,
                value,
                ..
            } => {
                let context = writer.context(*context_id);
                let track = writer.counter(&counter_track_name(name, context), Some(*unit));
                writer.counter_value(at, track, *unit, *value);
            }
            OwnedObservation::TimelineSpan { span } => {
                let clock = span.begin().clock_id();
                let (Some(begin), Some(end)) = (
                    clocks.to_runtime(clock, span.begin().ticks()),
                    clocks.to_runtime(clock, span.end()),
                ) else {
                    unplaced_spans += 1;
                    continue;
                };
                let (begin, end) = (begin.nanos(), end.nanos().max(begin.nanos()));
                let track = timelines[&span.timeline_id()];
                let mut slice = Event::named(TYPE_SLICE_BEGIN, track, span.name(), "timeline")
                    .with(uint_annotation("context_id", span.context_id().get()))
                    .with(context_annotation(writer.context(span.context_id())));
                if let Some(frame) = span.frame() {
                    slice = slice
                        .with(uint_annotation("frame", frame.number()))
                        .of_frame(frame);
                }
                if !span.args().is_empty() {
                    slice = slice.with(args_annotation(span.args()));
                }
                if let Some(flow) = span.flow() {
                    slice.terminating_flows.push(flow.get());
                }
                writer.event(begin, &slice);
                writer.event(end, &Event::new(TYPE_SLICE_END, track));
            }
            OwnedObservation::FrameOpportunity {
                stream,
                time,
                interval,
                ..
            } => {
                let Some(offered) = clocks.to_runtime(time.clock_id(), time.ticks()) else {
                    unplaced_opportunities += 1;
                    continue;
                };
                let track = streams[stream].opportunities;
                let mut instant = Event::named(TYPE_INSTANT, track, "frame opportunity", "frame");
                if let Some(interval) = interval {
                    instant = instant.with(uint_annotation(
                        "interval_ns",
                        time.period().ticks_to_nanos(*interval),
                    ));
                }
                writer.event(offered.nanos(), &instant);
            }
            // Clock readings were used to place times; frame points are
            // drawn on the frame tracks; kinds this exporter does not know
            // are left out.
            _ => {}
        }
    }

    let drops = snapshot.drops();
    for (name, value) in [
        ("footfall_unmatched_scope_ends", unmatched_ends),
        ("footfall_unplaced_timeline_spans", unplaced_spans),
        (
            "footfall_unplaced_frame_opportunities",
            unplaced_opportunities,
        ),
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
        if value > 0 {
            let track = writer.counter(name, None);
            writer.counter_value(captured, track, Unit::Count, value);
        }
    }
    writer.trace.0
}

/// Writes the capture's metadata and its contexts as instants on the
/// `footfall` track.
fn capture_events(writer: &mut Writer<'_>) {
    let snapshot = writer.snapshot;
    let metadata = snapshot.metadata();
    let capture = metadata.capture();
    let window = snapshot.window();
    let drops = snapshot.drops();
    let mut event = Event::named(
        TYPE_INSTANT,
        writer.aggregate,
        "footfall_capture",
        "footfall",
    )
    .with(string_annotation(
        "instance_id",
        &format!("{:016x}", metadata.instance_id()),
    ))
    .with(string_annotation(
        "producer_version",
        metadata.producer_version(),
    ))
    .with(string_annotation("level", capture.level().as_str()));
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
        event = event.with(uint_annotation(name, value));
    }
    writer.event(snapshot.captured_at_nanos(), &event);

    let mut contexts = Event::named(
        TYPE_INSTANT,
        writer.aggregate,
        "footfall_contexts",
        "footfall",
    );
    for context in snapshot.contexts() {
        contexts = contexts.with(dict_annotation(
            &context.id().get().to_string(),
            context
                .entries()
                .iter()
                .map(|entry| (entry.key(), entry.value())),
        ));
    }
    writer.event(snapshot.captured_at_nanos(), &contexts);
}

/// One frame stream's tracks.
#[derive(Default)]
struct StreamTracks {
    group: u64,
    opportunities: u64,
}

/// Draws one frame stream's frames: each frame's life on the first lane of
/// the `frames` track group free when it began, and how long each shown
/// frame stayed on screen.
fn frame_tracks(
    writer: &mut Writer<'_>,
    stream: FrameStream,
    frames: &BTreeMap<u64, PacedFrame>,
    tracks: &mut StreamTracks,
) {
    let on_screen = writer.named_track(
        tracks.group,
        "frames on screen",
        Some("each shown frame, from its presentation until the next replaced it"),
        Some(2),
    );
    // Each lane holds frames whose lives do not overlap; lanes share a
    // name, so Perfetto stacks them as one group.
    let mut lanes: Vec<(u64, u64)> = Vec::new();
    for (&number, frame) in frames {
        let times = [
            frame.scheduled,
            frame.begin,
            frame.end,
            frame.deadline,
            frame.target,
            frame.presented,
        ];
        let (Some(start), Some(finish)) = (
            times.iter().flatten().min().copied(),
            times.iter().flatten().max().copied(),
        ) else {
            continue;
        };
        let lane = match lanes.iter().position(|(_, free_at)| *free_at <= start) {
            Some(lane) => lane,
            None => {
                let track = writer.named_track(
                    tracks.group,
                    "frames",
                    Some("each frame from when it was to begin until it was shown"),
                    Some(1),
                );
                lanes.push((track, 0));
                lanes.len() - 1
            }
        };
        lanes[lane].1 = finish;
        let track = lanes[lane].0;
        let frame_number = stream.frame(number);
        let mut life = Event::named(TYPE_SLICE_BEGIN, track, format!("frame {number}"), "frame")
            .with(uint_annotation("frame", number))
            .of_frame(frame_number);
        if stream != FrameStream::DEFAULT {
            life = life.with(string_annotation("stream", &stream.to_string()));
        }
        if let (Some(target), Some(presented)) = (frame.target, frame.presented) {
            life = life.with(micros_annotation("after_target_us", presented, target));
        }
        if let (Some(deadline), Some(end)) = (frame.deadline, frame.end) {
            life = life.with(micros_annotation("left_before_deadline_us", deadline, end));
        }
        if let Some(reason) = frame.dropped {
            life = life.with(string_annotation("dropped", &reason.to_string()));
        }
        writer.event(start, &life);
        // Its stages, in order, each inside the frame's life.
        let mut stages = Vec::new();
        if let (Some(scheduled), Some(begin)) = (frame.scheduled, frame.begin) {
            stages.push(("waiting to begin", scheduled, begin));
        }
        if let (Some(begin), Some(end)) = (frame.begin, frame.end) {
            stages.push(("built", begin, end));
        }
        if let (Some(end), Some(presented)) = (frame.end, frame.presented) {
            stages.push(("in flight", end, presented));
        }
        let mut instants = Vec::new();
        if let Some(deadline) = frame.deadline {
            instants.push((String::from("deadline"), deadline));
        }
        if let Some(target) = frame.target {
            instants.push((String::from("target"), target));
        }
        if let (Some(reason), Some(at)) = (frame.dropped, frame.end.or(frame.target)) {
            instants.push((format!("dropped: {reason}"), at));
        }
        // Write stages and instants in time order, instants at a stage's
        // edge after it ends, so each nests where it falls.
        let mut steps: Vec<(u64, u8, Event)> = Vec::new();
        for (name, from, to) in stages {
            if to < from {
                continue;
            }
            steps.push((
                from,
                1,
                Event::named(TYPE_SLICE_BEGIN, track, name, "frame").of_frame(frame_number),
            ));
            steps.push((to, 0, Event::new(TYPE_SLICE_END, track)));
        }
        for (name, at) in instants {
            steps.push((
                at,
                2,
                Event::named(TYPE_INSTANT, track, name, "frame").of_frame(frame_number),
            ));
        }
        steps.sort_by_key(|(at, order, _)| (*at, *order));
        for (at, _, event) in &steps {
            writer.event(*at, event);
        }
        writer.event(finish, &Event::new(TYPE_SLICE_END, track));
    }

    let mut shown: Vec<(u64, u64, Option<u64>)> = frames
        .iter()
        .filter_map(|(&number, frame)| Some((frame.presented?, number, frame.target)))
        .collect();
    shown.sort_unstable();
    for pair in shown.windows(2) {
        let ((presented, number, target), (next, ..)) = (pair[0], pair[1]);
        let mut slice = Event::named(
            TYPE_SLICE_BEGIN,
            on_screen,
            format!("frame {number}"),
            "frame",
        )
        .with(uint_annotation("frame", number))
        .of_frame(stream.frame(number));
        if let Some(target) = target {
            slice = slice.with(micros_annotation("after_target_us", presented, target));
        }
        writer.event(presented, &slice);
        writer.event(next, &Event::new(TYPE_SLICE_END, on_screen));
    }
}

#[cfg(test)]
mod tests {
    use footfall_core::{
        Args, BeginsAt, ClockDesc, ClockReading, ContextHandle, ContextId, FlowId, FlowPhase,
        FrameNumber, FramePointKind, ScopeDesc, ScopeDescId, ScopeToken, TickPeriod, TimelineDesc,
        TimelineSpan, TimingBasis,
    };
    use footfall_runtime::{
        ClockTicks, Observation, Snapshot, SnapshotMetadata, ThreadSnapshot, TimedObservation,
        TimelineWork,
    };

    use super::to_perfetto_trace;

    static HOST: ClockDesc = ClockDesc::new("host");

    /// A protobuf field's value, as these tests read it back.
    #[derive(Clone, Copy, Debug)]
    enum Value<'a> {
        Varint(u64),
        Fixed64(u64),
        Bytes(&'a [u8]),
    }

    fn varint(bytes: &mut &[u8]) -> u64 {
        let mut value = 0;
        for shift in (0..64).step_by(7) {
            let byte = bytes[0];
            *bytes = &bytes[1..];
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                break;
            }
        }
        value
    }

    /// Reads a message's fields, in order.
    fn fields(mut bytes: &[u8]) -> Vec<(u32, Value<'_>)> {
        let mut fields = Vec::new();
        while !bytes.is_empty() {
            let key = varint(&mut bytes);
            let field = u32::try_from(key >> 3).expect("a field number");
            let value = match key & 7 {
                0 => Value::Varint(varint(&mut bytes)),
                1 => {
                    let (value, rest) = bytes.split_at(8);
                    bytes = rest;
                    Value::Fixed64(u64::from_le_bytes(value.try_into().expect("8 bytes")))
                }
                2 => {
                    let length = usize::try_from(varint(&mut bytes)).expect("a length");
                    let (value, rest) = bytes.split_at(length);
                    bytes = rest;
                    Value::Bytes(value)
                }
                wire => panic!("unexpected wire type {wire}"),
            };
            fields.push((field, value));
        }
        fields
    }

    fn uint(fields: &[(u32, Value<'_>)], field: u32) -> Option<u64> {
        fields
            .iter()
            .find_map(|(number, value)| match (number, value) {
                (number, Value::Varint(value)) if *number == field => Some(*value),
                _ => None,
            })
    }

    fn text<'a>(fields: &[(u32, Value<'a>)], field: u32) -> Option<&'a str> {
        fields
            .iter()
            .find_map(|(number, value)| match (number, value) {
                (number, Value::Bytes(value)) if *number == field => {
                    Some(core::str::from_utf8(value).expect("UTF-8"))
                }
                _ => None,
            })
    }

    fn nested<'a>(fields: &[(u32, Value<'a>)], field: u32) -> Option<Vec<(u32, Value<'a>)>> {
        fields
            .iter()
            .find_map(|(number, value)| match (number, value) {
                (number, Value::Bytes(value)) if *number == field => Some(self::fields(value)),
                _ => None,
            })
    }

    fn fixed(fields: &[(u32, Value<'_>)], field: u32) -> Vec<u64> {
        fields
            .iter()
            .filter_map(|(number, value)| match (number, value) {
                (number, Value::Fixed64(value)) if *number == field => Some(*value),
                _ => None,
            })
            .collect()
    }

    /// A track a trace declared.
    #[derive(Debug)]
    struct Track {
        uuid: u64,
        parent: Option<u64>,
        name: Option<String>,
        pid: Option<u64>,
        tid: Option<u64>,
    }

    /// A track event a trace holds.
    #[derive(Debug)]
    struct Event {
        at: u64,
        kind: u64,
        track: u64,
        name: Option<String>,
        flows: Vec<u64>,
        terminating_flows: Vec<u64>,
        correlation: Option<String>,
    }

    fn read(trace: &[u8]) -> (Vec<Track>, Vec<Event>) {
        let (mut tracks, mut events) = (Vec::new(), Vec::new());
        for (field, value) in fields(trace) {
            assert_eq!(field, 1, "a trace holds only packets");
            let Value::Bytes(packet) = value else {
                panic!("a packet is a message");
            };
            let packet = fields(packet);
            assert_eq!(
                uint(&packet, 10),
                Some(1),
                "every packet is on one sequence"
            );
            if let Some(track) = nested(&packet, 60) {
                let process = nested(&track, 3);
                let thread = nested(&track, 4);
                tracks.push(Track {
                    uuid: uint(&track, 1).expect("a uuid"),
                    parent: uint(&track, 5),
                    name: text(&track, 2)
                        .or_else(|| process.as_ref().and_then(|process| text(process, 6)))
                        .or_else(|| thread.as_ref().and_then(|thread| text(thread, 5)))
                        .map(String::from),
                    pid: process.as_ref().and_then(|process| uint(process, 1)),
                    tid: thread.as_ref().and_then(|thread| uint(thread, 2)),
                });
            }
            if let Some(event) = nested(&packet, 11) {
                events.push(Event {
                    at: uint(&packet, 8).expect("a timestamp"),
                    kind: uint(&event, 9).expect("a type"),
                    track: uint(&event, 11).expect("a track"),
                    name: text(&event, 23).map(String::from),
                    flows: fixed(&event, 47),
                    terminating_flows: fixed(&event, 48),
                    correlation: text(&event, 53).map(String::from),
                });
            }
        }
        (tracks, events)
    }

    fn track<'a>(tracks: &'a [Track], name: &str) -> &'a Track {
        tracks
            .iter()
            .find(|track| track.name.as_deref() == Some(name))
            .unwrap_or_else(|| panic!("no track named {name}: {tracks:?}"))
    }

    fn sync() -> Observation {
        Observation::ClockSync {
            clock: ClockTicks::from(ClockReading::new(&HOST, 0, TickPeriod::NANOSECOND)),
        }
    }

    #[test]
    fn scopes_are_slices_on_their_thread_to_the_nanosecond() {
        let snapshot = Snapshot::builder(
            SnapshotMetadata::new("app")
                .with_process_id(42)
                .with_wall_clock_anchor_nanos(1_792_122_998_206_840_832),
        )
        .thread(ThreadSnapshot::new(1, Some(String::from("main"))))
        .event(TimedObservation::new(
            5_250,
            0,
            1,
            Observation::ScopeBegin {
                desc_id: ScopeDescId::new(1),
                name: "work",
                location: None,
                context_id: ContextId::new(0),
                token: ScopeToken::new(1),
                args: Args::none(),
            },
        ))
        .event(TimedObservation::new(
            7_001,
            1,
            1,
            Observation::ScopeEnd {
                token: ScopeToken::new(1),
            },
        ))
        .build();
        let (tracks, events) = read(&to_perfetto_trace(&snapshot.into()));

        let process = track(&tracks, "app");
        assert_eq!(process.pid, Some(42));
        let main = track(&tracks, "main");
        assert_eq!((main.tid, main.parent), (Some(1), Some(process.uuid)));
        let work: Vec<_> = events
            .iter()
            .filter(|event| event.track == main.uuid)
            .collect();
        assert_eq!(work.len(), 2);
        assert_eq!(
            (work[0].kind, work[0].name.as_deref(), work[0].at),
            (1, Some("work"), 1_792_122_998_206_846_082),
            "the begin, to the nanosecond at a wall-clock time"
        );
        assert_eq!((work[1].kind, work[1].at), (2, 1_792_122_998_206_847_833));
    }

    #[test]
    fn frames_in_flight_together_take_stacked_lanes() {
        let point = |number: u64, kind, at: u64| Observation::FramePoint {
            number: FrameNumber::from(number),
            kind,
            time: Some(ClockTicks::from(ClockReading::new(
                &HOST,
                at,
                TickPeriod::NANOSECOND,
            ))),
            basis: TimingBasis::Unknown,
            args: Args::none(),
        };
        let marker = |number: u64, phase| Observation::FrameMarker {
            kind: footfall_core::MarkerKind::Frame,
            phase,
            number: Some(FrameNumber::from(number)),
            context_id: ContextId::new(0),
        };
        use footfall_core::MarkerPhase::{Begin, End};
        let scheduled = FramePointKind::Scheduled(BeginsAt::Opportunity);
        let mut observations = vec![(0, sync())];
        // Three frames 10 µs apart, each shown 25 µs after it began: the
        // first two are in flight at once, and the third begins after the
        // first was shown.
        for (number, start) in [(1_u64, 10_000_u64), (2, 20_000), (3, 40_000)] {
            observations.extend([
                (start + 1_000, marker(number, Begin)),
                (start + 4_000, marker(number, End)),
                (50_000, point(number, scheduled, start)),
                (
                    50_000,
                    point(number, FramePointKind::Deadline, start + 6_000),
                ),
                (
                    50_000,
                    point(number, FramePointKind::Target, start + 25_000),
                ),
                (
                    50_000,
                    point(number, FramePointKind::Presented, start + 25_000),
                ),
            ]);
        }
        let mut builder = Snapshot::builder(SnapshotMetadata::new("app"));
        for (sequence, (at, observation)) in (0..).zip(observations) {
            builder = builder.event(TimedObservation::new(at, sequence, 1, observation));
        }
        let (tracks, events) = read(&to_perfetto_trace(&builder.build().into()));

        let group = track(&tracks, "frames, stream 0");
        let lanes: Vec<_> = tracks
            .iter()
            .filter(|track| track.name.as_deref() == Some("frames"))
            .collect();
        assert_eq!(lanes.len(), 2, "two frames were in flight at once");
        assert!(lanes.iter().all(|lane| lane.parent == Some(group.uuid)));
        let on = |name: &str| {
            events
                .iter()
                .find(|event| event.name.as_deref() == Some(name))
                .map(|event| event.track)
        };
        assert_eq!(on("frame 1"), Some(lanes[0].uuid));
        assert_eq!(on("frame 2"), Some(lanes[1].uuid));
        assert_eq!(
            on("frame 3"),
            Some(lanes[0].uuid),
            "frame 1 was shown by then"
        );
        // Frame 2's life: waiting from 20 µs, built from 21 µs to 24 µs, in
        // flight until shown at 45 µs, with its deadline and target inside.
        let life: Vec<_> = events
            .iter()
            .filter(|event| event.track == lanes[1].uuid)
            .map(|event| (event.at, event.kind, event.name.as_deref()))
            .collect();
        assert_eq!(
            life,
            [
                (20_000, 1, Some("frame 2")),
                (20_000, 1, Some("waiting to begin")),
                (21_000, 2, None),
                (21_000, 1, Some("built")),
                (24_000, 2, None),
                (24_000, 1, Some("in flight")),
                (26_000, 3, Some("deadline")),
                (45_000, 2, None),
                (45_000, 3, Some("target")),
                (45_000, 2, None),
            ]
        );
        let on_screen = track(&tracks, "frames on screen");
        let shown: Vec<_> = events
            .iter()
            .filter(|event| event.track == on_screen.uuid)
            .map(|event| (event.at, event.kind))
            .collect();
        assert_eq!(
            shown,
            [(35_000, 1), (45_000, 2), (45_000, 1), (65_000, 2)],
            "each shown frame until the next replaced it"
        );
        // Every event of frame 2 carries its correlation id, which Perfetto
        // colors alike: its life, stages, and points, and its time on
        // screen.
        let frame_2: Vec<_> = events
            .iter()
            .filter(|event| event.correlation.as_deref() == Some("stream 0 frame 2"))
            .map(|event| (event.track, event.name.as_deref()))
            .collect();
        for name in [
            "frame 2",
            "waiting to begin",
            "built",
            "in flight",
            "deadline",
            "target",
        ] {
            assert!(
                frame_2.contains(&(lanes[1].uuid, Some(name))),
                "{name}: {frame_2:?}"
            );
        }
        assert!(frame_2.contains(&(on_screen.uuid, Some("frame 2"))));
        assert!(
            events
                .iter()
                .filter(|event| event.correlation.as_deref() == Some("stream 0 frame 2"))
                .all(|event| event.track != lanes[0].uuid),
            "only frame 2's events carry its id"
        );
    }

    #[test]
    fn flows_join_the_events_they_connect() {
        static GRAPHICS: TimelineDesc = TimelineDesc::gpu("graphics");
        static PASS: ScopeDesc = ScopeDesc::new("pass");
        let span = TimelineWork::new(
            &TimelineSpan::new(
                &GRAPHICS,
                &PASS,
                ContextHandle::ROOT,
                &HOST,
                TickPeriod::NANOSECOND,
                3_000,
                4_000,
            )
            .after_flow(FlowId::new(7)),
        );
        let snapshot = Snapshot::builder(SnapshotMetadata::new("app"))
            .event(TimedObservation::new(0, 0, 1, sync()))
            .event(TimedObservation::new(
                1_000,
                1,
                1,
                Observation::Flow {
                    flow_id: FlowId::new(7),
                    phase: FlowPhase::Begin,
                    context_id: ContextId::new(0),
                },
            ))
            .event(TimedObservation::new(
                5_000,
                2,
                1,
                Observation::TimelineSpan {
                    span: Box::new(span),
                },
            ))
            .build();
        let (tracks, events) = read(&to_perfetto_trace(&snapshot.into()));

        let flow = events
            .iter()
            .find(|event| event.name.as_deref() == Some("flow"))
            .expect("the flow's start");
        assert_eq!((flow.at, flow.flows.as_slice()), (1_000, &[7][..]));
        let gpu = track(&tracks, "GPU graphics");
        let pass = events
            .iter()
            .find(|event| event.track == gpu.uuid && event.kind == 1)
            .expect("the pass");
        assert_eq!(
            (
                pass.at,
                pass.name.as_deref(),
                pass.terminating_flows.as_slice()
            ),
            (3_000, Some("pass"), &[7][..])
        );
    }
}
