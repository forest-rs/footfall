// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Snapshots: the plain values a [`Runtime`] is read as.
//! They need only `alloc`, so they exist without the `std` feature.

use alloc::{boxed::Box, string::String, vec::Vec};

// For links in the docs, which describe snapshots in terms of the runtime.
#[cfg(all(doc, feature = "std"))]
use crate::{MESSAGE_TEXT_MAX_BYTES, Runtime, RuntimeBuilder, current_thread_id};
use footfall_core::{
    Args, ClockId, ClockReading, Context, ContextId, CounterDescId, EventDescId, FlowId, FlowPhase,
    FrameKind, FrameNumber, FramePointKind, FrameStream, HoldReason, InstrumentationLevel,
    MarkerKind, MarkerPhase, ResourceId, ResourceKindId, ScopeDescId, ScopeToken, SourceLocation,
    TickPeriod, TimelineId, TimelineKind, TimelineSpan, TimingBasis, Unit,
};
#[cfg(doc)]
use footfall_core::{Backend, ContextHandle};

/// Snapshot of contexts, counters, histograms, and events retained by a
/// [`Runtime`].
///
/// [`Runtime::snapshot`] and [`Runtime::snapshot_since`] return one. Read it
/// directly, or hand it on: `footfall_session::OwnedSnapshot` converts from
/// it for consumers and capture files, and `footfall_export` writes that as a
/// Chrome trace. Build one by hand with [`Snapshot::builder`].
///
/// Descriptor ids in a snapshot ([`ScopeDescId`], [`CounterDescId`],
/// [`EventDescId`], and [`ResourceKindId`]) are opaque: dense integers from
/// `1`, shared across descriptor kinds, assigned in first-seen order and
/// fixed for the life of the runtime instance, so every snapshot of one
/// instance agrees. The address-derived identity that descriptors have
/// inside the process never appears in a snapshot.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Snapshot {
    pub(crate) metadata: SnapshotMetadata,
    pub(crate) captured_at_nanos: u64,
    pub(crate) window: EventWindow,
    pub(crate) threads: Vec<ThreadSnapshot>,
    pub(crate) contexts: Vec<ContextSnapshot>,
    pub(crate) counters: Vec<CounterSnapshot>,
    pub(crate) histograms: Vec<HistogramSnapshot>,
    pub(crate) events: Vec<TimedObservation>,
    pub(crate) drops: Drops,
}

impl Snapshot {
    /// Starts building a snapshot by hand, for example as a test fixture for
    /// a snapshot consumer. Runtimes produce snapshots through
    /// [`Runtime::snapshot`] and [`Runtime::snapshot_since`].
    pub fn builder(metadata: SnapshotMetadata) -> SnapshotBuilder {
        SnapshotBuilder {
            snapshot: Self {
                metadata,
                ..Self::default()
            },
        }
    }

    /// Returns the metadata identifying the producing runtime.
    #[must_use]
    pub const fn metadata(&self) -> &SnapshotMetadata {
        &self.metadata
    }

    /// Returns the time since runtime creation when the snapshot was taken.
    #[must_use]
    pub const fn captured_at_nanos(&self) -> u64 {
        self.captured_at_nanos
    }

    /// Returns the range of event sequence numbers this snapshot covers.
    #[must_use]
    pub const fn window(&self) -> EventWindow {
        self.window
    }

    /// Returns the cursor to read the next snapshot from, with
    /// [`Runtime::snapshot_since`].
    #[must_use]
    pub const fn next_cursor(&self) -> EventCursor {
        EventCursor::new(self.metadata.instance_id, self.window.next)
    }

    /// Returns the threads that recorded this snapshot's events, sorted by
    /// id.
    #[must_use]
    pub fn threads(&self) -> &[ThreadSnapshot] {
        &self.threads
    }

    /// Returns the registered contexts the runtime holds, sorted by id and
    /// starting with the root context (id `0`).
    ///
    /// Ids carry the [`ContextHandle`] values the contexts were emitted with
    /// and stay fixed for the life of the runtime instance. Released contexts
    /// are included until the runtime retires them, so observations recorded
    /// before release still resolve.
    #[must_use]
    pub fn contexts(&self) -> &[ContextSnapshot] {
        &self.contexts
    }

    /// Returns the counters and gauges, cumulative since the runtime was
    /// created.
    #[must_use]
    pub fn counters(&self) -> &[CounterSnapshot] {
        &self.counters
    }

    /// Returns the value distributions, cumulative since the runtime was
    /// created.
    #[must_use]
    pub fn histograms(&self) -> &[HistogramSnapshot] {
        &self.histograms
    }

    /// Returns the events in the [`window`](Self::window), ordered by
    /// timestamp and then by recording sequence.
    #[must_use]
    pub fn events(&self) -> &[TimedObservation] {
        &self.events
    }

    /// Returns what the runtime could not keep or attribute.
    ///
    /// Check it before trusting totals: a snapshot with drops is missing
    /// data, and the kinds of drop say which capacity to raise. Events
    /// missing from this snapshot's window in particular are counted by
    /// [`EventWindow::missed`].
    #[must_use]
    pub const fn drops(&self) -> Drops {
        self.drops
    }

    /// Resolves one canonical context by id.
    #[must_use]
    pub fn context(&self, context_id: ContextId) -> Option<&ContextSnapshot> {
        self.contexts
            .binary_search_by_key(&context_id, |context| context.id)
            .ok()
            .map(|index| &self.contexts[index])
    }

    /// Resolves one thread by id.
    #[must_use]
    pub fn thread(&self, thread_id: u64) -> Option<&ThreadSnapshot> {
        self.threads
            .binary_search_by_key(&thread_id, |thread| thread.id)
            .ok()
            .map(|index| &self.threads[index])
    }
}

/// Builds a [`Snapshot`] by hand; see [`Snapshot::builder`].
#[derive(Clone, Debug)]
#[must_use = "a snapshot builder does nothing until `build` is called"]
pub struct SnapshotBuilder {
    pub(crate) snapshot: Snapshot,
}

