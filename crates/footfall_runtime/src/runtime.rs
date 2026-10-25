// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The collector: a [`Runtime`] and how it records and is read.

use crate::snapshot::event_sort_key;
use crate::{
    CaptureSettings, ClockTicks, ContextSnapshot, CounterKind, CounterSnapshot, Drops, EventCursor,
    EventWindow, HistogramSnapshot, Observation, Snapshot, SnapshotMetadata, ThreadSnapshot,
    TimedObservation, TimelineWork,
};
use footfall_core::{
    Args, Backend, ClockId, ClockReading, Context, ContextHandle, ContextId, CounterDesc,
    CounterDescId, EventDesc, EventDescId, FlowId, FlowPhase, FrameNumber, FrameOpportunity,
    FramePlan, FramePoint, InstallBackendError, InstrumentationLevel, MarkerKind, MarkerPhase,
    ResourceId, ResourceKindDesc, ResourceKindId, ScopeDesc, ScopeDescId, ScopeToken, TimelineId,
    TimelineSpan, install_backend, set_max_level,
};
use hashbrown::{HashMap, HashSet};
use std::{
    boxed::Box,
    collections::{BTreeMap, VecDeque},
    string::String,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    vec::Vec,
};

const DEFAULT_SHARD_COUNT: usize = 16;

/// The runtime installed with [`Runtime::install_global`].
static GLOBAL_RUNTIME: std::sync::OnceLock<&'static Runtime> = std::sync::OnceLock::new();

/// Maximum retained message text length in bytes.
///
/// Longer texts are truncated on a UTF-8 boundary before retention so one
/// noisy log line cannot dominate event memory.
pub const MESSAGE_TEXT_MAX_BYTES: usize = 256;

/// In-process bounded collector for counters and timestamped observation records.
///
/// A runtime is a [`Backend`]: install it with
/// [`install_global`](RuntimeBuilder::install_global) so instrumented code
/// reaches it, or call its [`Backend`] methods directly, as tests do.
/// [`Runtime::global`] returns the installed runtime.
///
/// Build one with [`Runtime::builder`], which documents every setting and
/// its default.
///
/// # Reading
///
/// A runtime keeps a recent window of history rather than a session that
/// someone must drain, so any number of readers can read it without
/// interfering. [`snapshot`](Runtime::snapshot) returns everything retained;
/// [`snapshot_since`](Runtime::snapshot_since) returns the events recorded
/// since an [`EventCursor`], plus the cursor to read from next and how many
/// events in between were evicted before the reader got to them:
///
/// ```
/// use footfall_core::{Backend, ContextHandle, ScopeDesc};
/// use footfall_runtime::Runtime;
///
/// static WORK: ScopeDesc = ScopeDesc::new("work");
/// let runtime = Runtime::default();
///
/// let cursor = runtime.cursor();
/// let token = runtime.scope_begin(&WORK, ContextHandle::ROOT, &Default::default());
/// runtime.scope_end(token);
///
/// let window = runtime.snapshot_since(cursor);
/// assert_eq!(window.events().len(), 2);
/// assert_eq!(window.window().missed(), 0);
/// let next = window.next_cursor();
/// assert!(runtime.snapshot_since(next).events().is_empty());
/// ```
///
/// Counters, gauges, and histograms are cumulative since the runtime was
/// created, so every snapshot carries their current values.
///
/// Every runtime has a random [`instance_id`](Runtime::instance_id), carried
/// by its snapshots and cursors. A cursor from another instance, for example
/// one taken before the producing process restarted, reads from the start.
///
/// # Capacity
///
/// Every buffer has a fixed capacity, and nothing the runtime keeps grows
/// past it:
///
/// - **Events.** Once [`event_capacity`](RuntimeBuilder::event_capacity)
///   events are retained, each new event evicts the oldest event recorded by
///   the same thread, or by another thread when the recording thread has
///   none. Eviction makes room for recent history, so a long capture keeps
///   its newest events, and a scope can lose its begin while keeping its end.
/// - **Counter keys.** A key is one counter, gauge, or histogram descriptor
///   under one context, admitted once for the whole runtime whichever threads
///   record it. Once [`counter_capacity`](RuntimeBuilder::counter_capacity)
///   keys exist, the runtime first retires keys of released contexts (see
///   below) and otherwise drops updates to new keys.
/// - **Contexts.** Once
///   [`context_capacity`](RuntimeBuilder::context_capacity) contexts are
///   registered, the runtime first retires released contexts and otherwise
///   refuses the registration. Observations emitted with a refused handle,
///   or with any handle the runtime does not know, are kept and attributed
///   to the root context.
///
/// What does not fit is counted in [`Snapshot::drops`], and each count names
/// the capacity to raise.
///
/// # Contexts
///
/// The runtime keeps the contexts registered through
/// [`footfall_core::register_context`] in a table keyed by handle. Emissions
/// carry only the handle and never touch the table, so attribution adds no
/// locking or hashing to the hot path; snapshots resolve handles to contexts.
///
/// A released context stays in the table, with its counter rows, until the
/// runtime needs the room and no retained event can refer to it: that is,
/// once every event recorded before its release has been evicted. Retiring
/// it removes its counter rows too, so their final values are only visible to
/// readers that read before then, and [`Drops::retired_rows`] counts them.
/// Size [`context_capacity`](RuntimeBuilder::context_capacity) for the live
/// contexts plus the released ones the event buffer still refers to; snapshots
/// mark released contexts with [`ContextSnapshot::is_released`].
///
/// A handle must not be used after its context is released. When it is
/// anyway, events recorded with it go to the root context and are counted in
/// [`Drops::unattributed`], whether or not the context has been retired yet.
/// Counter rows cannot split one context's updates from another's, so a row
/// updated after its context's release keeps that context and is counted in
/// [`Drops::updated_after_release`].
///
/// # Threads
///
/// Each recording thread writes its own shard of counters and events, so
/// threads updating the same counter do not contend, and snapshots merge the
/// shards. Snapshots name each thread that recorded an event after its
/// operating-system thread name, when it has one.
#[derive(Debug)]
pub struct Runtime {
    instance_id: u64,
    process_id: u32,
    created_at: Instant,
    wall_clock_anchor_nanos: u64,
    process_name: String,
    /// Packed current level (low two bits) and lifetime minimum (next two).
    level: AtomicU8,
    /// Set once this runtime is the process-wide backend; its level then
    /// also drives `footfall_core`'s maximum level.
    drives_max_level: AtomicBool,
    counter_capacity: usize,
    event_capacity: usize,
    context_capacity: usize,
    counter_samples: bool,
    counter_sample_markers: [bool; 3],
    /// When the previous automatic sample or explicit checkpoint ran.
    last_counter_sample_nanos: AtomicU64,
    next_scope_token: AtomicU64,
    /// Sequence number of the next event.
    ///
    /// Allocated while holding the recording thread's event shard, so a
    /// reader that loads it and then locks each shard sees every event
    /// numbered below the loaded value that has not been evicted.
    next_event_sequence: AtomicU64,
    /// Counter and histogram keys admitted against `counter_capacity`.
    ///
    /// Shards consult it only the first time they see a key, so steady-state
    /// updates never touch it.
    admitted_keys: Mutex<HashSet<AdmittedKey>>,
    event_count: AtomicUsize,
    dropped_counters: AtomicU64,
    evicted_events: AtomicU64,
    /// Counter and histogram rows removed with retired contexts.
    retired_rows: AtomicU64,
    labels: Mutex<LabelState>,
    contexts: Mutex<ContextTable>,
    counter_shards: Vec<Mutex<CounterShard>>,
    event_shards: Vec<Mutex<EventShard>>,
}

impl Default for Runtime {
    /// Builds a runtime with every [`RuntimeBuilder`] default.
    fn default() -> Self {
        Self::builder().build()
    }
}

impl Runtime {
    /// Starts configuring a runtime.
    ///
    /// ```
    /// use footfall_core::InstrumentationLevel;
    /// use footfall_runtime::Runtime;
    ///
    /// let runtime = Runtime::builder()
    ///     .process_name("my_app")
    ///     .level(InstrumentationLevel::Scoped)
    ///     .event_capacity(16 * 1024)
    ///     .build();
    /// assert_eq!(runtime.level(), InstrumentationLevel::Scoped);
    /// ```
    pub fn builder() -> RuntimeBuilder {
        RuntimeBuilder::default()
    }

    fn from_builder(builder: RuntimeBuilder) -> Self {
        let RuntimeBuilder {
            process_name,
            session_name,
            level,
            counter_capacity,
            event_capacity,
            context_capacity,
            counter_samples,
            counter_sample_markers,
        } = builder;
        let wall_clock = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO);
        Self {
            instance_id: new_instance_id(wall_clock),
            process_id: current_process_id(),
            created_at: Instant::now(),
            wall_clock_anchor_nanos: duration_to_nanos(wall_clock),
            process_name: process_name.unwrap_or_else(default_process_name),
            level: AtomicU8::new(level_to_u8(level) | (level_to_u8(level) << 2)),
            drives_max_level: AtomicBool::new(false),
            counter_capacity,
            event_capacity,
            context_capacity,
            counter_samples,
            counter_sample_markers,
            last_counter_sample_nanos: AtomicU64::new(0),
            next_scope_token: AtomicU64::new(1),
            next_event_sequence: AtomicU64::new(0),
            admitted_keys: Mutex::new(HashSet::new()),
            event_count: AtomicUsize::new(0),
            dropped_counters: AtomicU64::new(0),
            evicted_events: AtomicU64::new(0),
            retired_rows: AtomicU64::new(0),
            labels: Mutex::new(LabelState {
                session_name,
                descriptor_ids: DescriptorIds::default(),
            }),
            contexts: Mutex::new(ContextTable::new(context_capacity)),
            counter_shards: build_counter_shards(DEFAULT_SHARD_COUNT),
            event_shards: build_event_shards(DEFAULT_SHARD_COUNT),
        }
    }

    /// Installs this runtime as the process-wide backend and keeps it alive for the process.
    ///
    /// The runtime is leaked so the returned reference stays valid for the rest of the
    /// process. To compose the runtime with other backends instead, leak it yourself
    /// (for example with [`Box::leak`]) and pass the reference to an adapter such as
    /// `footfall_fanout`, or to [`footfall_core::install_backend`].
    ///
    /// Once installed this way, the runtime's capture level also sets
    /// [`footfall_core::set_max_level`], now and on every
    /// [`set_level`](Self::set_level), so emissions the level excludes return
    /// before reaching the runtime at all. Ends for previously admitted scopes
    /// still reach the runtime after a level reduction. A runtime installed any
    /// other way leaves the process maximum level to whoever installed it.
    pub fn install_global(self) -> Result<&'static Self, InstallBackendError> {
        let runtime: &'static Self = Box::leak(Box::new(self));
        install_backend(runtime)?;
        let _ = GLOBAL_RUNTIME.set(runtime);
        runtime.drives_max_level.store(true, Ordering::Relaxed);
        set_max_level(runtime.level());
        Ok(runtime)
    }

    /// Returns the runtime installed with [`install_global`](Self::install_global),
    /// if any, for code that needs to read it but did not install it.
    #[must_use]
    pub fn global() -> Option<&'static Self> {
        GLOBAL_RUNTIME.get().copied()
    }

    /// Returns the random id that distinguishes this runtime from every
    /// other, including runtimes in earlier runs of the same process.
    #[must_use]
    pub const fn instance_id(&self) -> u64 {
        self.instance_id
    }

    /// Returns the operating-system process id reported in snapshots, or `0`
    /// on platforms without one.
    #[must_use]
    pub const fn process_id(&self) -> u32 {
        self.process_id
    }

    /// Returns the process name reported in snapshots.
    #[must_use]
    pub fn process_name(&self) -> &str {
        &self.process_name
    }

    /// Returns the capture-session label reported in snapshots, if one is
    /// set.
    #[must_use]
    pub fn session_name(&self) -> Option<String> {
        self.labels
            .lock()
            .expect("runtime label mutex should not be poisoned")
            .session_name
            .clone()
    }

    /// Returns the current runtime capture level.
    #[must_use]
    pub fn level(&self) -> InstrumentationLevel {
        level_from_u8(self.level.load(Ordering::Relaxed) & 0b11)
    }

    /// Sets the runtime capture level used by subsequent backend emissions.
    ///
    /// Previously admitted scopes still emit their ends after the level is lowered.
    ///
    /// For a runtime installed with [`install_global`](Self::install_global)
    /// this also sets the process-wide [`footfall_core::max_level`].
    pub fn set_level(&self, level: InstrumentationLevel) {
        let next = level_to_u8(level);
        let mut state = self.level.load(Ordering::Relaxed);
        loop {
            let updated = next | (((state >> 2) & 0b11).min(next) << 2);
            match self.level.compare_exchange_weak(
                state,
                updated,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(current) => state = current,
            }
        }
        if self.drives_max_level.load(Ordering::Relaxed) {
            set_max_level(level);
        }
    }

    /// Sets an optional capture-session label for future snapshots.
    ///
    /// The label can be changed after the runtime is installed as the
    /// process-wide backend.
    pub fn set_session_name(&self, session_name: impl Into<String>) {
        self.labels
            .lock()
            .expect("runtime label mutex should not be poisoned")
            .session_name = Some(session_name.into());
    }

    /// Clears the capture-session label for future snapshots.
    pub fn clear_session_name(&self) {
        self.labels
            .lock()
            .expect("runtime label mutex should not be poisoned")
            .session_name = None;
    }

    /// Returns a cursor positioned after every event recorded so far.
    ///
    /// Pass it to [`snapshot_since`](Self::snapshot_since) to read only what
    /// is recorded from now on, for example to capture one region of work.
    #[must_use]
    pub fn cursor(&self) -> EventCursor {
        EventCursor::new(
            self.instance_id,
            self.next_event_sequence.load(Ordering::SeqCst),
        )
    }

    /// Returns a snapshot of everything retained: contexts, counters,
    /// histograms, and every event still in the buffer.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        self.snapshot_from(0)
    }

    /// Returns a snapshot whose events are those recorded at or after
    /// `cursor`, with the current contexts, counters, and histograms.
    ///
    /// [`Snapshot::window`] reports the range of event sequence numbers the
    /// snapshot covers and how many events in that range were evicted before
    /// this read, and [`Snapshot::next_cursor`] is where the next read should
    /// start. A cursor from another runtime instance reads from the start.
    #[must_use]
    pub fn snapshot_since(&self, cursor: EventCursor) -> Snapshot {
        let since = if cursor.instance_id == self.instance_id {
            cursor.sequence
        } else {
            0
        };
        self.snapshot_from(since)
    }

    fn metadata(&self, labels: &LabelState) -> SnapshotMetadata {
        let state = self.level.load(Ordering::Relaxed);
        let level = level_from_u8(state & 0b11);
        let floor = level_from_u8((state >> 2) & 0b11).min(footfall_core::max_level_floor());
        SnapshotMetadata {
            process_name: self.process_name.clone(),
            session_name: labels.session_name.clone(),
            instance_id: self.instance_id,
            wall_clock_anchor_nanos: self.wall_clock_anchor_nanos,
            producer_version: String::from(PRODUCER_VERSION),
            process_id: self.process_id,
            capture: CaptureSettings {
                level,
                level_floor: Some(floor.min(level)),
                counter_capacity: self.counter_capacity,
                event_capacity: self.event_capacity,
                context_capacity: self.context_capacity,
                // Sampling is configured once; historical availability is qualified
                // separately by the recording floor.
                counter_samples: self.counter_samples && level.captures_counters(),
                compiled_out: cfg!(footfall_compile_out),
            },
        }
    }

    fn now_nanos(&self) -> u64 {
        // A thread's first emission registers the thread, which takes a lock
        // and copies its name; do it before reading the clock, so the first
        // scope a thread records is not inflated by that setup.
        let _ = current_thread_id();
        duration_to_nanos(self.created_at.elapsed())
    }

    fn snapshot_from(&self, since: u64) -> Snapshot {
        let captured_at_nanos = self.now_nanos();
        // Load the watermark before locking any event shard: every event
        // numbered below it was pushed under its shard's lock before the
        // number was published, so the reads below see it unless it was
        // evicted.
        let next = self.next_event_sequence.load(Ordering::SeqCst);
        let since = since.min(next);

        let mut labels = self
            .labels
            .lock()
            .expect("runtime label mutex should not be poisoned");
        let contexts = self
            .contexts
            .lock()
            .expect("runtime context mutex should not be poisoned");
        // Each recording thread writes its own shard, so one key can appear
        // in several shards; merge them, resolving handles the table does not
        // know to the root context on the way.
        let mut tally = ResolveTally::default();
        let mut merged_counters: HashMap<CounterKey, CounterSnapshot> = HashMap::new();
        let mut merged_histograms: HashMap<HistogramKey, HistogramSnapshot> = HashMap::new();
        for shard in &self.counter_shards {
            let shard = shard
                .lock()
                .expect("runtime counter shard should not be poisoned");
            for counter in shard.counters.values() {
                let mut counter = counter.clone();
                counter.context_id =
                    contexts.resolve_row(counter.context_id, counter.updated_at_nanos, &mut tally);
                let key = CounterKey::new(counter.desc_id, counter.context_id, counter.kind);
                match merged_counters.get_mut(&key) {
                    Some(existing) => existing.merge(&counter),
                    None => {
                        merged_counters.insert(key, counter);
                    }
                }
            }
            for histogram in shard.histograms.values() {
                let mut histogram = histogram.clone();
                histogram.context_id = contexts.resolve_row(
                    histogram.context_id,
                    histogram.updated_at_nanos,
                    &mut tally,
                );
                let key = HistogramKey {
                    desc_id: histogram.desc_id,
                    context_id: histogram.context_id,
                };
                match merged_histograms.get_mut(&key) {
                    Some(existing) => existing.merge(&histogram),
                    None => {
                        merged_histograms.insert(key, histogram);
                    }
                }
            }
        }
        let mut counters: Vec<_> = merged_counters.into_values().collect();
        let mut histograms: Vec<_> = merged_histograms.into_values().collect();

        let mut events = Vec::new();
        for shard in &self.event_shards {
            let shard = shard
                .lock()
                .expect("runtime event shard should not be poisoned");
            // A shard's events are in sequence order, because sequence
            // numbers are allocated under its lock, so the window is a
            // contiguous range found by binary search.
            let start = shard.events.partition_point(|event| event.sequence < since);
            events.extend(
                shard
                    .events
                    .range(start..)
                    .take_while(|event| event.sequence < next)
                    .cloned(),
            );
        }
        events.sort_by_key(event_sort_key);
        for event in &mut events {
            let sequence = event.sequence;
            if let Some(context_id) = event.observation.context_id_mut() {
                *context_id = contexts.resolve_event(*context_id, sequence, &mut tally);
            }
        }
        let delivered = u64::try_from(events.len()).unwrap_or(u64::MAX);
        let window = EventWindow {
            since,
            next,
            missed: (next - since).saturating_sub(delivered),
        };
        let threads = thread_snapshots(&events);

        // Counter maps iterate in arbitrary order; sort so snapshots, and
        // the dense descriptor ids assigned below, are deterministic.
        counters.sort_by_key(|counter| {
            (
                counter.name,
                counter.context_id,
                counter_kind_discriminant(counter.kind),
            )
        });
        histograms.sort_by_key(|histogram| (histogram.name, histogram.context_id));
        labels
            .descriptor_ids
            .remap(&mut events, &mut counters, &mut histograms);

        Snapshot {
            metadata: self.metadata(&labels),
            captured_at_nanos,
            window,
            threads,
            contexts: contexts.snapshot(),
            counters,
            histograms,
            events,
            drops: Drops {
                refused_contexts: contexts.refused,
                unattributed: tally.unattributed,
                updated_after_release: tally.updated_after_release,
                retired_rows: self.retired_rows.load(Ordering::Relaxed),
                counter_updates: self.dropped_counters.load(Ordering::Relaxed),
                evicted_events: self.evicted_events.load(Ordering::Relaxed),
            },
        }
    }

    fn counter_add_at(
        &self,
        desc: &'static CounterDesc,
        context: ContextHandle,
        value: u64,
        at_nanos: u64,
    ) {
        self.counter_update_at(desc, context, value, at_nanos, CounterKind::Counter);
    }

    fn gauge_set_at(
        &self,
        desc: &'static CounterDesc,
        context: ContextHandle,
        value: u64,
        at_nanos: u64,
    ) {
        self.counter_update_at(desc, context, value, at_nanos, CounterKind::Gauge);
    }

    fn histogram_record_at(
        &self,
        desc: &'static CounterDesc,
        context: ContextHandle,
        value: u64,
        at_nanos: u64,
    ) {
        let context_id = context.id();
        let key = HistogramKey {
            desc_id: desc.id(),
            context_id,
        };
        let shard_index = self.thread_shard_index();
        {
            let mut shard = self.lock_counter_shard(shard_index);
            if let Some(histogram) = shard.histograms.get_mut(&key) {
                histogram.record(value, at_nanos);
                return;
            }
        }

        // Admission takes other locks, so it runs with the shard released.
        if !self.admit_key(AdmittedKey::Histogram(key)) {
            self.dropped_counters.fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.lock_counter_shard(shard_index)
            .histograms
            .entry(key)
            .or_insert_with(|| {
                HistogramSnapshot::new(desc.id(), desc.name(), desc.unit(), context_id)
            })
            .record(value, at_nanos);
    }

    fn counter_update_at(
        &self,
        desc: &'static CounterDesc,
        context: ContextHandle,
        value: u64,
        at_nanos: u64,
        kind: CounterKind,
    ) {
        let context_id = context.id();
        let key = CounterKey::new(desc.id(), context_id, kind);
        let shard_index = self.thread_shard_index();
        {
            let mut shard = self.lock_counter_shard(shard_index);
            if let Some(snapshot) = shard.counters.get_mut(&key) {
                snapshot.apply(value, at_nanos);
                return;
            }
        }

        // Admission takes other locks, so it runs with the shard released.
        if !self.admit_key(AdmittedKey::Counter(key)) {
            self.dropped_counters.fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.lock_counter_shard(shard_index)
            .counters
            .entry(key)
            .or_insert_with(|| CounterSnapshot {
                desc_id: desc.id(),
                name: desc.name(),
                unit: desc.unit(),
                context_id,
                kind,
                value: match kind {
                    CounterKind::Counter => 0,
                    CounterKind::Gauge => value,
                },
                updated_at_nanos: at_nanos,
            })
            .apply(value, at_nanos);
    }

    fn lock_counter_shard(&self, index: usize) -> std::sync::MutexGuard<'_, CounterShard> {
        self.counter_shards[index]
            .lock()
            .expect("runtime counter shard should not be poisoned")
    }

    fn next_scope_token(&self) -> ScopeToken {
        ScopeToken::new(self.next_scope_token.fetch_add(1, Ordering::Relaxed))
    }

    fn push_event_at(&self, observation: Observation, at_nanos: u64) {
        let shard_index = self.thread_shard_index();
        let mut shard = self.event_shards[shard_index]
            .lock()
            .expect("runtime event shard should not be poisoned");
        if !reserve_slot(&self.event_count, self.event_capacity) {
            // Full: take the slot of the oldest event this shard holds, or of
            // one in another shard when this one is empty.
            if shard.events.pop_front().is_none() {
                drop(shard);
                if !self.evict_from_any_shard() {
                    // Nothing to evict: the capacity is zero.
                    self.evicted_events.fetch_add(1, Ordering::Relaxed);
                    return;
                }
                shard = self.event_shards[shard_index]
                    .lock()
                    .expect("runtime event shard should not be poisoned");
            }
            self.evicted_events.fetch_add(1, Ordering::Relaxed);
        }

        // Allocating the sequence number under the shard lock keeps it in
        // program order for the recording thread, which timestamps alone
        // cannot guarantee when the clock ticks coarsely, and lets readers use
        // the next number as a watermark; see `next_event_sequence`.
        let sequence = self.next_event_sequence.fetch_add(1, Ordering::SeqCst);
        shard.events.push_back(TimedObservation {
            at_nanos,
            sequence,
            thread_id: current_thread_id(),
            observation,
        });
    }

    /// Records a frame, transaction, or pass marker, and at a frame end, the
    /// counters the frame updated.
    fn marker(
        &self,
        kind: MarkerKind,
        phase: MarkerPhase,
        number: Option<FrameNumber>,
        context: ContextHandle,
    ) {
        if !self.level().captures_markers() {
            return;
        }
        let at_nanos = self.now_nanos();
        self.push_event_at(
            Observation::FrameMarker {
                kind,
                phase,
                number,
                context_id: context.id(),
            },
            at_nanos,
        );
        if self.counter_samples
            && phase == MarkerPhase::End
            && marker_index(kind).is_some_and(|i| self.counter_sample_markers[i])
        {
            self.sample_counters(at_nanos, false);
        }
    }

    /// Records a sample of every counter and gauge updated since the previous
    /// frame end, with its value merged across threads.
    fn sample_counters(&self, at_nanos: u64, all: bool) -> u64 {
        let since = self
            .last_counter_sample_nanos
            .swap(at_nanos, Ordering::Relaxed);
        let mut changed = HashSet::new();
        let mut merged: HashMap<CounterKey, CounterSnapshot> = HashMap::new();
        for shard in &self.counter_shards {
            let shard = shard
                .lock()
                .expect("runtime counter shard should not be poisoned");
            for (key, counter) in &shard.counters {
                if all || counter.updated_at_nanos >= since {
                    changed.insert(*key);
                }
                match merged.get_mut(key) {
                    Some(existing) => existing.merge(counter),
                    None => {
                        merged.insert(*key, counter.clone());
                    }
                }
            }
        }
        let mut samples: Vec<_> = changed
            .into_iter()
            .filter_map(|key| merged.remove(&key))
            .collect();
        // Shard maps iterate in arbitrary order; keep samples deterministic.
        samples.sort_by_key(|counter| {
            (
                counter.name,
                counter.context_id,
                counter_kind_discriminant(counter.kind),
            )
        });
        let count = samples.len() as u64;
        for counter in samples {
            self.push_event_at(
                Observation::CounterSample {
                    desc_id: counter.desc_id,
                    name: counter.name,
                    unit: counter.unit,
                    context_id: counter.context_id,
                    kind: counter.kind,
                    value: counter.value,
                },
                at_nanos,
            );
        }
        count
    }

    /// Evicts the oldest event of the first non-empty shard, keeping its
    /// capacity slot for the caller. Returns `false` when every shard is
    /// empty.
    fn evict_from_any_shard(&self) -> bool {
        self.event_shards.iter().any(|shard| {
            shard
                .lock()
                .expect("runtime event shard should not be poisoned")
                .events
                .pop_front()
                .is_some()
        })
    }

    /// Admits a key the calling thread's shard has not seen yet.
    ///
    /// A key another thread already admitted is admitted again for free, so
    /// a key costs one slot of `counter_capacity` however many threads
    /// record it. When the capacity is reached, keys of retired contexts
    /// make room first.
    fn admit_key(&self, key: AdmittedKey) -> bool {
        // A handle the runtime does not know is reported under the root
        // context, so it takes the root context's key, not one of its own.
        let key = if self
            .contexts
            .lock()
            .expect("runtime context mutex should not be poisoned")
            .knows(key.context_id())
        {
            key
        } else {
            key.at_root()
        };
        {
            let mut admitted = self
                .admitted_keys
                .lock()
                .expect("runtime admitted-key mutex should not be poisoned");
            if admitted.contains(&key) {
                return true;
            }
            if admitted.len() < self.counter_capacity {
                admitted.insert(key);
                return true;
            }
        }
        let mut contexts = self
            .contexts
            .lock()
            .expect("runtime context mutex should not be poisoned");
        if self.retire_released_contexts(&mut contexts) == 0 {
            return false;
        }
        drop(contexts);
        let mut admitted = self
            .admitted_keys
            .lock()
            .expect("runtime admitted-key mutex should not be poisoned");
        if admitted.contains(&key) {
            return true;
        }
        if admitted.len() < self.counter_capacity {
            admitted.insert(key);
            return true;
        }
        false
    }

    /// Retires released contexts that no retained event can refer to,
    /// removing their counter rows and admitted keys, and returns how many
    /// it retired.
    ///
    /// Lock order: the context table, then each event shard in turn, then
    /// the admitted keys, then each counter shard in turn.
    fn retire_released_contexts(&self, contexts: &mut ContextTable) -> usize {
        if !contexts.has_released() {
            return 0;
        }
        // Every event recorded before this sequence number has been evicted.
        let horizon = self
            .event_shards
            .iter()
            .filter_map(|shard| {
                shard
                    .lock()
                    .expect("runtime event shard should not be poisoned")
                    .events
                    .front()
                    .map(|event| event.sequence)
            })
            .min()
            .unwrap_or_else(|| self.next_event_sequence.load(Ordering::SeqCst));
        let retired = contexts.retire_released_before(horizon);
        if retired.is_empty() {
            return 0;
        }

        let is_retired = |context_id: ContextId| retired.contains(&context_id.get());
        self.admitted_keys
            .lock()
            .expect("runtime admitted-key mutex should not be poisoned")
            .retain(|key| !is_retired(key.context_id()));
        let mut retired_rows = 0_usize;
        for shard in &self.counter_shards {
            let mut shard = shard
                .lock()
                .expect("runtime counter shard should not be poisoned");
            let before = shard.counters.len() + shard.histograms.len();
            shard.counters.retain(|key, _| !is_retired(key.context_id));
            shard
                .histograms
                .retain(|key, _| !is_retired(key.context_id));
            retired_rows += before - (shard.counters.len() + shard.histograms.len());
        }
        // Rows are counted per shard, so a key recorded from several threads
        // counts once per thread.
        self.retired_rows.fetch_add(
            u64::try_from(retired_rows).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        retired.len()
    }

    /// Picks the shard owned by the calling thread.
    ///
    /// Counters and events live in per-thread shards so threads recording
    /// the same key do not contend; snapshots merge the shards.
    fn thread_shard_index(&self) -> usize {
        let shards = self
            .counter_shards
            .len()
            .min(self.event_shards.len())
            .max(1);
        usize::try_from(current_thread_id()).unwrap_or(0) % shards
    }
}

/// Configures a [`Runtime`]; see [`Runtime::builder`].
///
/// Capacities bound memory: see [`Runtime`] for what happens when each one
/// is reached.
#[derive(Clone, Debug)]
#[must_use = "a runtime builder does nothing until `build` is called"]
pub struct RuntimeBuilder {
    process_name: Option<String>,
    session_name: Option<String>,
    level: InstrumentationLevel,
    counter_capacity: usize,
    event_capacity: usize,
    context_capacity: usize,
    counter_samples: bool,
    counter_sample_markers: [bool; 3],
}

fn marker_index(kind: MarkerKind) -> Option<usize> {
    match kind {
        MarkerKind::Frame => Some(0),
        MarkerKind::Transaction => Some(1),
        MarkerKind::Pass => Some(2),
        _ => None,
    }
}

impl Default for RuntimeBuilder {
    fn default() -> Self {
        Self {
            process_name: None,
            session_name: None,
            level: InstrumentationLevel::Trace,
            counter_capacity: Self::DEFAULT_COUNTER_CAPACITY,
            event_capacity: Self::DEFAULT_EVENT_CAPACITY,
            context_capacity: Self::DEFAULT_CONTEXT_CAPACITY,
            counter_samples: true,
            counter_sample_markers: [true, false, false],
        }
    }
}

impl RuntimeBuilder {
    /// Default number of counter, gauge, and histogram keys.
    pub const DEFAULT_COUNTER_CAPACITY: usize = 1024;
    /// Default number of retained events.
    pub const DEFAULT_EVENT_CAPACITY: usize = 65_536;
    /// Default number of registered contexts.
    pub const DEFAULT_CONTEXT_CAPACITY: usize = 4096;

    /// Sets the process name reported in snapshots and exports.
    ///
    /// Defaults to the file stem of the running executable.
    pub fn process_name(mut self, process_name: impl Into<String>) -> Self {
        self.process_name = Some(process_name.into());
        self
    }

    /// Sets the capture-session label reported in snapshots; see
    /// [`Runtime::set_session_name`]. Defaults to none.
    pub fn session_name(mut self, session_name: impl Into<String>) -> Self {
        self.session_name = Some(session_name.into());
        self
    }

    /// Sets the capture level; see [`Runtime::set_level`]. Defaults to
    /// [`InstrumentationLevel::Trace`], which captures everything.
    pub const fn level(mut self, level: InstrumentationLevel) -> Self {
        self.level = level;
        self
    }

    /// Sets how many counter keys the runtime keeps, where a key is one
    /// counter, gauge, or histogram descriptor under one context. Defaults to
    /// [`DEFAULT_COUNTER_CAPACITY`](Self::DEFAULT_COUNTER_CAPACITY).
    ///
    /// Size it for descriptors times the contexts they are recorded under.
    pub const fn counter_capacity(mut self, capacity: usize) -> Self {
        self.counter_capacity = capacity;
        self
    }

    /// Sets how many events the runtime retains. Defaults to
    /// [`DEFAULT_EVENT_CAPACITY`](Self::DEFAULT_EVENT_CAPACITY).
    ///
    /// Each scope takes two events, a begin and an end, and each marker,
    /// flow step, message, and resource change takes one.
    pub const fn event_capacity(mut self, capacity: usize) -> Self {
        self.event_capacity = capacity;
        self
    }

    /// Sets how many registered contexts the runtime keeps, live and
    /// released. Defaults to
    /// [`DEFAULT_CONTEXT_CAPACITY`](Self::DEFAULT_CONTEXT_CAPACITY).
    pub const fn context_capacity(mut self, capacity: usize) -> Self {
        self.context_capacity = capacity;
        self
    }

    /// Enables counter/gauge sampling, both explicit checkpoints and the
    /// automatic marker triggers selected by [`Self::counter_samples_on`].
    /// Defaults to `true`, with frame ends as the only automatic trigger.
    /// Each automatic marker end records a
    /// [`CounterSample`](Observation::CounterSample) for every counter and
    /// gauge updated since the previous sampling call.
    ///
    /// Samples give per-frame counter values without registering a context
    /// per frame, and a time series for each counter. They are events, so
    /// they take event capacity, one per updated counter per frame, and are
    /// recorded only at levels that capture markers. Explicit checkpoints
    /// capture all counter/gauge rows, including unchanged values, at levels
    /// that capture counters. Sampling locks and scans all counter shards;
    /// it is not an atomic process-wide snapshot. Disable it here to avoid
    /// both the scans and their events.
    pub const fn counter_samples(mut self, enabled: bool) -> Self {
        self.counter_samples = enabled;
        self
    }

    /// Enables automatic sampling at the end of `kind`. Frames are enabled
    /// by default; transactions and passes are disabled. Explicit checkpoints
    /// are independent of these triggers. [`counter_samples(false)`](Self::counter_samples)
    /// disables both automatic and explicit sampling.
    ///
    /// Automatic sampling scans all counter shards and emits one event per
    /// changed counter/gauge. Changes are since the previous sample, not
    /// exclusively attributable to this marker when work overlaps.
    pub fn counter_samples_on(mut self, kind: MarkerKind, enabled: bool) -> Self {
        if let Some(index) = marker_index(kind) {
            self.counter_sample_markers[index] = enabled;
        }
        self
    }

    /// Builds the runtime.
    #[must_use]
    pub fn build(self) -> Runtime {
        Runtime::from_builder(self)
    }

    /// Builds the runtime and installs it as the process-wide backend; see
    /// [`Runtime::install_global`].
    pub fn install_global(self) -> Result<&'static Runtime, InstallBackendError> {
        self.build().install_global()
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct HistogramKey {
    desc_id: CounterDescId,
    context_id: ContextId,
}

/// Labels and descriptor ids snapshots share.
#[derive(Debug)]
struct LabelState {
    session_name: Option<String>,
    descriptor_ids: DescriptorIds,
}

/// Maps descriptor addresses to dense snapshot ids.
///
/// Descriptor identity inside the process is the descriptor's address, which
/// must never reach a snapshot consumer: it would reveal the process memory
/// layout to anything that reads a snapshot. Snapshots therefore carry
/// dense ids from one id space shared by every descriptor kind (scope,
/// counter, event, and resource kind), assigned from `1` in first-seen order:
/// events in snapshot order, then counters, then histograms. Assignments stay
/// fixed for the life of the runtime, so every snapshot agrees.
#[derive(Clone, Debug, Default)]
struct DescriptorIds {
    dense: HashMap<u64, u64>,
}

impl DescriptorIds {
    fn dense(&mut self, address_id: u64) -> u64 {
        let next = u64::try_from(self.dense.len())
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        *self.dense.entry(address_id).or_insert(next)
    }

    fn remap(
        &mut self,
        events: &mut [TimedObservation],
        counters: &mut [CounterSnapshot],
        histograms: &mut [HistogramSnapshot],
    ) {
        for event in events {
            match &mut event.observation {
                Observation::ScopeBegin { desc_id, .. } => {
                    *desc_id = ScopeDescId::new(self.dense(desc_id.get()));
                }
                Observation::Message { desc_id, .. }
                | Observation::CounterCheckpoint { desc_id, .. } => {
                    *desc_id = EventDescId::new(self.dense(desc_id.get()));
                }
                Observation::CounterSample { desc_id, .. } => {
                    *desc_id = CounterDescId::new(self.dense(desc_id.get()));
                }
                Observation::ResourceCreate { kind_id, .. }
                | Observation::ResourceUpdate { kind_id, .. }
                | Observation::ResourceDrop { kind_id, .. } => {
                    *kind_id = ResourceKindId::new(self.dense(kind_id.get()));
                }
                Observation::ClockSync { clock } => {
                    clock.clock_id = ClockId::new(self.dense(clock.clock_id.get()));
                }
                Observation::ClockPair { first, second } => {
                    first.clock_id = ClockId::new(self.dense(first.clock_id.get()));
                    second.clock_id = ClockId::new(self.dense(second.clock_id.get()));
                }
                Observation::FramePoint {
                    time: Some(time), ..
                }
                | Observation::FrameOpportunity { time, .. } => {
                    time.clock_id = ClockId::new(self.dense(time.clock_id.get()));
                }
                Observation::TimelineSpan { span } => {
                    span.timeline_id = TimelineId::new(self.dense(span.timeline_id.get()));
                    span.desc_id = ScopeDescId::new(self.dense(span.desc_id.get()));
                    span.begin.clock_id = ClockId::new(self.dense(span.begin.clock_id.get()));
                }
                Observation::ScopeEnd { .. }
                | Observation::FrameMarker { .. }
                | Observation::FramePoint { time: None, .. }
                | Observation::FramePlan { .. }
                | Observation::Flow { .. } => {}
            }
        }
        for counter in counters {
            counter.desc_id = CounterDescId::new(self.dense(counter.desc_id.get()));
        }
        for histogram in histograms {
            histogram.desc_id = CounterDescId::new(self.dense(histogram.desc_id.get()));
        }
    }
}

/// Registered contexts, keyed by handle.
///
/// Only registration, release, and snapshots touch the table; emissions carry
/// a handle and never look it up. Released contexts stay until the runtime
/// retires them; see [`Runtime`].
#[derive(Debug)]
struct ContextTable {
    capacity: usize,
    entries: HashMap<u64, RegisteredEntry>,
    /// Registrations refused because the table was full of live contexts.
    refused: u64,
    /// Number of released entries still in the table.
    released: usize,
}

#[derive(Debug)]
struct RegisteredEntry {
    context: Context,
    /// When the context was released, or `None` while live.
    released_at: Option<Release>,
}

/// When a context was released, in event sequence and in time.
#[derive(Clone, Copy, Debug)]
struct Release {
    sequence: u64,
    nanos: u64,
}

/// Attribution problems found while resolving one snapshot.
#[derive(Debug, Default)]
struct ResolveTally {
    unattributed: u64,
    updated_after_release: u64,
}

impl ContextTable {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: HashMap::new(),
            refused: 0,
            released: 0,
        }
    }

    /// Returns `true` for the root context and every context in the table.
    fn knows(&self, context_id: ContextId) -> bool {
        context_id.get() == 0 || self.entries.contains_key(&context_id.get())
    }

    fn is_full(&self) -> bool {
        self.entries.len() >= self.capacity
    }

    const fn has_released(&self) -> bool {
        self.released > 0
    }

    fn register(&mut self, handle: ContextHandle, context: &Context) {
        if handle.is_root() {
            return;
        }
        if self.is_full() {
            self.refused = self.refused.saturating_add(1);
            return;
        }
        self.entries.insert(
            handle.get(),
            RegisteredEntry {
                context: *context,
                released_at: None,
            },
        );
    }

    fn release(&mut self, handle: ContextHandle, release: Release) {
        if let Some(entry) = self.entries.get_mut(&handle.get())
            && entry.released_at.is_none()
        {
            entry.released_at = Some(release);
            self.released += 1;
        }
    }

    /// Removes contexts released before event `horizon`, returning their
    /// handles.
    fn retire_released_before(&mut self, horizon: u64) -> HashSet<u64> {
        let mut retired = HashSet::new();
        self.entries
            .retain(|handle, entry| match entry.released_at {
                Some(released_at) if released_at.sequence <= horizon => {
                    retired.insert(*handle);
                    false
                }
                _ => true,
            });
        self.released -= retired.len();
        retired
    }

    /// Returns `context_id` when the table knows it, and otherwise the root
    /// context, counting the miss.
    /// Resolves the context of a counter or histogram row last updated at
    /// `updated_at_nanos`: the row keeps a context the table knows, counted
    /// when it was updated after the context's release, and otherwise goes
    /// to the root context, counted as unattributed.
    fn resolve_row(
        &self,
        context_id: ContextId,
        updated_at_nanos: u64,
        tally: &mut ResolveTally,
    ) -> ContextId {
        if context_id.get() == 0 {
            return context_id;
        }
        match self.entries.get(&context_id.get()) {
            Some(entry) => {
                if entry
                    .released_at
                    .is_some_and(|release| updated_at_nanos > release.nanos)
                {
                    tally.updated_after_release += 1;
                }
                context_id
            }
            None => {
                tally.unattributed += 1;
                ContextId::new(0)
            }
        }
    }

    /// Resolves the context of the event numbered `sequence`: an event
    /// recorded with a handle the table does not know, or after its context
    /// was released, goes to the root context, counted as unattributed.
    fn resolve_event(
        &self,
        context_id: ContextId,
        sequence: u64,
        tally: &mut ResolveTally,
    ) -> ContextId {
        if context_id.get() == 0 {
            return context_id;
        }
        match self.entries.get(&context_id.get()) {
            Some(entry)
                if entry
                    .released_at
                    .is_none_or(|release| sequence < release.sequence) =>
            {
                context_id
            }
            _ => {
                tally.unattributed += 1;
                ContextId::new(0)
            }
        }
    }

    fn snapshot(&self) -> Vec<ContextSnapshot> {
        let root = ContextSnapshot {
            id: ContextId::new(0),
            context: Context::root(),
            released: false,
        };
        let mut contexts: Vec<_> = core::iter::once(root)
            .chain(self.entries.iter().map(|(handle, entry)| ContextSnapshot {
                id: ContextId::new(*handle),
                context: entry.context,
                released: entry.released_at.is_some(),
            }))
            .collect();
        contexts.sort_by_key(|context| context.id);
        contexts
    }
}