impl SnapshotBuilder {
    /// Sets the time since runtime creation when the snapshot was taken.
    pub const fn captured_at_nanos(mut self, captured_at_nanos: u64) -> Self {
        self.snapshot.captured_at_nanos = captured_at_nanos;
        self
    }

    /// Sets the range of event sequence numbers the snapshot covers.
    pub const fn window(mut self, window: EventWindow) -> Self {
        self.snapshot.window = window;
        self
    }

    /// Adds one thread row. Rows are sorted by id when the snapshot is built.
    pub fn thread(mut self, thread: ThreadSnapshot) -> Self {
        self.snapshot.threads.push(thread);
        self
    }

    /// Adds one context row. Rows are sorted by id when the snapshot is built.
    pub fn context(mut self, context: ContextSnapshot) -> Self {
        self.snapshot.contexts.push(context);
        self
    }

    /// Adds one counter or gauge row.
    pub fn counter(mut self, counter: CounterSnapshot) -> Self {
        self.snapshot.counters.push(counter);
        self
    }

    /// Adds one histogram row.
    pub fn histogram(mut self, histogram: HistogramSnapshot) -> Self {
        self.snapshot.histograms.push(histogram);
        self
    }

    /// Adds one event. Events are ordered by timestamp and then by sequence
    /// when the snapshot is built.
    pub fn event(mut self, event: TimedObservation) -> Self {
        self.snapshot.events.push(event);
        self
    }

    /// Sets the drop totals reported by the snapshot.
    pub const fn drops(mut self, drops: Drops) -> Self {
        self.snapshot.drops = drops;
        self
    }

    /// Finishes the snapshot.
    #[must_use]
    pub fn build(mut self) -> Snapshot {
        self.snapshot.threads.sort_by_key(|thread| thread.id);
        self.snapshot.contexts.sort_by_key(|context| context.id);
        self.snapshot.events.sort_by_key(event_sort_key);
        self.snapshot
    }
}

/// Position in one runtime instance's event sequence.
///
/// [`Runtime::cursor`] returns the current position and
/// [`Snapshot::next_cursor`] the position after a snapshot; pass either to
/// [`Runtime::snapshot_since`] to read only newer events.
///
/// A cursor names the runtime instance it belongs to, so a cursor kept across
/// a producer restart is recognized as foreign rather than silently skipping
/// the new instance's first events.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct EventCursor {
    pub(crate) instance_id: u64,
    pub(crate) sequence: u64,
}

impl EventCursor {
    /// Creates a cursor at `sequence` in the runtime instance `instance_id`.
    #[must_use]
    pub const fn new(instance_id: u64, sequence: u64) -> Self {
        Self {
            instance_id,
            sequence,
        }
    }

    /// Returns the id of the runtime instance this cursor belongs to.
    #[must_use]
    pub const fn instance_id(self) -> u64 {
        self.instance_id
    }

    /// Returns the sequence number of the first event the next read returns.
    #[must_use]
    pub const fn sequence(self) -> u64 {
        self.sequence
    }
}

/// The range of event sequence numbers a [`Snapshot`] covers, as
/// [`Snapshot::window`] returns it.
///
/// A snapshot's events are the retained events numbered from
/// [`since`](Self::since) up to, but not including, [`next`](Self::next).
/// Events in that range that were evicted before the read are counted by
/// [`missed`](Self::missed), so `next - since == events + missed`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EventWindow {
    pub(crate) since: u64,
    pub(crate) next: u64,
    pub(crate) missed: u64,
}

impl EventWindow {
    /// Creates a window.
    #[must_use]
    pub const fn new(since: u64, next: u64, missed: u64) -> Self {
        Self {
            since,
            next,
            missed,
        }
    }

    /// Returns the first sequence number the window covers.
    ///
    /// This is the cursor's sequence for a read with a cursor from the same
    /// runtime instance, `0` for a full snapshot or a foreign cursor, and
    /// [`next`](Self::next) for a cursor ahead of every recorded event.
    #[must_use]
    pub const fn since(self) -> u64 {
        self.since
    }

    /// Returns the sequence number after the last one the window covers,
    /// where the next read starts.
    #[must_use]
    pub const fn next(self) -> u64 {
        self.next
    }

    /// Returns how many events in the window were evicted before the read.
    #[must_use]
    pub const fn missed(self) -> u64 {
        self.missed
    }
}

/// What a [`Runtime`] could not keep or attribute, as [`Snapshot::drops`]
/// returns it.
///
/// Each count names the capacity to raise; see [`Runtime`] for the overflow
/// rules. Every count except [`unattributed`](Self::unattributed) is
/// cumulative since the runtime was created.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Drops {
    pub(crate) refused_contexts: u64,
    pub(crate) unattributed: u64,
    pub(crate) updated_after_release: u64,
    pub(crate) counter_updates: u64,
    pub(crate) evicted_events: u64,
    pub(crate) retired_rows: u64,
}

impl Drops {
    /// Returns totals with nothing dropped.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            refused_contexts: 0,
            unattributed: 0,
            updated_after_release: 0,
            counter_updates: 0,
            evicted_events: 0,
            retired_rows: 0,
        }
    }

    /// Sets the number of refused context registrations.
    #[must_use]
    pub const fn with_refused_contexts(mut self, count: u64) -> Self {
        self.refused_contexts = count;
        self
    }

    /// Sets the number of unattributed counter rows and events.
    #[must_use]
    pub const fn with_unattributed(mut self, count: u64) -> Self {
        self.unattributed = count;
        self
    }

    /// Sets the number of counter and histogram rows updated after their
    /// context was released.
    #[must_use]
    pub const fn with_updated_after_release(mut self, count: u64) -> Self {
        self.updated_after_release = count;
        self
    }

    /// Sets the number of counter and histogram rows retired with their
    /// contexts.
    #[must_use]
    pub const fn with_retired_rows(mut self, count: u64) -> Self {
        self.retired_rows = count;
        self
    }

    /// Sets the number of dropped counter, gauge, and histogram updates.
    #[must_use]
    pub const fn with_counter_updates(mut self, count: u64) -> Self {
        self.counter_updates = count;
        self
    }

    /// Sets the number of evicted events.
    #[must_use]
    pub const fn with_evicted_events(mut self, count: u64) -> Self {
        self.evicted_events = count;
        self
    }

    /// Returns the number of context registrations refused because the
    /// context table was full. Raise
    /// [`context_capacity`](RuntimeBuilder::context_capacity).
    ///
    /// The table holds released contexts too, until every event recorded
    /// before their release has been evicted, so it can be full with few
    /// live contexts; [`ContextSnapshot::is_released`] shows which rows are
    /// released. A smaller event buffer retires them sooner.
    #[must_use]
    pub const fn refused_contexts(&self) -> u64 {
        self.refused_contexts
    }

    /// Returns the number of counter rows and events in this snapshot that
    /// are attributed to the root context instead of the context they were
    /// recorded with.
    ///
    /// An event goes to the root context when it was recorded after its
    /// context was released, or with a handle the runtime does not know: one
    /// refused (see [`refused_contexts`](Self::refused_contexts)), registered
    /// before this runtime was installed, never registered, or retired. A
    /// counter row goes there when its handle is unknown. The rows and
    /// events themselves are kept.
    #[must_use]
    pub const fn unattributed(&self) -> u64 {
        self.unattributed
    }

    /// Returns the number of counter and histogram rows in this snapshot
    /// that were updated after their context was released.
    ///
    /// Counter rows aggregate, so the runtime cannot move just the late
    /// updates to the root context: the row keeps its released context, and
    /// this count says its value includes updates made with a handle that
    /// was no longer valid. Keep a
    /// [`RegisteredContext`](footfall_core::RegisteredContext) alive for as long as
    /// its handle is used.
    #[must_use]
    pub const fn updated_after_release(&self) -> u64 {
        self.updated_after_release
    }

    /// Returns the number of counter and histogram rows removed, since the
    /// runtime was created, because their context was released and retired.
    ///
    /// Retiring a released context removes its rows, so totals summed across
    /// contexts no longer include what those rows held. Counted per thread
    /// that recorded the row.
    #[must_use]
    pub const fn retired_rows(&self) -> u64 {
        self.retired_rows
    }

    /// Returns the number of counter, gauge, and histogram updates dropped
    /// because their key did not fit. Raise
    /// [`counter_capacity`](RuntimeBuilder::counter_capacity).
    #[must_use]
    pub const fn counter_updates(&self) -> u64 {
        self.counter_updates
    }

    /// Returns the number of events evicted to make room for newer ones.
    ///
    /// Eviction is how the runtime keeps recent history, so this grows in
    /// any long capture; [`EventWindow::missed`] says whether a particular
    /// reader lost events to it. Raise
    /// [`event_capacity`](RuntimeBuilder::event_capacity), or read more
    /// often, to keep more.
    #[must_use]
    pub const fn evicted_events(&self) -> u64 {
        self.evicted_events
    }

    /// Returns `true` when nothing was refused, dropped, evicted, retired, or
    /// misattributed.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.refused_contexts == 0
            && self.unattributed == 0
            && self.updated_after_release == 0
            && self.counter_updates == 0
            && self.evicted_events == 0
            && self.retired_rows == 0
    }
}

/// Which runtime produced a snapshot and how it was capturing.
///
/// [`Snapshot::metadata`] returns it: the process and session names, the
/// runtime instance and process ids, the wall-clock anchor for event times,
/// the producer version, and the [`CaptureSettings`]. Pass one to
/// [`Snapshot::builder`] to build a snapshot by hand.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SnapshotMetadata {
    pub(crate) process_name: String,
    pub(crate) session_name: Option<String>,
    pub(crate) instance_id: u64,
    pub(crate) process_id: u32,
    pub(crate) wall_clock_anchor_nanos: u64,
    pub(crate) producer_version: String,
    pub(crate) capture: CaptureSettings,
}

impl SnapshotMetadata {
    /// Creates metadata for the named process, with every other field at its
    /// zero value.
    #[must_use]
    pub fn new(process_name: impl Into<String>) -> Self {
        Self {
            process_name: process_name.into(),
            ..Self::default()
        }
    }

    /// Sets the capture-session label.
    #[must_use]
    pub fn with_session_name(mut self, session_name: impl Into<String>) -> Self {
        self.session_name = Some(session_name.into());
        self
    }

    /// Sets the runtime instance id.
    #[must_use]
    pub const fn with_instance_id(mut self, instance_id: u64) -> Self {
        self.instance_id = instance_id;
        self
    }

    /// Sets the producer's operating-system process id.
    #[must_use]
    pub const fn with_process_id(mut self, process_id: u32) -> Self {
        self.process_id = process_id;
        self
    }

    /// Sets the wall-clock anchor in nanoseconds since the Unix epoch.
    #[must_use]
    pub const fn with_wall_clock_anchor_nanos(mut self, wall_clock_anchor_nanos: u64) -> Self {
        self.wall_clock_anchor_nanos = wall_clock_anchor_nanos;
        self
    }

    /// Sets the producer version.
    #[must_use]
    pub fn with_producer_version(mut self, producer_version: impl Into<String>) -> Self {
        self.producer_version = producer_version.into();
        self
    }

    /// Sets the capture settings.
    #[must_use]
    pub const fn with_capture(mut self, capture: CaptureSettings) -> Self {
        self.capture = capture;
        self
    }

    /// Returns the process name used by exporters.
    #[must_use]
    pub fn process_name(&self) -> &str {
        &self.process_name
    }

    /// Returns the capture-session label, if one is set.
    #[must_use]
    pub fn session_name(&self) -> Option<&str> {
        self.session_name.as_deref()
    }