#[derive(Debug, Default)]
struct CounterShard {
    counters: HashMap<CounterKey, CounterSnapshot>,
    histograms: HashMap<HistogramKey, HistogramSnapshot>,
}

#[derive(Debug, Default)]
struct EventShard {
    events: VecDeque<TimedObservation>,
}

/// A counter or histogram key, as admitted against the counter capacity.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum AdmittedKey {
    Counter(CounterKey),
    Histogram(HistogramKey),
}

impl AdmittedKey {
    const fn context_id(self) -> ContextId {
        match self {
            Self::Counter(key) => key.context_id,
            Self::Histogram(key) => key.context_id,
        }
    }

    const fn at_root(self) -> Self {
        let root = ContextId::new(0);
        match self {
            Self::Counter(key) => Self::Counter(CounterKey {
                context_id: root,
                ..key
            }),
            Self::Histogram(key) => Self::Histogram(HistogramKey {
                context_id: root,
                ..key
            }),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct CounterKey {
    desc_id: CounterDescId,
    context_id: ContextId,
    kind: CounterKind,
}

impl CounterKey {
    fn new(desc_id: CounterDescId, context_id: ContextId, kind: CounterKind) -> Self {
        Self {
            desc_id,
            context_id,
            kind,
        }
    }
}

impl Backend for Runtime {
    fn counter_checkpoint(&self, desc: &'static EventDesc, context: ContextHandle) {
        if !self.counter_samples || !self.level().captures_counters() {
            return;
        }
        let at = self.now_nanos();
        let samples = self.sample_counters(at, true);
        self.push_event_at(
            Observation::CounterCheckpoint {
                desc_id: desc.id(),
                name: desc.name(),
                context_id: context.id(),
                samples,
            },
            at,
        );
    }

    fn context_registered(&self, handle: ContextHandle, context: &Context) {
        let mut contexts = self
            .contexts
            .lock()
            .expect("runtime context mutex should not be poisoned");
        if contexts.is_full() {
            self.retire_released_contexts(&mut contexts);
        }
        contexts.register(handle, context);
    }

    fn context_released(&self, handle: ContextHandle) {
        // Events numbered from here on were recorded after the release, so
        // once everything before it is evicted no retained event needs the
        // context.
        let release = Release {
            sequence: self.next_event_sequence.load(Ordering::SeqCst),
            nanos: self.now_nanos(),
        };
        self.contexts
            .lock()
            .expect("runtime context mutex should not be poisoned")
            .release(handle, release);
    }

    fn counter_add(&self, desc: &'static CounterDesc, context: ContextHandle, value: u64) {
        if !self.level().captures_counters() {
            return;
        }
        let at_nanos = self.now_nanos();
        self.counter_add_at(desc, context, value, at_nanos);
    }

    fn gauge_set(&self, desc: &'static CounterDesc, context: ContextHandle, value: u64) {
        if !self.level().captures_counters() {
            return;
        }
        let at_nanos = self.now_nanos();
        self.gauge_set_at(desc, context, value, at_nanos);
    }

    fn histogram_record(&self, desc: &'static CounterDesc, context: ContextHandle, value: u64) {
        if !self.level().captures_counters() {
            return;
        }
        let at_nanos = self.now_nanos();
        self.histogram_record_at(desc, context, value, at_nanos);
    }

    fn scope_begin(
        &self,
        desc: &'static ScopeDesc,
        context: ContextHandle,
        args: &Args,
    ) -> ScopeToken {
        if !self.level().captures_scopes() {
            return ScopeToken::INVALID;
        }
        let at_nanos = self.now_nanos();
        let context_id = context.id();
        let token = self.next_scope_token();
        self.push_event_at(
            Observation::ScopeBegin {
                desc_id: desc.id(),
                name: desc.name(),
                location: desc.location(),
                context_id,
                token,
                args: *args,
            },
            at_nanos,
        );
        token
    }

    fn scope_end(&self, token: ScopeToken) {
        if !token.is_valid() {
            return;
        }
        // The level controls admission at scope_begin, not completion of an admitted scope.
        let at_nanos = self.now_nanos();
        self.push_event_at(Observation::ScopeEnd { token }, at_nanos);
    }

    fn resource_create(
        &self,
        desc: &'static ResourceKindDesc,
        resource_id: ResourceId,
        context: ContextHandle,
        size: u64,
    ) {
        if !self.level().captures_resources() {
            return;
        }
        let at_nanos = self.now_nanos();
        let context_id = context.id();
        self.push_event_at(
            Observation::ResourceCreate {
                kind_id: desc.id(),
                kind_name: desc.name(),
                resource_id,
                context_id,
                size,
            },
            at_nanos,
        );
    }

    fn resource_update(
        &self,
        desc: &'static ResourceKindDesc,
        resource_id: ResourceId,
        context: ContextHandle,
        size: u64,
    ) {
        if !self.level().captures_resources() {
            return;
        }
        let at_nanos = self.now_nanos();
        let context_id = context.id();
        self.push_event_at(
            Observation::ResourceUpdate {
                kind_id: desc.id(),
                kind_name: desc.name(),
                resource_id,
                context_id,
                size,
            },
            at_nanos,
        );
    }

    fn resource_drop(
        &self,
        desc: &'static ResourceKindDesc,
        resource_id: ResourceId,
        context: ContextHandle,
    ) {
        if !self.level().captures_resources() {
            return;
        }
        let at_nanos = self.now_nanos();
        let context_id = context.id();
        self.push_event_at(
            Observation::ResourceDrop {
                kind_id: desc.id(),
                kind_name: desc.name(),
                resource_id,
                context_id,
            },
            at_nanos,
        );
    }

    fn frame_marker(&self, kind: MarkerKind, phase: MarkerPhase, context: ContextHandle) {
        self.marker(kind, phase, None, context);
    }

    fn numbered_marker(
        &self,
        kind: MarkerKind,
        phase: MarkerPhase,
        number: FrameNumber,
        context: ContextHandle,
    ) {
        self.marker(kind, phase, Some(number), context);
    }

    fn clock_sync(&self, reading: ClockReading) {
        if !self.level().captures_markers() {
            return;
        }
        let at_nanos = self.now_nanos();
        self.push_event_at(
            Observation::ClockSync {
                clock: reading.into(),
            },
            at_nanos,
        );
    }

    fn clock_pair(&self, first: ClockReading, second: ClockReading) {
        if !self.level().captures_markers() {
            return;
        }
        let at_nanos = self.now_nanos();
        self.push_event_at(
            Observation::ClockPair {
                first: first.into(),
                second: second.into(),
            },
            at_nanos,
        );
    }

    fn frame_point(&self, point: &FramePoint) {
        if !self.level().captures_markers() {
            return;
        }
        let at_nanos = self.now_nanos();
        self.push_event_at(
            Observation::FramePoint {
                number: point.number(),
                kind: point.kind(),
                time: point.time().map(ClockTicks::from),
                basis: point.basis(),
                args: *point.args(),
            },
            at_nanos,
        );
    }

    fn frame_opportunity(&self, opportunity: &FrameOpportunity) {
        if !self.level().captures_markers() {
            return;
        }
        let at_nanos = self.now_nanos();
        self.push_event_at(
            Observation::FrameOpportunity {
                stream: opportunity.stream(),
                time: ClockTicks::from(opportunity.time()),
                interval: opportunity.interval(),
                args: *opportunity.args(),
            },
            at_nanos,
        );
    }

    fn frame_plan(&self, plan: &FramePlan) {
        if !self.level().captures_markers() {
            return;
        }
        let at_nanos = self.now_nanos();
        self.push_event_at(
            Observation::FramePlan {
                number: plan.number(),
                period: plan.period(),
                interval: plan.interval(),
                depth: plan.depth(),
                latency_allowance: plan.latency_allowance(),
                hold: plan.hold(),
                kind: plan.kind(),
                args: *plan.args(),
            },
            at_nanos,
        );
    }

    fn timeline_span(&self, span: &TimelineSpan) {
        if !self.level().captures_markers() {
            return;
        }
        let at_nanos = self.now_nanos();
        self.push_event_at(
            Observation::TimelineSpan {
                span: Box::new(TimelineWork::new(span)),
            },
            at_nanos,
        );
    }

    fn flow(&self, flow_id: FlowId, phase: FlowPhase, context: ContextHandle) {
        if !self.level().captures_flows() {
            return;
        }
        let at_nanos = self.now_nanos();
        let context_id = context.id();
        self.push_event_at(
            Observation::Flow {
                flow_id,
                phase,
                context_id,
            },
            at_nanos,
        );
    }

    fn message(&self, desc: &'static EventDesc, context: ContextHandle, text: &str, args: &Args) {
        if !self.level().captures_messages() {
            return;
        }
        let at_nanos = self.now_nanos();
        let context_id = context.id();
        self.push_event_at(
            Observation::Message {
                desc_id: desc.id(),
                name: desc.name(),
                context_id,
                text: truncate_message_text(text),
                args: *args,
            },
            at_nanos,
        );
    }
}

fn truncate_message_text(text: &str) -> String {
    if text.len() <= MESSAGE_TEXT_MAX_BYTES {
        return String::from(text);
    }
    let mut end = MESSAGE_TEXT_MAX_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    String::from(&text[..end])
}

static NEXT_THREAD_ID: AtomicU64 = AtomicU64::new(1);

/// Most thread names the registry keeps, so processes that spawn threads
/// without end cannot grow it without bound; later threads go unnamed.
const MAX_THREAD_NAMES: usize = 4096;

/// Operating-system names of the threads that have recorded, by runtime id.
static THREAD_NAMES: Mutex<BTreeMap<u64, String>> = Mutex::new(BTreeMap::new());

std::thread_local! {
    static THREAD_ID: u64 = {
        let id = NEXT_THREAD_ID.fetch_add(1, Ordering::Relaxed);
        if let Some(name) = std::thread::current().name()
            && let Ok(mut names) = THREAD_NAMES.lock()
            && names.len() < MAX_THREAD_NAMES
        {
            names.insert(id, String::from(name));
        }
        id
    };
}

/// Returns a row for each thread that recorded one of `events`.
fn thread_snapshots(events: &[TimedObservation]) -> Vec<ThreadSnapshot> {
    let ids: std::collections::BTreeSet<u64> = events.iter().map(|event| event.thread_id).collect();
    let names = THREAD_NAMES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    ids.into_iter()
        .map(|id| ThreadSnapshot::new(id, names.get(&id).cloned()))
        .collect()
}

/// Returns the runtime-assigned id for the calling thread.
///
/// Ids are assigned in first-use order starting at `1` and remain stable for
/// the lifetime of the thread.
#[must_use]
pub fn current_thread_id() -> u64 {
    THREAD_ID.with(|id| *id)
}

fn build_counter_shards(count: usize) -> Vec<Mutex<CounterShard>> {
    (0..count.max(1))
        .map(|_| Mutex::new(CounterShard::default()))
        .collect()
}

fn build_event_shards(count: usize) -> Vec<Mutex<EventShard>> {
    (0..count.max(1))
        .map(|_| Mutex::new(EventShard::default()))
        .collect()
}

/// `footfall_runtime`'s version, reported as the producer of every snapshot.
const PRODUCER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Returns the operating-system process id, or `0` where there is none.
fn current_process_id() -> u32 {
    // `std::process::id` panics on WASI, which has no processes.
    if cfg!(target_os = "wasi") {
        0
    } else {
        std::process::id()
    }
}

/// Returns a random, nonzero runtime instance id.
fn new_instance_id(wall_clock: Duration) -> u64 {
    use std::hash::BuildHasher as _;
    // `RandomState` is seeded from the operating system's randomness where
    // there is one; the time and a per-process counter keep ids distinct
    // where there is not, and between runtimes created in the same instant.
    static CREATED: AtomicU64 = AtomicU64::new(0);
    std::hash::RandomState::new()
        .hash_one((wall_clock, CREATED.fetch_add(1, Ordering::Relaxed)))
        .max(1)
}

/// Names the process after the running executable, falling back to
/// `"footfall"` when the executable path is unavailable.
fn default_process_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .map(String::from)
        })
        .unwrap_or_else(|| String::from("footfall"))
}