    /// Returns the random id of the producing runtime instance; see
    /// [`Runtime::instance_id`].
    ///
    /// Context ids, descriptor ids, thread ids, and event sequence numbers
    /// are only comparable between snapshots with the same instance id.
    #[must_use]
    pub const fn instance_id(&self) -> u64 {
        self.instance_id
    }

    /// Returns the producer's operating-system process id, or `0` on
    /// platforms without one. Together with the process name it tells apart
    /// producers that offer the same session id.
    #[must_use]
    pub const fn process_id(&self) -> u32 {
        self.process_id
    }

    /// Returns the wall-clock time of runtime creation in nanoseconds since
    /// the Unix epoch.
    ///
    /// All snapshot timestamps are nanoseconds on a monotonic clock that
    /// starts at runtime creation, so adding this anchor to any `*_nanos`
    /// value yields wall-clock time. The anchor lets consumers correlate
    /// captures with logs and merge captures from several processes onto one
    /// timeline.
    #[must_use]
    pub const fn wall_clock_anchor_nanos(&self) -> u64 {
        self.wall_clock_anchor_nanos
    }

    /// Returns the version of `footfall_runtime` that produced the snapshot.
    #[must_use]
    pub fn producer_version(&self) -> &str {
        &self.producer_version
    }

    /// Returns the level and capacities the runtime was capturing with.
    #[must_use]
    pub const fn capture(&self) -> CaptureSettings {
        self.capture
    }
}

/// The level and capacities a [`Runtime`] was capturing with when it took a
/// snapshot, as [`SnapshotMetadata::capture`] returns it.
///
/// The level tells a reader what an empty snapshot means: no events at
/// [`InstrumentationLevel::Counters`] means scopes were not being captured,
/// not that none ran.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CaptureSettings {
    pub(crate) level: InstrumentationLevel,
    pub(crate) level_floor: Option<InstrumentationLevel>,
    pub(crate) counter_capacity: usize,
    pub(crate) event_capacity: usize,
    pub(crate) context_capacity: usize,
    pub(crate) counter_samples: bool,
    pub(crate) compiled_out: bool,
}

impl CaptureSettings {
    /// Creates capture settings.
    #[must_use]
    pub const fn new(
        level: InstrumentationLevel,
        counter_capacity: usize,
        event_capacity: usize,
        context_capacity: usize,
    ) -> Self {
        Self {
            level,
            level_floor: None,
            counter_capacity,
            event_capacity,
            context_capacity,
            counter_samples: false,
            compiled_out: false,
        }
    }

    /// Declares the lowest recording level over this runtime's history, combining
    /// its backend and the installed process-wide gate. Hand-built snapshots must
    /// supply this evidence explicitly. Unknown (including protocol 1.0 captures) is
    /// the default. A floor above the current level is conservatively clamped.
    #[must_use]
    pub fn with_level_floor(mut self, floor: InstrumentationLevel) -> Self {
        self.level_floor = Some(floor.min(self.level));
        self
    }

    /// Returns the producer-attested minimum recording level, or `None` when
    /// history is unknown. A current enabled level alone cannot prove that earlier
    /// events were recorded. Raising the current level never raises this floor.
    #[must_use]
    pub const fn level_floor(self) -> Option<InstrumentationLevel> {
        self.level_floor
    }

    /// Sets whether counter sampling is enabled, including explicit checkpoints.
    #[must_use]
    pub const fn with_counter_samples(mut self, enabled: bool) -> Self {
        self.counter_samples = enabled;
        self
    }

    /// Sets whether the producer was built with instrumentation compiled
    /// out.
    #[must_use]
    pub const fn with_compiled_out(mut self, compiled_out: bool) -> Self {
        self.compiled_out = compiled_out;
        self
    }

    /// Returns `true` when counter sampling is enabled and the level captures
    /// counters. Automatic marker-end sampling additionally needs a marker
    /// trigger and a level that captures markers. When `false`, no samples
    /// means they were not recorded, not that no counter changed.
    #[must_use]
    pub const fn counter_samples(self) -> bool {
        self.counter_samples
    }

    /// Returns `true` when the producer was built with
    /// `--cfg footfall_compile_out`, so instrumentation emits nothing and an
    /// empty snapshot says nothing about what ran.
    #[must_use]
    pub const fn compiled_out(self) -> bool {
        self.compiled_out
    }

    /// Returns the capture level.
    #[must_use]
    pub const fn level(self) -> InstrumentationLevel {
        self.level
    }

    /// Returns the counter-key capacity.
    #[must_use]
    pub const fn counter_capacity(self) -> usize {
        self.counter_capacity
    }

    /// Returns the event capacity.
    #[must_use]
    pub const fn event_capacity(self) -> usize {
        self.event_capacity
    }

    /// Returns the context capacity.
    #[must_use]
    pub const fn context_capacity(self) -> usize {
        self.context_capacity
    }
}

/// One thread that recorded events in a snapshot, as [`Snapshot::threads`]
/// returns it; events refer to it by [`TimedObservation::thread_id`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThreadSnapshot {
    pub(crate) id: u64,
    pub(crate) name: Option<String>,
}

impl ThreadSnapshot {
    /// Creates a thread row.
    #[must_use]
    pub const fn new(id: u64, name: Option<String>) -> Self {
        Self { id, name }
    }

    /// Returns the runtime-assigned thread id that events carry; see
    /// [`current_thread_id`].
    #[must_use]
    pub const fn id(&self) -> u64 {
        self.id
    }

    /// Returns the operating-system thread name, when the thread had one.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }
}

/// One registered context, as [`Snapshot::contexts`] returns it; rows and
/// events refer to it by [`ContextId`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextSnapshot {
    /// Canonical context id.
    pub(crate) id: ContextId,
    /// Explicit semantic context value.
    pub(crate) context: Context,
    /// Whether the context has been released.
    pub(crate) released: bool,
}

impl ContextSnapshot {
    /// Creates a row for a live context.
    #[must_use]
    pub const fn new(id: ContextId, context: Context) -> Self {
        Self {
            id,
            context,
            released: false,
        }
    }

    /// Marks the row's context as released.
    #[must_use]
    pub const fn with_released(mut self, released: bool) -> Self {
        self.released = released;
        self
    }

    /// Returns `true` when the context has been released, so the runtime
    /// keeps it only until every event recorded under it is evicted; see
    /// [`Runtime`].
    #[must_use]
    pub const fn is_released(&self) -> bool {
        self.released
    }

    /// Returns the context id, which carries the emitting [`ContextHandle`].
    #[must_use]
    pub const fn id(&self) -> ContextId {
        self.id
    }

    /// Returns the semantic context value.
    #[must_use]
    pub const fn context(&self) -> &Context {
        &self.context
    }
}

/// One counter or gauge under one context, as [`Snapshot::counters`]
/// returns it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CounterSnapshot {
    /// Opaque counter descriptor id; see [`Snapshot`].
    pub(crate) desc_id: CounterDescId,
    /// Stable descriptor name.
    pub(crate) name: &'static str,
    /// Unit copied from the counter descriptor.
    pub(crate) unit: Unit,
    /// Canonical semantic context id.
    pub(crate) context_id: ContextId,
    /// Whether the value is additive or gauge-like.
    pub(crate) kind: CounterKind,
    /// Aggregated value.
    pub(crate) value: u64,
    /// Last update time relative to runtime creation.
    pub(crate) updated_at_nanos: u64,
}

impl CounterSnapshot {
    /// Creates a counter row.
    #[must_use]
    pub const fn new(
        desc_id: CounterDescId,
        name: &'static str,
        unit: Unit,
        context_id: ContextId,
        kind: CounterKind,
        value: u64,
        updated_at_nanos: u64,
    ) -> Self {
        Self {
            desc_id,
            name,
            unit,
            context_id,
            kind,
            value,
            updated_at_nanos,
        }
    }

    /// Returns the opaque counter descriptor id; see [`Snapshot`].
    #[must_use]
    pub const fn desc_id(&self) -> CounterDescId {
        self.desc_id
    }

    /// Returns the stable descriptor name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Returns the unit copied from the counter descriptor.
    #[must_use]
    pub const fn unit(&self) -> Unit {
        self.unit
    }

    /// Returns the canonical semantic context id.
    #[must_use]
    pub const fn context_id(&self) -> ContextId {
        self.context_id
    }

    /// Returns whether the value is additive or gauge-like.
    #[must_use]
    pub const fn kind(&self) -> CounterKind {
        self.kind
    }

    /// Returns the aggregated value.
    #[must_use]
    pub const fn value(&self) -> u64 {
        self.value
    }

    /// Returns the last update time relative to runtime creation.
    #[must_use]
    pub const fn updated_at_nanos(&self) -> u64 {
        self.updated_at_nanos
    }

    /// Applies one update recorded at `at_nanos`.
    #[cfg(feature = "std")]
    pub(crate) fn apply(&mut self, value: u64, at_nanos: u64) {
        self.value = match self.kind {
            CounterKind::Counter => self.value.saturating_add(value),
            CounterKind::Gauge => value,
        };
        self.updated_at_nanos = at_nanos;
    }

    /// Folds another shard's value for the same key into this one.
    #[cfg(feature = "std")]
    pub(crate) fn merge(&mut self, other: &Self) {
        match self.kind {
            CounterKind::Counter => self.value = self.value.saturating_add(other.value),
            // The most recent write wins; writes from different threads in
            // the same nanosecond are ordered arbitrarily.
            CounterKind::Gauge => {
                if other.updated_at_nanos >= self.updated_at_nanos {
                    self.value = other.value;
                }
            }
        }
        self.updated_at_nanos = self.updated_at_nanos.max(other.updated_at_nanos);
    }
}

/// Whether a [`CounterSnapshot`] adds up (a counter) or holds the latest
/// value (a gauge); see [`CounterSnapshot::kind`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum CounterKind {
    /// Additive counter.
    Counter,
    /// Last-value gauge.
    Gauge,
}

/// Number of power-of-two buckets carried by a histogram.
pub const HISTOGRAM_BUCKET_COUNT: usize = 64;

/// One histogram under one context, as [`Snapshot::histograms`] returns it.
///
/// Bucket `i` counts samples whose bit length is `i`: bucket `0` holds zero
/// values, bucket `1` holds `1`, bucket `2` holds `2..=3`, and so on. The top
/// bucket merges 63-bit and 64-bit samples. Histogram keys count against the
/// runtime counter capacity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistogramSnapshot {
    /// Opaque counter descriptor id; see [`Snapshot`].
    pub(crate) desc_id: CounterDescId,
    /// Stable descriptor name.
    pub(crate) name: &'static str,
    /// Unit copied from the counter descriptor.
    pub(crate) unit: Unit,
    /// Canonical semantic context id.
    pub(crate) context_id: ContextId,
    /// Number of recorded samples.
    pub(crate) count: u64,
    /// Saturating sum of recorded samples.
    pub(crate) sum: u64,
    /// Smallest recorded sample. Meaningless while `count` is zero.
    pub(crate) min: u64,
    /// Largest recorded sample.
    pub(crate) max: u64,
    /// Power-of-two bucket counts indexed by sample bit length.
    pub(crate) buckets: [u64; HISTOGRAM_BUCKET_COUNT],
    /// Last update time relative to runtime creation.
    pub(crate) updated_at_nanos: u64,
}

impl HistogramSnapshot {
    /// Creates an empty histogram row; add samples with
    /// [`HistogramSnapshot::record`].
    #[must_use]
    pub const fn new(
        desc_id: CounterDescId,
        name: &'static str,
        unit: Unit,
        context_id: ContextId,
    ) -> Self {
        Self {
            desc_id,
            name,
            unit,
            context_id,
            count: 0,
            sum: 0,
            min: u64::MAX,
            max: 0,
            buckets: [0; HISTOGRAM_BUCKET_COUNT],
            updated_at_nanos: 0,
        }
    }