fn reserve_slot(count: &AtomicUsize, capacity: usize) -> bool {
    let previous = count.fetch_add(1, Ordering::Relaxed);
    if previous < capacity {
        true
    } else {
        count.fetch_sub(1, Ordering::Relaxed);
        false
    }
}

const fn counter_kind_discriminant(kind: CounterKind) -> u64 {
    match kind {
        CounterKind::Counter => 0,
        CounterKind::Gauge => 1,
    }
}

fn duration_to_nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

const fn level_to_u8(level: InstrumentationLevel) -> u8 {
    match level {
        InstrumentationLevel::Off => 0,
        InstrumentationLevel::Counters => 1,
        InstrumentationLevel::Scoped => 2,
        // `Trace` and any future level capture everything this runtime records.
        _ => 3,
    }
}

const fn level_from_u8(level: u8) -> InstrumentationLevel {
    match level {
        0 => InstrumentationLevel::Off,
        1 => InstrumentationLevel::Counters,
        2 => InstrumentationLevel::Scoped,
        _ => InstrumentationLevel::Trace,
    }
}

#[cfg(test)]
mod tests {
    use super::{CaptureSettings, CounterKind, EventCursor, EventWindow, Observation, Runtime};
    use footfall_core::{
        Args, Backend as _, Context, ContextHandle, ContextId, CounterDesc, FlowId, FlowPhase,
        InstrumentationLevel, LabelKey, MarkerKind, MarkerPhase, ResourceId, ResourceKindDesc,
        ScopeDesc, ScopeToken, SourceLocation, Unit,
    };
    use std::{string::String, vec::Vec};