    /// Folds another shard's distribution for the same key into this one.
    #[cfg(feature = "std")]
    pub(crate) fn merge(&mut self, other: &Self) {
        self.count = self.count.saturating_add(other.count);
        self.sum = self.sum.saturating_add(other.sum);
        self.min = self.min.min(other.min);
        self.max = self.max.max(other.max);
        for (bucket, other) in self.buckets.iter_mut().zip(other.buckets) {
            *bucket = bucket.saturating_add(other);
        }
        self.updated_at_nanos = self.updated_at_nanos.max(other.updated_at_nanos);
    }

    /// Returns the opaque counter descriptor id; see [`Snapshot`].
    #[must_use]
    pub const fn desc_id(&self) -> CounterDescId {
        self.desc_id
    }

    /// Returns the stable descriptor name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Returns the unit copied from the counter descriptor.
    #[must_use]
    pub const fn unit(&self) -> Unit {
        self.unit
    }

    /// Returns the canonical semantic context id.
    #[must_use]
    pub const fn context_id(&self) -> ContextId {
        self.context_id
    }

    /// Returns the number of recorded samples.
    #[must_use]
    pub const fn count(&self) -> u64 {
        self.count
    }

    /// Returns the saturating sum of recorded samples.
    #[must_use]
    pub const fn sum(&self) -> u64 {
        self.sum
    }

    /// Returns the smallest recorded sample. Meaningless while
    /// [`count`](Self::count) is zero.
    #[must_use]
    pub const fn min(&self) -> u64 {
        self.min
    }

    /// Returns the largest recorded sample.
    #[must_use]
    pub const fn max(&self) -> u64 {
        self.max
    }

    /// Returns the power-of-two bucket counts indexed by sample bit length.
    #[must_use]
    pub const fn buckets(&self) -> &[u64; HISTOGRAM_BUCKET_COUNT] {
        &self.buckets
    }

    /// Returns the last update time relative to runtime creation.
    #[must_use]
    pub const fn updated_at_nanos(&self) -> u64 {
        self.updated_at_nanos
    }

    /// Records one sample at the given time relative to runtime creation.
    pub fn record(&mut self, value: u64, at_nanos: u64) {
        self.count = self.count.saturating_add(1);
        self.sum = self.sum.saturating_add(value);
        self.min = self.min.min(value);
        self.max = self.max.max(value);
        self.buckets[histogram_bucket_index(value)] += 1;
        self.updated_at_nanos = at_nanos;
    }
}

/// Returns the bucket index for one histogram sample.
#[must_use]
pub fn histogram_bucket_index(value: u64) -> usize {
    ((u64::BITS - value.leading_zeros()) as usize).min(HISTOGRAM_BUCKET_COUNT - 1)
}

/// One retained event, as [`Snapshot::events`] returns it: an
/// [`Observation`] with its thread, sequence number, and timestamp relative
/// to runtime creation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimedObservation {
    /// Observation time in nanoseconds since runtime creation.
    pub(crate) at_nanos: u64,
    /// Runtime-assigned recording sequence number.
    ///
    /// Sequence numbers start at `0` for each runtime instance and increase
    /// by one per event in recording order across all threads; cursors and
    /// [`EventWindow`]s are positions in this sequence. Snapshots order
    /// events by timestamp and then by sequence, so events recorded by one
    /// thread at the same timestamp keep their program order.
    pub(crate) sequence: u64,
    /// Runtime-assigned id of the thread that recorded the observation.
    ///
    /// Ids are small integers assigned in first-emission order. They identify
    /// threads within one process lifetime and are not OS thread ids.
    pub(crate) thread_id: u64,
    /// Observation payload.
    pub(crate) observation: Observation,
}

impl TimedObservation {
    /// Creates a timed observation.
    #[must_use]
    pub const fn new(
        at_nanos: u64,
        sequence: u64,
        thread_id: u64,
        observation: Observation,
    ) -> Self {
        Self {
            at_nanos,
            sequence,
            thread_id,
            observation,
        }
    }

    /// Returns the observation time in nanoseconds since runtime creation.
    #[must_use]
    pub const fn at_nanos(&self) -> u64 {
        self.at_nanos
    }

    /// Returns the runtime-assigned recording sequence number.
    ///
    /// Sequence numbers start at `0` for each runtime instance and increase
    /// by one per event in recording order across all threads; cursors and
    /// [`EventWindow`]s are positions in this sequence. Snapshots order
    /// events by timestamp and then by sequence, so events recorded by one
    /// thread at the same timestamp keep their program order.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns the runtime-assigned id of the thread that recorded the
    /// observation.
    ///
    /// Ids are small integers assigned in first-emission order. They identify
    /// threads within one process lifetime and are not OS thread ids.
    #[must_use]
    pub const fn thread_id(&self) -> u64 {
        self.thread_id
    }

    /// Returns the observation payload.
    #[must_use]
    pub const fn observation(&self) -> &Observation {
        &self.observation
    }
}