    static FRAME: LabelKey = LabelKey::new("frame");
    static PASS: LabelKey = LabelKey::new("pass");

    /// Registers `context` with `runtime` alone, as the process backend
    /// would on `register_context`.
    fn test_runtime(counter_capacity: usize, event_capacity: usize) -> Runtime {
        Runtime::builder()
            .counter_capacity(counter_capacity)
            .event_capacity(event_capacity)
            .build()
    }

    fn register(runtime: &Runtime, context: &Context) -> ContextHandle {
        let handle = footfall_core::register_context(context);
        runtime.context_registered(handle, context);
        handle
    }
    static NODE: LabelKey = LabelKey::new("node");

    #[test]
    fn messages_are_captured_with_truncated_text() {
        let runtime = test_runtime(8, 8);
        static CHANNEL: footfall_core::EventDesc = footfall_core::EventDesc::new("app_log");
        let context_id = ContextId::new(0);

        let long_text = "x".repeat(super::MESSAGE_TEXT_MAX_BYTES + 64);
        runtime.push_event_at(
            Observation::Message {
                desc_id: CHANNEL.id(),
                name: CHANNEL.name(),
                context_id,
                text: super::truncate_message_text(&long_text),
                args: Args::none().with("level", "warn"),
            },
            5,
        );

        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.events.len(), 1);
        let Observation::Message { text, args, .. } = &snapshot.events[0].observation else {
            panic!("expected a message observation");
        };
        assert_eq!(text.len(), super::MESSAGE_TEXT_MAX_BYTES);
        assert_eq!(
            args.get("level").and_then(|value| value.as_static_str()),
            Some("warn")
        );
    }

    #[test]
    fn message_truncation_respects_char_boundaries() {
        let text = "é".repeat(super::MESSAGE_TEXT_MAX_BYTES);
        let truncated = super::truncate_message_text(&text);
        assert!(truncated.len() <= super::MESSAGE_TEXT_MAX_BYTES);
        assert!(truncated.chars().all(|ch| ch == 'é'));
        assert_eq!(super::truncate_message_text("short"), "short");
    }

    #[test]
    fn histograms_aggregate_bucketed_distributions() {
        let runtime = test_runtime(8, 8);
        static LATENCY: CounterDesc = CounterDesc::new("frame_micros", Unit::Micros);
        let context = register(&runtime, &Context::root().with(&PASS, 1_u32));

        runtime.histogram_record_at(&LATENCY, context, 0, 1);
        runtime.histogram_record_at(&LATENCY, context, 1, 2);
        runtime.histogram_record_at(&LATENCY, context, 3, 3);
        runtime.histogram_record_at(&LATENCY, context, 1000, 4);

        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.histograms.len(), 1);
        let histogram = &snapshot.histograms[0];
        assert_eq!(histogram.name, "frame_micros");
        assert_eq!(histogram.unit, Unit::Micros);
        assert_eq!(histogram.count, 4);
        assert_eq!(histogram.sum, 1004);
        assert_eq!(histogram.min, 0);
        assert_eq!(histogram.max, 1000);
        assert_eq!(histogram.buckets[crate::histogram_bucket_index(0)], 1);
        assert_eq!(histogram.buckets[crate::histogram_bucket_index(1)], 1);
        assert_eq!(histogram.buckets[crate::histogram_bucket_index(3)], 1);
        assert_eq!(histogram.buckets[crate::histogram_bucket_index(1000)], 1);
        assert_eq!(histogram.updated_at_nanos, 4);

        assert_eq!(
            runtime.snapshot().histograms,
            snapshot.histograms,
            "histograms are cumulative, so reading does not clear them"
        );
    }

    #[test]
    fn histogram_bucket_indices_follow_bit_length() {
        assert_eq!(crate::histogram_bucket_index(0), 0);
        assert_eq!(crate::histogram_bucket_index(1), 1);
        assert_eq!(crate::histogram_bucket_index(2), 2);
        assert_eq!(crate::histogram_bucket_index(3), 2);
        assert_eq!(crate::histogram_bucket_index(4), 3);
        assert_eq!(crate::histogram_bucket_index(u64::MAX), 63);
    }

    #[test]
    #[cfg_attr(target_os = "wasi", ignore = "wasm32-wasip1 cannot spawn threads")]
    fn parallel_counter_writes_aggregate_without_loss() {
        const THREADS: u64 = 8;
        const UPDATES: u64 = 1000;

        let runtime = test_runtime(64, 64);
        static OPS: CounterDesc = CounterDesc::count("ops");
        let shared = register(&runtime, &Context::root().with(&PASS, 1_u32));
        std::thread::scope(|scope| {
            for thread in 0..THREADS {
                let runtime = &runtime;

                scope.spawn(move || {
                    let distinct = register(
                        runtime,
                        &Context::root()
                            .with(&NODE, u32::try_from(thread).expect("thread index fits")),
                    );
                    for update in 0..UPDATES {
                        runtime.counter_add_at(&OPS, shared, 1, update);
                        runtime.counter_add_at(&OPS, distinct, 1, update);
                    }
                });
            }
        });

        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.drops.counter_updates(), 0);
        assert_eq!(snapshot.drops.unattributed(), 0);
        let total: u64 = snapshot.counters.iter().map(|counter| counter.value).sum();
        assert_eq!(total, THREADS * UPDATES * 2);
        // One shared row plus one row per thread.
        assert_eq!(snapshot.counters.len() as u64, THREADS + 1);
    }

    #[test]
    #[cfg_attr(target_os = "wasi", ignore = "wasm32-wasip1 cannot spawn threads")]
    fn parallel_event_writes_respect_capacity_and_account_drops() {
        const THREADS: u64 = 8;
        const EVENTS: u64 = 200;
        const CAPACITY: usize = 128;

        let runtime = test_runtime(16, CAPACITY);
        let context_id = ContextId::new(0);
        std::thread::scope(|scope| {
            for _ in 0..THREADS {
                let runtime = &runtime;
                scope.spawn(move || {
                    for event in 0..EVENTS {
                        runtime.push_event_at(
                            Observation::FrameMarker {
                                kind: MarkerKind::Frame,
                                phase: MarkerPhase::Begin,
                                number: None,
                                context_id,
                            },
                            event,
                        );
                    }
                });
            }
        });

        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.events.len(), CAPACITY);
        assert_eq!(
            snapshot.events.len() as u64 + snapshot.drops.evicted_events(),
            THREADS * EVENTS
        );
        assert_eq!(snapshot.window.next(), THREADS * EVENTS);
        assert_eq!(
            snapshot.window.missed(),
            snapshot.drops.evicted_events(),
            "a full read misses exactly what was evicted"
        );
    }

    #[test]
    fn context_table_exhaustion_is_accounted() {
        let runtime = Runtime::builder().context_capacity(5).build();
        static OPS: CounterDesc = CounterDesc::count("ops");
        let handles: Vec<_> = (0..32_u32)
            .map(|node| register(&runtime, &Context::root().with(&NODE, node)))
            .collect();

        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.contexts.len(), 1 + 5, "root plus a full table");
        assert_eq!(snapshot.drops.refused_contexts(), 27);
        assert_eq!(snapshot.drops.unattributed(), 0);

        // Emitting with a refused handle attributes the update to the root
        // context and counts it.
        runtime.counter_add_at(&OPS, handles[31], 1, 1);
        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.counters.len(), 1);
        assert_eq!(snapshot.counters[0].context_id, ContextId::new(0));
        assert_eq!(snapshot.drops.refused_contexts(), 27);
        assert_eq!(snapshot.drops.unattributed(), 1);
    }

    #[test]
    fn released_contexts_retire_once_their_events_are_evicted() {
        let runtime = Runtime::builder()
            .counter_capacity(8)
            .event_capacity(2)
            .context_capacity(2)
            .build();
        static OPS: CounterDesc = CounterDesc::count("ops");
        let marker = |context_id| Observation::FrameMarker {
            kind: MarkerKind::Frame,
            phase: MarkerPhase::Begin,
            number: None,
            context_id,
        };
        let live = register(&runtime, &Context::root().with(&PASS, 1_u32));
        let released = register(&runtime, &Context::root().with(&PASS, 2_u32));
        runtime.push_event_at(marker(released.id()), 1);
        runtime.counter_add_at(&OPS, released, 1, 1);
        runtime.context_released(released);

        let snapshot = runtime.snapshot();
        assert!(
            snapshot.context(released.id()).is_some(),
            "observations recorded before release still resolve"
        );
        assert_eq!(snapshot.counters[0].context_id, released.id());

        // The table is full and an event still refers to the released
        // context, so a new registration is refused rather than retiring it.
        let refused = register(&runtime, &Context::root().with(&PASS, 3_u32));
        assert_eq!(runtime.snapshot().drops.refused_contexts(), 1);

        // Once that event is evicted, the released context and its counter
        // row make room.
        runtime.push_event_at(marker(live.id()), 2);
        runtime.push_event_at(marker(live.id()), 3);
        let fresh = register(&runtime, &Context::root().with(&PASS, 4_u32));
        let snapshot = runtime.snapshot();
        assert!(snapshot.context(released.id()).is_none());
        assert!(snapshot.context(live.id()).is_some(), "live contexts stay");
        assert!(snapshot.context(fresh.id()).is_some());
        assert!(snapshot.context(refused.id()).is_none());
        assert!(snapshot.counters.is_empty(), "the counter row retires too");
        assert_eq!(snapshot.drops.unattributed(), 0);
    }

    #[test]
    fn a_handle_used_after_release_is_counted_not_silently_misattributed() {
        let runtime = test_runtime(8, 16);
        static OPS: CounterDesc = CounterDesc::count("ops");
        let marker = |context_id| Observation::FrameMarker {
            kind: MarkerKind::Frame,
            phase: MarkerPhase::Begin,
            number: None,
            context_id,
        };
        let owner = register(&runtime, &Context::root().with(&PASS, 1_u32));
        runtime.push_event_at(marker(owner.id()), 1);
        runtime.counter_add_at(&OPS, owner, 1, 1);
        runtime.context_released(owner);
        std::thread::sleep(std::time::Duration::from_millis(1));

        // The owner is gone, but its handle is used again.
        let late = runtime.now_nanos();
        runtime.push_event_at(marker(owner.id()), late);
        runtime.counter_add_at(&OPS, owner, 10, late);

        let snapshot = runtime.snapshot();
        let contexts: Vec<_> = snapshot
            .events
            .iter()
            .filter_map(|event| event.observation.context_id())
            .collect();
        assert_eq!(
            contexts,
            [owner.id(), ContextId::new(0)],
            "the event before release keeps its context, the one after goes to root"
        );
        assert_eq!(snapshot.drops.unattributed(), 1);
        assert_eq!(snapshot.counters[0].context_id, owner.id());
        assert_eq!(snapshot.drops.updated_after_release(), 1);
        assert!(
            snapshot
                .context(owner.id())
                .is_some_and(super::ContextSnapshot::is_released)
        );
    }

    #[test]
    fn unknown_handles_share_the_root_counter_key() {
        let runtime = test_runtime(1, 4);
        static OPS: CounterDesc = CounterDesc::count("ops");
        // Handles the runtime never registered all report under the root
        // context, so they take its one key rather than one each.
        for handle in 100..110 {
            runtime.counter_add_at(&OPS, ContextHandle::new(handle), 1, 1);
        }
        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.drops.counter_updates(), 0);
        assert_eq!(snapshot.counters.len(), 1);
        assert_eq!(snapshot.counters[0].context_id, ContextId::new(0));
        assert_eq!(snapshot.counters[0].value, 10);
    }

    #[test]
    fn retired_contexts_free_their_counter_keys() {
        let runtime = Runtime::builder()
            .counter_capacity(1)
            .event_capacity(4)
            .build();
        static OPS: CounterDesc = CounterDesc::count("ops");
        let first = register(&runtime, &Context::root().with(&PASS, 1_u32));
        runtime.counter_add_at(&OPS, first, 1, 1);
        runtime.context_released(first);

        // No event refers to the released context, so its key makes room.
        let second = register(&runtime, &Context::root().with(&PASS, 2_u32));
        runtime.counter_add_at(&OPS, second, 5, 2);
        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.counters.len(), 1);
        assert_eq!(snapshot.counters[0].context_id, second.id());
        assert_eq!(snapshot.counters[0].value, 5);
        assert_eq!(snapshot.drops.counter_updates(), 0);
        assert_eq!(snapshot.drops.retired_rows(), 1, "the first context's row");
    }

    #[test]
    #[cfg_attr(target_os = "wasi", ignore = "wasm32-wasip1 cannot spawn threads")]
    fn counters_from_several_threads_merge_into_one_row() {
        let runtime = test_runtime(8, 8);
        static OPS: CounterDesc = CounterDesc::count("ops");
        static LEVEL: CounterDesc = CounterDesc::count("level");
        static LATENCY: CounterDesc = CounterDesc::new("latency", Unit::Nanos);
        let context = register(&runtime, &Context::root().with(&PASS, 1_u32));

        runtime.counter_add_at(&OPS, context, 2, 1);
        runtime.gauge_set_at(&LEVEL, context, 5, 1);
        runtime.histogram_record_at(&LATENCY, context, 4, 1);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                runtime.counter_add_at(&OPS, context, 3, 2);
                runtime.gauge_set_at(&LEVEL, context, 7, 2);
                runtime.histogram_record_at(&LATENCY, context, 9, 2);
            });
        });

        let snapshot = runtime.snapshot();
        let value = |kind| {
            snapshot
                .counters
                .iter()
                .find(|counter| counter.kind == kind)
                .map(|counter| counter.value)
        };
        assert_eq!(snapshot.counters.len(), 2);
        assert_eq!(value(CounterKind::Counter), Some(5));
        assert_eq!(value(CounterKind::Gauge), Some(7), "the latest write wins");
        assert_eq!(snapshot.histograms.len(), 1);
        assert_eq!(snapshot.histograms[0].count, 2);
        assert_eq!(snapshot.histograms[0].min, 4);
        assert_eq!(snapshot.histograms[0].max, 9);
    }

    #[test]
    #[cfg_attr(target_os = "wasi", ignore = "wasm32-wasip1 cannot spawn threads")]
    fn events_from_different_threads_carry_distinct_thread_ids() {
        let runtime = test_runtime(8, 8);
        let context_id = ContextId::new(0);
        std::thread::scope(|scope| {
            for _ in 0..2 {
                let runtime = &runtime;
                scope.spawn(move || {
                    runtime.push_event_at(
                        Observation::FrameMarker {
                            kind: MarkerKind::Frame,
                            phase: MarkerPhase::Begin,
                            number: None,
                            context_id,
                        },
                        1,
                    );
                });
            }
        });

        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.events.len(), 2);
        assert_ne!(snapshot.events[0].thread_id, snapshot.events[1].thread_id);
    }

    #[test]
    fn runtime_collects_context_table_and_timestamped_records() {
        let runtime = test_runtime(16, 16);
        static OPS: CounterDesc = CounterDesc::count("ops");
        static ATLAS: ResourceKindDesc = ResourceKindDesc::new("atlas");
        static RECORD: ScopeDesc = ScopeDesc::new("record");
        let context = Context::root().with(&PASS, 1_u32);
        let handle = register(&runtime, &context);
        let context_id = handle.id();

        runtime.counter_add_at(&OPS, handle, 5, 10);
        runtime.gauge_set_at(&OPS, handle, 9, 11);
        let token = runtime.next_scope_token();
        runtime.push_event_at(
            Observation::ScopeBegin {
                desc_id: RECORD.id(),
                name: RECORD.name(),
                location: RECORD.location(),
                context_id,
                token,
                args: Args::none().with("items", 3_u32),
            },
            12,
        );
        runtime.push_event_at(Observation::ScopeEnd { token }, 13);
        runtime.push_event_at(
            Observation::ResourceCreate {
                kind_id: ATLAS.id(),
                kind_name: ATLAS.name(),
                resource_id: ResourceId::new(7),
                context_id,
                size: 64,
            },
            14,
        );
        runtime.push_event_at(
            Observation::FrameMarker {
                kind: MarkerKind::Frame,
                phase: MarkerPhase::Begin,
                number: None,
                context_id,
            },
            15,
        );
        runtime.push_event_at(
            Observation::Flow {
                flow_id: FlowId::new(21),
                phase: FlowPhase::Begin,
                context_id,
            },
            16,
        );

        let snapshot = runtime.snapshot();
        assert_eq!(
            snapshot.metadata.process_name,
            super::default_process_name(),
            "the process is named after the executable where there is one"
        );
        assert!(
            snapshot.metadata.wall_clock_anchor_nanos > 0,
            "runtime creation should anchor snapshots to wall-clock time"
        );
        assert_eq!(snapshot.contexts[0].id, ContextId::new(0));
        assert_eq!(snapshot.contexts[1].context, context);
        assert_eq!(snapshot.counters.len(), 2);
        assert!(snapshot.counters.iter().any(|counter| {
            counter.kind == CounterKind::Counter
                && counter.value == 5
                && counter.name == "ops"
                && counter.unit == Unit::Count
                && counter.context_id == context_id
        }));
        assert_eq!(snapshot.events.len(), 5);
        assert_eq!(snapshot.events[0].at_nanos, 12);
        assert_eq!(snapshot.events[0].thread_id, super::current_thread_id());
        assert_eq!(
            snapshot.events[0].observation.context_id(),
            Some(context_id)
        );
    }

    #[test]
    fn scope_begins_refer_to_their_descriptors_location() {
        static LOCATED: ScopeDesc = ScopeDesc::with_location(
            "located",
            SourceLocation::new("tests::located", "lib.rs", 7),
        );
        static PLAIN: ScopeDesc = ScopeDesc::new("plain");
        let runtime = test_runtime(8, 8);
        for desc in [&LOCATED, &PLAIN] {
            let token = runtime.scope_begin(desc, ContextHandle::ROOT, &Args::none());
            runtime.scope_end(token);
        }

        let locations: Vec<_> = runtime
            .snapshot()
            .events
            .iter()
            .filter_map(|event| match &event.observation {
                Observation::ScopeBegin { location, .. } => Some(*location),
                _ => None,
            })
            .collect();
        assert_eq!(locations.len(), 2);
        let declared = LOCATED.location().expect("declared with a location");
        assert!(
            locations[0].is_some_and(|location| core::ptr::eq(location, declared)),
            "the descriptor's own location, not a copy"
        );
        assert_eq!(locations[1], None);
    }

    #[test]
    fn sequential_scopes_at_one_timestamp_keep_program_order() {
        let runtime = test_runtime(8, 8);
        static FIRST: ScopeDesc = ScopeDesc::new("first");
        static SECOND: ScopeDesc = ScopeDesc::new("second");
        let context_id = register(&runtime, &Context::root().with(&PASS, 1_u32)).id();

        for desc in [&FIRST, &SECOND] {
            let token = runtime.next_scope_token();
            runtime.push_event_at(
                Observation::ScopeBegin {
                    desc_id: desc.id(),
                    name: desc.name(),
                    location: desc.location(),
                    context_id,
                    token,
                    args: Args::none(),
                },
                5,
            );
            runtime.push_event_at(Observation::ScopeEnd { token }, 5);
        }

        let snapshot = runtime.snapshot();
        let order: Vec<_> = snapshot
            .events
            .iter()
            .map(|event| match &event.observation {
                Observation::ScopeBegin { name, .. } => ("begin", *name),
                Observation::ScopeEnd { token } => {
                    ("end", if token.get() == 1 { "first" } else { "second" })
                }
                other => panic!("unexpected observation {other:?}"),
            })
            .collect();
        assert_eq!(
            order,
            [
                ("begin", "first"),
                ("end", "first"),
                ("begin", "second"),
                ("end", "second"),
            ],
            "scopes at one timestamp must not appear nested"
        );
        assert!(
            snapshot
                .events
                .windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence),
            "sequence numbers should increase in recording order"
        );
    }

    #[test]
    fn snapshots_describe_their_producer() {
        let runtime = Runtime::builder()
            .process_name("demo")
            .session_name("recording")
            .level(InstrumentationLevel::Scoped)
            .counter_capacity(8)
            .event_capacity(16)
            .context_capacity(32)
            .build();

        let metadata = runtime.snapshot().metadata;
        assert_eq!(metadata.process_name, "demo");
        assert_eq!(metadata.session_name.as_deref(), Some("recording"));
        assert_eq!(metadata.instance_id, runtime.instance_id());
        assert_eq!(metadata.producer_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(
            metadata.capture,
            CaptureSettings::new(InstrumentationLevel::Scoped, 8, 16, 32)
                .with_level_floor(InstrumentationLevel::Scoped)
                .with_counter_samples(true)
                .with_compiled_out(cfg!(footfall_compile_out))
        );
        if !cfg!(target_os = "wasi") {
            assert_ne!(metadata.process_id, 0, "the process id is recorded");
        }
    }

    #[test]
    fn session_name_changes_through_shared_reference() {
        let runtime = Runtime::builder()
            .process_name("demo")
            .counter_capacity(8)
            .event_capacity(8)
            .session_name("first")
            .build();
        let shared: &Runtime = &runtime;

        assert_eq!(
            shared.snapshot().metadata.session_name.as_deref(),
            Some("first")
        );

        shared.set_session_name("second");
        assert_eq!(
            shared.snapshot().metadata.session_name.as_deref(),
            Some("second")
        );
        shared.clear_session_name();
        assert_eq!(shared.snapshot().metadata.session_name, None);
    }

    #[test]
    fn a_full_buffer_evicts_the_recording_threads_oldest_event() {
        let runtime = test_runtime(4, 2);
        let marker = |phase| Observation::FrameMarker {
            kind: MarkerKind::Frame,
            phase,
            number: None,
            context_id: ContextId::new(0),
        };
        runtime.push_event_at(marker(MarkerPhase::Begin), 1);
        runtime.push_event_at(marker(MarkerPhase::End), 2);
        runtime.push_event_at(marker(MarkerPhase::Begin), 3);

        let snapshot = runtime.snapshot();
        let times: Vec<_> = snapshot.events.iter().map(|event| event.at_nanos).collect();
        assert_eq!(times, [2, 3], "the newest events are kept");
        assert_eq!(snapshot.drops.evicted_events(), 1);
        assert_eq!(snapshot.window, EventWindow::new(0, 3, 1));
    }

    #[test]
    #[cfg_attr(target_os = "wasi", ignore = "wasm32-wasip1 cannot spawn threads")]
    fn a_thread_with_no_events_evicts_another_threads_oldest() {
        let runtime = test_runtime(4, 1);
        let marker = Observation::FrameMarker {
            kind: MarkerKind::Frame,
            phase: MarkerPhase::Begin,
            number: None,
            context_id: ContextId::new(0),
        };
        runtime.push_event_at(marker.clone(), 1);
        std::thread::scope(|scope| {
            scope.spawn(|| runtime.push_event_at(marker.clone(), 2));
        });

        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.events.len(), 1);
        assert_eq!(snapshot.events[0].at_nanos, 2);
        assert_eq!(snapshot.drops.evicted_events(), 1);
    }

    #[test]
    fn cursor_reads_return_new_events_and_count_misses() {
        let runtime = test_runtime(4, 4);
        let marker = Observation::FrameMarker {
            kind: MarkerKind::Frame,
            phase: MarkerPhase::Begin,
            number: None,
            context_id: ContextId::new(0),
        };
        let start = runtime.cursor();
        assert_eq!(start, EventCursor::new(runtime.instance_id(), 0));
        for at in 0..2 {
            runtime.push_event_at(marker.clone(), at);
        }

        let first = runtime.snapshot_since(start);
        assert_eq!(first.events.len(), 2);
        assert_eq!(first.window, EventWindow::new(0, 2, 0));
        assert!(
            runtime
                .snapshot_since(first.next_cursor())
                .events
                .is_empty()
        );

        // Six more events overflow the buffer of four, evicting two that
        // the next read had not seen.
        for at in 2..8 {
            runtime.push_event_at(marker.clone(), at);
        }
        let second = runtime.snapshot_since(first.next_cursor());
        let sequences: Vec<_> = second.events.iter().map(|event| event.sequence).collect();
        assert_eq!(sequences, [4, 5, 6, 7]);
        assert_eq!(second.window, EventWindow::new(2, 8, 2));
        assert_eq!(second.drops.evicted_events(), 4);

        // A cursor from another instance reads from the start.
        let foreign = EventCursor::new(runtime.instance_id() ^ 1, 6);
        assert_eq!(
            runtime.snapshot_since(foreign).window,
            EventWindow::new(0, 8, 4)
        );
    }

    #[test]
    #[cfg_attr(target_os = "wasi", ignore = "wasm32-wasip1 cannot spawn threads")]
    fn frame_ends_sample_the_counters_each_frame_changed() {
        static UPLOADS: CounterDesc = CounterDesc::bytes("uploads");
        static DEPTH: CounterDesc = CounterDesc::count("depth");
        let runtime = test_runtime(8, 64);
        let samples = |snapshot: &super::Snapshot| -> Vec<(&'static str, u64)> {
            snapshot
                .events
                .iter()
                .filter_map(|event| match event.observation {
                    Observation::CounterSample { name, value, .. } => Some((name, value)),
                    _ => None,
                })
                .collect()
        };

        // Frame 1: two threads upload, and the depth gauge is set.
        let cursor = runtime.cursor();
        runtime.frame_marker(MarkerKind::Frame, MarkerPhase::Begin, ContextHandle::ROOT);
        std::thread::scope(|scope| {
            for _ in 0..2 {
                scope.spawn(|| runtime.counter_add(&UPLOADS, ContextHandle::ROOT, 100));
            }
        });
        runtime.gauge_set(&DEPTH, ContextHandle::ROOT, 3);
        runtime.frame_marker(MarkerKind::Frame, MarkerPhase::End, ContextHandle::ROOT);
        let first = runtime.snapshot_since(cursor);
        assert_eq!(
            samples(&first),
            [("depth", 3), ("uploads", 200)],
            "samples merge every thread's updates"
        );

        // Frame 2: only uploads change, so only uploads are sampled, with
        // the cumulative value.
        let cursor = runtime.cursor();
        runtime.frame_marker(MarkerKind::Frame, MarkerPhase::Begin, ContextHandle::ROOT);
        runtime.counter_add(&UPLOADS, ContextHandle::ROOT, 50);
        runtime.frame_marker(MarkerKind::Frame, MarkerPhase::End, ContextHandle::ROOT);
        assert_eq!(samples(&runtime.snapshot_since(cursor)), [("uploads", 250)]);

        let quiet = Runtime::builder().counter_samples(false).build();
        quiet.counter_add(&UPLOADS, ContextHandle::ROOT, 1);
        quiet.frame_marker(MarkerKind::Frame, MarkerPhase::End, ContextHandle::ROOT);
        assert!(samples(&quiet.snapshot()).is_empty());
    }

    #[test]
    fn numbered_markers_keep_their_number_and_sample_counters() {
        static UPLOADS: CounterDesc = CounterDesc::bytes("uploads");
        let runtime = Runtime::default();
        // Frame 41 of the program's second window.
        let frame = footfall_core::FrameStream::new(2).frame(41);
        runtime.numbered_marker(
            MarkerKind::Frame,
            MarkerPhase::Begin,
            frame,
            ContextHandle::ROOT,
        );
        runtime.counter_add(&UPLOADS, ContextHandle::ROOT, 10);
        runtime.numbered_marker(
            MarkerKind::Frame,
            MarkerPhase::End,
            frame,
            ContextHandle::ROOT,
        );
        let snapshot = runtime.snapshot();
        let numbers: Vec<_> = snapshot
            .events
            .iter()
            .filter_map(|event| match event.observation {
                Observation::FrameMarker { number, .. } => Some(number),
                _ => None,
            })
            .collect();
        assert_eq!(numbers, [Some(frame), Some(frame)]);
        assert!(
            snapshot
                .events
                .iter()
                .any(|event| matches!(event.observation, Observation::CounterSample { .. })),
            "a numbered frame end samples counters like any frame end"
        );
    }

    #[test]
    fn runtimes_get_distinct_instance_ids() {
        let first = Runtime::default();
        let second = Runtime::default();
        assert_ne!(first.instance_id(), 0);
        assert_ne!(first.instance_id(), second.instance_id());
        assert_eq!(first.snapshot().metadata.instance_id, first.instance_id());
    }

    #[test]
    #[cfg_attr(target_os = "wasi", ignore = "wasm32-wasip1 cannot spawn threads")]
    fn snapshots_name_the_threads_that_recorded() {
        let runtime = test_runtime(4, 4);
        std::thread::scope(|scope| {
            std::thread::Builder::new()
                .name(String::from("worker-7"))
                .spawn_scoped(scope, || {
                    runtime.push_event_at(
                        Observation::FrameMarker {
                            kind: MarkerKind::Frame,
                            phase: MarkerPhase::Begin,
                            number: None,
                            context_id: ContextId::new(0),
                        },
                        1,
                    );
                })
                .expect("thread should spawn");
        });

        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.threads.len(), 1);
        let thread = &snapshot.threads[0];
        assert_eq!(thread.id, snapshot.events[0].thread_id);
        assert_eq!(thread.name(), Some("worker-7"));
    }

    #[test]
    #[cfg_attr(target_os = "wasi", ignore = "wasm32-wasip1 cannot spawn threads")]
    fn a_key_recorded_from_many_threads_takes_one_slot() {
        const THREADS: u64 = 12;
        let runtime = test_runtime(1, 4);
        static OPS: CounterDesc = CounterDesc::count("ops");
        std::thread::scope(|scope| {
            for _ in 0..THREADS {
                scope.spawn(|| runtime.counter_add_at(&OPS, ContextHandle::ROOT, 1, 1));
            }
        });

        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.counters.len(), 1);
        assert_eq!(snapshot.counters[0].value, THREADS);
        assert!(snapshot.drops.is_empty());
    }

    #[test]
    fn runtime_enforces_counter_capacity_across_shards() {
        let runtime = test_runtime(1, 4);
        let context = register(&runtime, &Context::root().with(&FRAME, 1_u32));
        static FIRST: CounterDesc = CounterDesc::count("first");
        static SECOND: CounterDesc = CounterDesc::count("second");

        runtime.counter_add_at(&FIRST, context, 1, 1);
        runtime.counter_add_at(&SECOND, context, 1, 2);

        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.counters.len(), 1);
        assert_eq!(snapshot.drops.counter_updates(), 1);
    }

    #[test]
    fn backend_respects_runtime_capture_level() {
        static COUNTER: CounterDesc = CounterDesc::count("ops");
        static SCOPE: ScopeDesc = ScopeDesc::new("record");
        static RESOURCE: ResourceKindDesc = ResourceKindDesc::new("atlas");

        let runtime = Runtime::builder()
            .counter_capacity(16)
            .event_capacity(16)
            .level(InstrumentationLevel::Counters)
            .build();

        let context = register(&runtime, &Context::root().with(&FRAME, 3_u32));
        runtime.counter_add(&COUNTER, context, 1);
        let token = runtime.scope_begin(&SCOPE, context, &Args::none());
        assert_eq!(token, ScopeToken::INVALID);
        runtime.scope_end(token);
        runtime.resource_create(&RESOURCE, ResourceId::new(7), context, 64);
        runtime.frame_marker(MarkerKind::Frame, MarkerPhase::Begin, context);
        runtime.flow(FlowId::new(9), FlowPhase::Begin, context);

        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.counters.len(), 1);
        assert_eq!(snapshot.events.len(), 0);

        // A scope begun while scopes were excluded must not record an end
        // after the level is raised.
        let excluded = runtime.scope_begin(&SCOPE, context, &Args::none());
        runtime.set_level(InstrumentationLevel::Trace);
        runtime.scope_end(excluded);
        assert!(runtime.snapshot().events.is_empty());

        let cursor = runtime.cursor();
        runtime.counter_add(&COUNTER, context, 1);
        let token = runtime.scope_begin(&SCOPE, context, &Args::none());
        runtime.scope_end(token);
        runtime.resource_create(&RESOURCE, ResourceId::new(7), context, 64);
        runtime.frame_marker(MarkerKind::Frame, MarkerPhase::Begin, context);
        runtime.flow(FlowId::new(9), FlowPhase::Begin, context);

        let snapshot = runtime.snapshot_since(cursor);
        assert_eq!(snapshot.counters.len(), 1);
        assert_eq!(snapshot.events.len(), 5);
    }

    #[test]
    fn snapshots_carry_dense_descriptor_ids_instead_of_addresses() {
        static OPS: CounterDesc = CounterDesc::count("ops");
        static LATENCY: CounterDesc = CounterDesc::new("latency", Unit::Nanos);
        static RECORD: ScopeDesc = ScopeDesc::new("record");
        static ATLAS: ResourceKindDesc = ResourceKindDesc::new("atlas");
        static LOG: footfall_core::EventDesc = footfall_core::EventDesc::new("log");

        let runtime = Runtime::builder()
            .counter_capacity(16)
            .event_capacity(16)
            .level(InstrumentationLevel::Trace)
            .build();
        let context = register(&runtime, &Context::root().with(&FRAME, 1_u32));
        for _ in 0..2 {
            let token = runtime.scope_begin(&RECORD, context, &Args::none());
            runtime.scope_end(token);
        }
        runtime.resource_create(&ATLAS, ResourceId::new(1), context, 64);
        runtime.message(&LOG, context, "hello", &Args::none());
        runtime.counter_add(&OPS, context, 1);
        runtime.counter_add(&OPS, ContextHandle::ROOT, 1);
        runtime.histogram_record(&OPS, context, 5);
        runtime.histogram_record(&LATENCY, context, 7);

        let snapshot = runtime.snapshot();
        let scope_ids: Vec<u64> = snapshot
            .events
            .iter()
            .filter_map(|event| match &event.observation {
                Observation::ScopeBegin { desc_id, .. } => Some(desc_id.get()),
                _ => None,
            })
            .collect();
        let kind_id = snapshot
            .events
            .iter()
            .find_map(|event| match &event.observation {
                Observation::ResourceCreate { kind_id, .. } => Some(kind_id.get()),
                _ => None,
            })
            .expect("resource event should be recorded");
        let message_id = snapshot
            .events
            .iter()
            .find_map(|event| match &event.observation {
                Observation::Message { desc_id, .. } => Some(desc_id.get()),
                _ => None,
            })
            .expect("message event should be recorded");
        let ops_counter_ids: Vec<u64> = snapshot
            .counters
            .iter()
            .map(|counter| counter.desc_id.get())
            .collect();
        let histogram_id = |name: &str| {
            snapshot
                .histograms
                .iter()
                .find(|histogram| histogram.name == name)
                .map(|histogram| histogram.desc_id.get())
                .expect("histogram should be recorded")
        };

        // Events come first in assignment order, then counters and
        // histograms; one id space covers every descriptor kind.
        assert_eq!(scope_ids, [1, 1], "one scope descriptor, one id");
        assert_eq!(kind_id, 2);
        assert_eq!(message_id, 3);
        assert_eq!(ops_counter_ids, [4, 4], "same descriptor in two contexts");
        assert_eq!(histogram_id("ops"), 4, "counters and histograms agree");
        assert_eq!(histogram_id("latency"), 5);
        assert_ne!(RECORD.id().get(), 1, "the address id stays in process");

        // Ids stay fixed for the life of the runtime.
        runtime.histogram_record(&LATENCY, context, 7);
        let again = runtime.snapshot();
        assert_eq!(again.counters[0].desc_id.get(), 4);
        assert_eq!(histogram_id("latency"), 5);
        assert_eq!(
            again
                .histograms
                .iter()
                .find(|histogram| histogram.name == "latency")
                .map(|histogram| histogram.desc_id.get()),
            Some(5)
        );
    }
}