/// What one event recorded, as [`TimedObservation::observation`] returns it.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Observation {
    /// Scope begin event.
    ScopeBegin {
        /// Opaque descriptor id for the scope; see [`Snapshot`].
        desc_id: ScopeDescId,
        /// Stable descriptor name.
        name: &'static str,
        /// Where the scope descriptor is declared, when it records that.
        ///
        /// It is the descriptor's own location, so recording one copies a
        /// reference, never its text.
        location: Option<&'static SourceLocation>,
        /// Canonical semantic context id.
        context_id: ContextId,
        /// Opaque scope token returned to the caller.
        token: ScopeToken,
        /// Per-instance arguments carried by this scope begin.
        args: Args,
    },
    /// Scope end event.
    ScopeEnd {
        /// Opaque scope token previously returned from `scope_begin`.
        token: ScopeToken,
    },
    /// Resource creation event.
    ResourceCreate {
        /// Opaque resource kind id; see [`Snapshot`].
        kind_id: ResourceKindId,
        /// Stable resource kind name.
        kind_name: &'static str,
        /// Resource instance id.
        resource_id: ResourceId,
        /// Canonical semantic context id.
        context_id: ContextId,
        /// Logical resource size.
        size: u64,
    },
    /// Resource update event.
    ResourceUpdate {
        /// Opaque resource kind id; see [`Snapshot`].
        kind_id: ResourceKindId,
        /// Stable resource kind name.
        kind_name: &'static str,
        /// Resource instance id.
        resource_id: ResourceId,
        /// Canonical semantic context id.
        context_id: ContextId,
        /// Logical resource size.
        size: u64,
    },
    /// Resource drop event.
    ResourceDrop {
        /// Opaque resource kind id; see [`Snapshot`].
        kind_id: ResourceKindId,
        /// Stable resource kind name.
        kind_name: &'static str,
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
        /// The program's own number for the unit of work, in its frame
        /// stream, when it gave one.
        number: Option<FrameNumber>,
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
    /// Message event with bounded text.
    Message {
        /// Opaque descriptor id for the message channel; see
        /// [`Snapshot`].
        desc_id: EventDescId,
        /// Stable descriptor name.
        name: &'static str,
        /// Canonical semantic context id.
        context_id: ContextId,
        /// Message text, truncated to [`MESSAGE_TEXT_MAX_BYTES`].
        text: String,
        /// Per-instance arguments carried by this message.
        args: Args,
    },
    /// A counter's or gauge's value at a sampling point.
    ///
    /// When the runtime records counter samples (see
    /// [`RuntimeBuilder::counter_samples`]), configured marker ends record
    /// changed rows; explicit checkpoints record every counter and gauge.
    /// Counter values are cumulative, so successive values measure global
    /// interval activity rather than exclusive work by a marker or scope.
    CounterSample {
        /// Opaque counter descriptor id; see [`Snapshot`].
        desc_id: CounterDescId,
        /// Stable descriptor name.
        name: &'static str,
        /// Unit copied from the counter descriptor.
        unit: Unit,
        /// Canonical semantic context id.
        context_id: ContextId,
        /// Whether the value is additive or gauge-like.
        kind: CounterKind,
        /// The value at the end of the frame.
        value: u64,
    },
    /// The boundary after a named batch of counter/gauge samples.
    CounterCheckpoint {
        /// Checkpoint descriptor id.
        desc_id: EventDescId,
        /// Stable checkpoint name.
        name: &'static str,
        /// Originating work; sampled counters can belong to other contexts.
        context_id: ContextId,
        /// Number of sample events emitted immediately before this boundary.
        samples: u64,
    },
    /// A clock other than the runtime's read [`ClockTicks::ticks`] at the
    /// observation's time; see [`footfall_core::clock_sync`].
    ClockSync {
        /// The clock's reading.
        clock: ClockTicks,
    },
    /// Two clocks other than the runtime's read these ticks at the same
    /// moment; see [`footfall_core::clock_pair`].
    ClockPair {
        /// The first clock's reading.
        first: ClockTicks,
        /// The second clock's reading.
        second: ClockTicks,
    },
    /// Finished work on a timeline that is not a CPU thread, timed by
    /// another clock; see [`footfall_core::timeline_span`]. It is boxed, so
    /// every other event stays as small as before.
    TimelineSpan {
        /// The work.
        span: Box<TimelineWork>,
    },
    /// A time a frame scheduler or display knows about a numbered frame;
    /// see [`footfall_core::frame_point`].
    FramePoint {
        /// The program's number for the frame, in its stream.
        number: FrameNumber,
        /// What the point says about the frame.
        kind: FramePointKind,
        /// When, as a reading of another clock; `None` for a dropped frame.
        time: Option<ClockTicks>,
        /// How the recorder came by the time.
        basis: TimingBasis,
        /// What the recorder knew about the frame at this point.
        args: Args,
    },
    /// A chance the platform gave the program to begin a frame; see
    /// [`footfall_core::frame_opportunity`].
    FrameOpportunity {
        /// The frame stream it was for.
        stream: FrameStream,
        /// When, as a reading of another clock.
        time: ClockTicks,
        /// How many ticks of that clock the platform meant to leave before
        /// the next opportunity, if given.
        interval: Option<u64>,
        /// What the recorder knew at the opportunity.
        args: Args,
    },
    /// What the frame scheduler decided for a frame beyond its times; see
    /// [`footfall_core::frame_plan`].
    FramePlan {
        /// The frame.
        number: FrameNumber,
        /// The period of the ticks the durations are in.
        period: TickPeriod,
        /// The ticks meant between this frame and the one before it, if
        /// given.
        interval: Option<u64>,
        /// How many frames deep the pipeline was planned to be, if given.
        depth: Option<u8>,
        /// How many ticks later than the platform's prediction the frame
        /// was aimed for, if given.
        latency_allowance: Option<u64>,
        /// How many ticks the frame was held back by, and why, if it was.
        hold: Option<(u64, HoldReason)>,
        /// Whether the frame was paced.
        kind: FrameKind,
        /// What else the scheduler knew.
        args: Args,
    },
}

/// Finished work on a timeline that is not a CPU thread, as
/// [`Observation::TimelineSpan`] carries it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimelineWork {
    pub(crate) timeline_id: TimelineId,
    pub(crate) timeline_name: &'static str,
    pub(crate) timeline_kind: TimelineKind,
    pub(crate) desc_id: ScopeDescId,
    pub(crate) name: &'static str,
    pub(crate) location: Option<&'static SourceLocation>,
    pub(crate) context_id: ContextId,
    pub(crate) begin: ClockTicks,
    pub(crate) end: u64,
    pub(crate) frame: Option<FrameNumber>,
    pub(crate) flow: Option<FlowId>,
    pub(crate) args: Args,
}

impl TimelineWork {
    /// Captures `span`, recording its descriptors by id and name.
    #[must_use]
    pub fn new(span: &TimelineSpan) -> Self {
        Self {
            timeline_id: span.timeline().id(),
            timeline_name: span.timeline().name(),
            timeline_kind: span.timeline().kind(),
            desc_id: span.scope().id(),
            name: span.scope().name(),
            location: span.scope().location(),
            context_id: span.context().id(),
            begin: ClockTicks::new(
                span.clock().id(),
                span.clock().name(),
                span.begin(),
                span.period(),
            ),
            end: span.end(),
            frame: span.frame(),
            flow: span.flow(),
            args: *span.args(),
        }
    }

    /// Returns the timeline descriptor's id; see [`Snapshot`].
    #[must_use]
    pub const fn timeline_id(&self) -> TimelineId {
        self.timeline_id
    }

    /// Returns the timeline's name.
    #[must_use]
    pub const fn timeline_name(&self) -> &'static str {
        self.timeline_name
    }

    /// Returns what the timeline stands for.
    #[must_use]
    pub const fn timeline_kind(&self) -> TimelineKind {
        self.timeline_kind
    }

    /// Returns the scope descriptor's id; see [`Snapshot`].
    #[must_use]
    pub const fn desc_id(&self) -> ScopeDescId {
        self.desc_id
    }

    /// Returns the scope descriptor's name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Returns where the scope descriptor is declared, when it records that.
    #[must_use]
    pub const fn location(&self) -> Option<&'static SourceLocation> {
        self.location
    }

    /// Returns the context the work is attributed to.
    #[must_use]
    pub const fn context_id(&self) -> ContextId {
        self.context_id
    }

    /// Returns the clock and the tick the work began at.
    #[must_use]
    pub const fn begin(&self) -> ClockTicks {
        self.begin
    }

    /// Returns the tick of the same clock the work ended at.
    #[must_use]
    pub const fn end(&self) -> u64 {
        self.end
    }

    /// Returns the program's number for the frame the work belongs to.
    #[must_use]
    pub const fn frame(&self) -> Option<FrameNumber> {
        self.frame
    }

    /// Returns the flow the work ends.
    #[must_use]
    pub const fn flow(&self) -> Option<FlowId> {
        self.flow
    }

    /// Returns the per-instance arguments.
    #[must_use]
    pub const fn args(&self) -> &Args {
        &self.args
    }
}

/// One reading of a clock other than the runtime's, as
/// [`Observation::ClockSync`] and [`Observation::ClockPair`] carry it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClockTicks {
    pub(crate) clock_id: ClockId,
    pub(crate) name: &'static str,
    pub(crate) ticks: u64,
    pub(crate) period: TickPeriod,
}

impl ClockTicks {
    /// Creates a reading of the clock `clock_id`, named `name`.
    #[must_use]
    pub const fn new(
        clock_id: ClockId,
        name: &'static str,
        ticks: u64,
        period: TickPeriod,
    ) -> Self {
        Self {
            clock_id,
            name,
            ticks,
            period,
        }
    }

    /// Returns the clock descriptor's id; see [`Snapshot`].
    #[must_use]
    pub const fn clock_id(&self) -> ClockId {
        self.clock_id
    }

    /// Returns the clock's name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Returns the tick count read.
    #[must_use]
    pub const fn ticks(&self) -> u64 {
        self.ticks
    }

    /// Returns how long one tick lasts.
    #[must_use]
    pub const fn period(&self) -> TickPeriod {
        self.period
    }
}

impl From<ClockReading> for ClockTicks {
    fn from(reading: ClockReading) -> Self {
        Self::new(
            reading.clock().id(),
            reading.clock().name(),
            reading.ticks(),
            reading.period(),
        )
    }
}

impl Observation {
    /// Returns the canonical context id carried by this observation when present.
    #[must_use]
    pub fn context_id(&self) -> Option<ContextId> {
        match self {
            Self::ScopeBegin { context_id, .. }
            | Self::ResourceCreate { context_id, .. }
            | Self::ResourceUpdate { context_id, .. }
            | Self::ResourceDrop { context_id, .. }
            | Self::FrameMarker { context_id, .. }
            | Self::Flow { context_id, .. }
            | Self::Message { context_id, .. }
            | Self::CounterSample { context_id, .. }
            | Self::CounterCheckpoint { context_id, .. } => Some(*context_id),
            Self::TimelineSpan { span } => Some(span.context_id),
            Self::ScopeEnd { .. }
            | Self::ClockSync { .. }
            | Self::ClockPair { .. }
            | Self::FramePoint { .. }
            | Self::FrameOpportunity { .. }
            | Self::FramePlan { .. } => None,
        }
    }

    #[cfg(feature = "std")]
    pub(crate) fn context_id_mut(&mut self) -> Option<&mut ContextId> {
        match self {
            Self::ScopeBegin { context_id, .. }
            | Self::ResourceCreate { context_id, .. }
            | Self::ResourceUpdate { context_id, .. }
            | Self::ResourceDrop { context_id, .. }
            | Self::FrameMarker { context_id, .. }
            | Self::Flow { context_id, .. }
            | Self::Message { context_id, .. }
            | Self::CounterSample { context_id, .. }
            | Self::CounterCheckpoint { context_id, .. } => Some(context_id),
            Self::TimelineSpan { span } => Some(&mut span.context_id),
            Self::ScopeEnd { .. }
            | Self::ClockSync { .. }
            | Self::ClockPair { .. }
            | Self::FramePoint { .. }
            | Self::FrameOpportunity { .. }
            | Self::FramePlan { .. } => None,
        }
    }
}

/// Orders events by time, then by sequence for events at the same time.
pub(crate) fn event_sort_key(event: &TimedObservation) -> (u64, u64) {
    (event.at_nanos, event.sequence)
}
