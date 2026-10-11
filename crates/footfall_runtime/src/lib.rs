// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Bounded runtime collection for Footfall.

use footfall_core::{
    Args, Backend, Context, ContextId, CounterDesc, CounterDescId, EventDesc, EventDescId, FlowId,
    FlowPhase, InstallBackendError, InstrumentationLevel, MarkerKind, MarkerPhase, ResourceId,
    ResourceKindDesc, ResourceKindId, ScopeDesc, ScopeDescId, ScopeToken, Unit, install_backend,
};
use std::{
    boxed::Box,
    collections::{HashMap, VecDeque},
    string::String,
    sync::{
        Mutex,
        atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const DEFAULT_SHARD_COUNT: usize = 16;

/// Maximum retained message text length in bytes.
///
/// Longer texts are truncated on a UTF-8 boundary before retention so one
/// noisy log line cannot dominate event memory.
pub const MESSAGE_TEXT_MAX_BYTES: usize = 256;

/// In-process bounded collector for counters and timestamped observation records.
#[derive(Debug)]
pub struct Runtime {
    created_at: Instant,
    wall_clock_anchor_micros: u64,
    process_name: String,
    level: AtomicU8,
    counter_capacity: usize,
    event_capacity: usize,
    next_scope_token: AtomicU64,
    next_event_sequence: AtomicU64,
    counter_count: AtomicUsize,
    event_count: AtomicUsize,
    dropped_counters: AtomicU64,
    dropped_events: AtomicU64,
    session: Mutex<SessionState>,
    contexts: Mutex<ContextState>,
    counter_shards: Vec<Mutex<CounterShard>>,
    event_shards: Vec<Mutex<EventShard>>,
}

impl Runtime {
    /// Creates a runtime with explicit counter-key and event capacities.
    #[must_use]
    pub fn new(counter_capacity: usize, event_capacity: usize) -> Self {
        Self::named("footfall", counter_capacity, event_capacity)
    }

    /// Creates a runtime with explicit capacities and capture level.
    #[must_use]
    pub fn with_level(
        counter_capacity: usize,
        event_capacity: usize,
        level: InstrumentationLevel,
    ) -> Self {
        Self::named_with_level("footfall", counter_capacity, event_capacity, level)
    }

    /// Creates a runtime with an explicit process name.
    #[must_use]
    pub fn named(
        process_name: impl Into<String>,
        counter_capacity: usize,
        event_capacity: usize,
    ) -> Self {
        Self::named_with_level(
            process_name,
            counter_capacity,
            event_capacity,
            InstrumentationLevel::Trace,
        )
    }

    /// Creates a runtime with explicit process name, capacities, and capture level.
    #[must_use]
    pub fn named_with_level(
        process_name: impl Into<String>,
        counter_capacity: usize,
        event_capacity: usize,
        level: InstrumentationLevel,
    ) -> Self {
        Self {
            created_at: Instant::now(),
            wall_clock_anchor_micros: duration_to_micros(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or(Duration::ZERO),
            ),
            process_name: process_name.into(),
            level: AtomicU8::new(level_to_u8(level)),
            counter_capacity,
            event_capacity,
            next_scope_token: AtomicU64::new(1),
            next_event_sequence: AtomicU64::new(0),
            counter_count: AtomicUsize::new(0),
            event_count: AtomicUsize::new(0),
            dropped_counters: AtomicU64::new(0),
            dropped_events: AtomicU64::new(0),
            session: Mutex::new(SessionState::default()),
            contexts: Mutex::new(ContextState::new(context_capacity(
                counter_capacity,
                event_capacity,
            ))),
            counter_shards: build_counter_shards(DEFAULT_SHARD_COUNT),
            event_shards: build_event_shards(DEFAULT_SHARD_COUNT),
        }
    }

    /// Adds a capture-session label and returns the runtime.
    #[must_use]
    pub fn with_session_name(self, session_name: impl Into<String>) -> Self {
        self.set_session_name(session_name);
        self
    }

    /// Installs this runtime as the process-wide backend and keeps it alive for the process.
    ///
    /// The runtime is leaked so the returned reference stays valid for the rest of the
    /// process. To compose the runtime with other backends instead, leak it yourself
    /// (for example with [`Box::leak`]) and pass the reference to an adapter such as
    /// `footfall_fanout`, or to [`footfall_core::install_backend`].
    pub fn install_global(self) -> Result<&'static Self, InstallBackendError> {
        let runtime: &'static Self = Box::leak(Box::new(self));
        install_backend(runtime)?;
        Ok(runtime)
    }

    /// Returns the current runtime capture level.
    #[must_use]
    pub fn level(&self) -> InstrumentationLevel {
        level_from_u8(self.level.load(Ordering::Relaxed))
    }

    /// Sets the runtime capture level used by subsequent backend emissions.
    pub fn set_level(&self, level: InstrumentationLevel) {
        self.level.store(level_to_u8(level), Ordering::Relaxed);
    }

    /// Sets an optional capture-session label for future snapshots.
    ///
    /// The label persists across session resets until cleared, and can be
    /// changed after the runtime is installed as the process-wide backend.
    pub fn set_session_name(&self, session_name: impl Into<String>) {
        self.session
            .lock()
            .expect("runtime session mutex should not be poisoned")
            .session_name = Some(session_name.into());
    }

    /// Clears the capture-session label for future snapshots.
    pub fn clear_session_name(&self) {
        self.session
            .lock()
            .expect("runtime session mutex should not be poisoned")
            .session_name = None;
    }

    /// Returns a snapshot of the currently collected counters, contexts, and events.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        let captured_at_micros = self.now_micros();
        self.snapshot_at(captured_at_micros)
    }

    /// Drains the current capture state into a snapshot and starts a fresh session.
    #[must_use]
    pub fn drain_snapshot(&self) -> Snapshot {
        let captured_at_micros = self.now_micros();
        let snapshot = self.snapshot_at(captured_at_micros);
        self.reset_at(captured_at_micros);
        snapshot
    }

    /// Clears the current capture state and starts a fresh session.
    pub fn reset(&self) {
        let captured_at_micros = self.now_micros();
        self.reset_at(captured_at_micros);
    }

    fn metadata(&self, session: &SessionState) -> SnapshotMetadata {
        SnapshotMetadata {
            process_name: self.process_name.clone(),
            session_name: session.session_name.clone(),
            session_sequence: session.session_sequence,
            wall_clock_anchor_micros: self.wall_clock_anchor_micros,
        }
    }

    fn now_micros(&self) -> u64 {
        duration_to_micros(self.created_at.elapsed())
    }

    fn snapshot_at(&self, captured_at_micros: u64) -> Snapshot {
        let mut session = self
            .session
            .lock()
            .expect("runtime session mutex should not be poisoned");
        let contexts = self
            .contexts
            .lock()
            .expect("runtime context mutex should not be poisoned");
        let mut counters = Vec::with_capacity(self.counter_count.load(Ordering::Relaxed));
        let mut histograms = Vec::new();
        for shard in &self.counter_shards {
            let shard = shard
                .lock()
                .expect("runtime counter shard should not be poisoned");
            counters.extend(shard.counters.values().cloned());
            histograms.extend(shard.histograms.values().cloned());
        }

        let mut events = Vec::with_capacity(self.event_count.load(Ordering::Relaxed));
        for shard in &self.event_shards {
            events.extend(
                shard
                    .lock()
                    .expect("runtime event shard should not be poisoned")
                    .events
                    .iter()
                    .cloned(),
            );
        }
        events.sort_by_key(event_sort_key);
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
        session
            .descriptor_ids
            .remap(&mut events, &mut counters, &mut histograms);

        Snapshot {
            metadata: self.metadata(&session),
            capture_start_micros: session.capture_start_micros,
            captured_at_micros,
            contexts: contexts.contexts.clone(),
            counters,
            histograms,
            events,
            dropped_contexts: contexts.dropped_contexts,
            dropped_counters: self.dropped_counters.load(Ordering::Relaxed),
            dropped_events: self.dropped_events.load(Ordering::Relaxed),
        }
    }

    fn reset_at(&self, capture_start_micros: u64) {
        let mut session = self
            .session
            .lock()
            .expect("runtime session mutex should not be poisoned");
        session.capture_start_micros = capture_start_micros;
        session.session_sequence = session.session_sequence.saturating_add(1);
        session.descriptor_ids.clear();

        // Scope tokens stay monotonic across session resets so a scope that
        // straddles a drain cannot collide with tokens issued afterwards.
        self.counter_count.store(0, Ordering::Relaxed);
        self.event_count.store(0, Ordering::Relaxed);
        self.dropped_counters.store(0, Ordering::Relaxed);
        self.dropped_events.store(0, Ordering::Relaxed);

        self.contexts
            .lock()
            .expect("runtime context mutex should not be poisoned")
            .reset();
        for shard in &self.counter_shards {
            let mut shard = shard
                .lock()
                .expect("runtime counter shard should not be poisoned");
            shard.counters.clear();
            shard.histograms.clear();
        }
        for shard in &self.event_shards {
            shard
                .lock()
                .expect("runtime event shard should not be poisoned")
                .events
                .clear();
        }
    }

    fn counter_add_at(
        &self,
        desc: &'static CounterDesc,
        context: &Context,
        value: u64,
        at_micros: u64,
    ) {
        self.counter_update_at(desc, context, value, at_micros, CounterKind::Counter);
    }

    fn gauge_set_at(
        &self,
        desc: &'static CounterDesc,
        context: &Context,
        value: u64,
        at_micros: u64,
    ) {
        self.counter_update_at(desc, context, value, at_micros, CounterKind::Gauge);
    }

    fn histogram_record_at(
        &self,
        desc: &'static CounterDesc,
        context: &Context,
        value: u64,
        at_micros: u64,
    ) {
        let Some(context_id) = self.context_id(context) else {
            self.dropped_counters.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let key = HistogramKey {
            desc_id: desc.id(),
            context_id,
        };
        let shard_index = self.histogram_shard_index(key);
        let mut shard = self.counter_shards[shard_index]
            .lock()
            .expect("runtime counter shard should not be poisoned");
        if let Some(histogram) = shard.histograms.get_mut(&key) {
            histogram.record(value, at_micros);
            return;
        }

        if !self.reserve_counter_slot() {
            self.dropped_counters.fetch_add(1, Ordering::Relaxed);
            return;
        }

        let mut histogram = HistogramSnapshot::new(desc, context_id);
        histogram.record(value, at_micros);
        shard.histograms.insert(key, histogram);
    }

    fn counter_update_at(
        &self,
        desc: &'static CounterDesc,
        context: &Context,
        value: u64,
        at_micros: u64,
        kind: CounterKind,
    ) {
        let Some(context_id) = self.context_id(context) else {
            self.dropped_counters.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let key = CounterKey::new(desc.id(), context_id, kind);
        let shard_index = self.counter_shard_index(key);
        let mut shard = self.counter_shards[shard_index]
            .lock()
            .expect("runtime counter shard should not be poisoned");
        if let Some(snapshot) = shard.counters.get_mut(&key) {
            snapshot.value = match kind {
                CounterKind::Counter => snapshot.value.saturating_add(value),
                CounterKind::Gauge => value,
            };
            snapshot.updated_at_micros = at_micros;
            return;
        }

        if !self.reserve_counter_slot() {
            self.dropped_counters.fetch_add(1, Ordering::Relaxed);
            return;
        }

        shard.counters.insert(
            key,
            CounterSnapshot {
                desc_id: desc.id(),
                name: desc.name(),
                unit: desc.unit(),
                context_id,
                kind,
                value,
                updated_at_micros: at_micros,
            },
        );
    }

    fn next_scope_token(&self) -> ScopeToken {
        ScopeToken::new(self.next_scope_token.fetch_add(1, Ordering::Relaxed))
    }

    fn push_event_at(&self, observation: Observation, at_micros: u64) {
        if !self.reserve_event_slot() {
            self.dropped_events.fetch_add(1, Ordering::Relaxed);
            return;
        }

        let shard_index = self.event_shard_index(&observation);
        // Taking the sequence number after the slot reservation keeps it in
        // program order for the recording thread, which timestamps alone
        // cannot guarantee at microsecond resolution.
        let sequence = self.next_event_sequence.fetch_add(1, Ordering::Relaxed);
        self.event_shards[shard_index]
            .lock()
            .expect("runtime event shard should not be poisoned")
            .events
            .push_back(TimedObservation {
                at_micros,
                sequence,
                thread_id: current_thread_id(),
                observation,
            });
    }

    fn context_id(&self, context: &Context) -> Option<ContextId> {
        self.contexts
            .lock()
            .expect("runtime context mutex should not be poisoned")
            .context_id(context)
    }

    fn reserve_counter_slot(&self) -> bool {
        reserve_slot(&self.counter_count, self.counter_capacity)
    }

    fn reserve_event_slot(&self) -> bool {
        reserve_slot(&self.event_count, self.event_capacity)
    }

    fn counter_shard_index(&self, key: CounterKey) -> usize {
        shard_index(
            key.desc_id
                .get()
                .wrapping_mul(31)
                .wrapping_add(key.context_id.get())
                .wrapping_add(counter_kind_discriminant(key.kind)),
            self.counter_shards.len(),
        )
    }

    fn histogram_shard_index(&self, key: HistogramKey) -> usize {
        shard_index(
            key.desc_id
                .get()
                .wrapping_mul(31)
                .wrapping_add(key.context_id.get())
                .wrapping_add(2),
            self.counter_shards.len(),
        )
    }

    fn event_shard_index(&self, observation: &Observation) -> usize {
        let raw = observation
            .context_id()
            .map_or_else(|| scope_token_for_observation(observation), ContextId::get);
        shard_index(raw, self.event_shards.len())
    }
}

/// Snapshot of counters, canonical contexts, and events retained by a [`Runtime`].
///
/// Descriptor ids in a snapshot ([`ScopeDescId`], [`CounterDescId`],
/// [`EventDescId`], and [`ResourceKindId`]) are opaque and session-scoped:
/// dense integers from `1`, shared across descriptor kinds, assigned in
/// first-seen order (events, then counters, then histograms). They stay fixed
/// for the rest of a capture session, so repeated snapshots agree, and start
/// over when the session resets. The address-derived identity that
/// descriptors have inside the process never appears in a snapshot.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Snapshot {
    /// Snapshot metadata identifying the process and capture session.
    pub metadata: SnapshotMetadata,
    /// Snapshot start time relative to runtime creation.
    pub capture_start_micros: u64,
    /// Time since runtime creation when the snapshot was captured.
    pub captured_at_micros: u64,
    /// Canonical context table referenced by counters and events.
    pub contexts: Vec<ContextSnapshot>,
    /// Aggregated counters and gauges.
    pub counters: Vec<CounterSnapshot>,
    /// Aggregated value distributions.
    pub histograms: Vec<HistogramSnapshot>,
    /// Bounded sequence of timestamped observation records, ordered by
    /// timestamp and then by recording sequence.
    pub events: Vec<TimedObservation>,
    /// Count of observations dropped because a new context could not be interned.
    pub dropped_contexts: u64,
    /// Count of counter updates dropped due to counter-key capacity pressure.
    pub dropped_counters: u64,
    /// Count of events dropped due to event-capacity pressure.
    pub dropped_events: u64,
}

impl Snapshot {
    /// Resolves one canonical context by id.
    #[must_use]
    pub fn context(&self, context_id: ContextId) -> Option<&ContextSnapshot> {
        usize::try_from(context_id.get())
            .ok()
            .and_then(|index| self.contexts.get(index))
    }
}

/// Metadata describing the process and session that produced a snapshot.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SnapshotMetadata {
    /// Process name used by exporters.
    pub process_name: String,
    /// Optional capture-session label.
    pub session_name: Option<String>,
    /// Monotonic sequence number of the capture session.
    pub session_sequence: u64,
    /// Wall-clock time of runtime creation in microseconds since the Unix
    /// epoch.
    ///
    /// All snapshot timestamps are relative to runtime creation, so adding
    /// this anchor to any `*_micros` value yields wall-clock time. The anchor
    /// lets consumers correlate captures with logs and merge sessions from
    /// multiple processes onto one timeline.
    pub wall_clock_anchor_micros: u64,
}

/// One canonical context carried by a snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextSnapshot {
    /// Canonical context id.
    pub id: ContextId,
    /// Explicit semantic context value.
    pub context: Context,
}

/// One aggregated counter or gauge value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CounterSnapshot {
    /// Session-scoped counter descriptor id; see [`Snapshot`].
    pub desc_id: CounterDescId,
    /// Stable descriptor name.
    pub name: &'static str,
    /// Unit copied from the counter descriptor.
    pub unit: Unit,
    /// Canonical semantic context id.
    pub context_id: ContextId,
    /// Whether the value is additive or gauge-like.
    pub kind: CounterKind,
    /// Aggregated value.
    pub value: u64,
    /// Last update time relative to runtime creation.
    pub updated_at_micros: u64,
}

/// Distinguishes additive counters from gauges.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CounterKind {
    /// Additive counter.
    Counter,
    /// Last-value gauge.
    Gauge,
}

/// Number of power-of-two buckets carried by a histogram.
pub const HISTOGRAM_BUCKET_COUNT: usize = 64;

/// One aggregated value distribution.
///
/// Bucket `i` counts samples whose bit length is `i`: bucket `0` holds zero
/// values, bucket `1` holds `1`, bucket `2` holds `2..=3`, and so on. The top
/// bucket merges 63-bit and 64-bit samples. Histogram keys count against the
/// runtime counter capacity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistogramSnapshot {
    /// Session-scoped counter descriptor id; see [`Snapshot`].
    pub desc_id: CounterDescId,
    /// Stable descriptor name.
    pub name: &'static str,
    /// Unit copied from the counter descriptor.
    pub unit: Unit,
    /// Canonical semantic context id.
    pub context_id: ContextId,
    /// Number of recorded samples.
    pub count: u64,
    /// Saturating sum of recorded samples.
    pub sum: u64,
    /// Smallest recorded sample. Meaningless while `count` is zero.
    pub min: u64,
    /// Largest recorded sample.
    pub max: u64,
    /// Power-of-two bucket counts indexed by sample bit length.
    pub buckets: [u64; HISTOGRAM_BUCKET_COUNT],
    /// Last update time relative to runtime creation.
    pub updated_at_micros: u64,
}

impl HistogramSnapshot {
    fn new(desc: &'static CounterDesc, context_id: ContextId) -> Self {
        Self {
            desc_id: desc.id(),
            name: desc.name(),
            unit: desc.unit(),
            context_id,
            count: 0,
            sum: 0,
            min: u64::MAX,
            max: 0,
            buckets: [0; HISTOGRAM_BUCKET_COUNT],
            updated_at_micros: 0,
        }
    }

    fn record(&mut self, value: u64, at_micros: u64) {
        self.count = self.count.saturating_add(1);
        self.sum = self.sum.saturating_add(value);
        self.min = self.min.min(value);
        self.max = self.max.max(value);
        self.buckets[histogram_bucket_index(value)] += 1;
        self.updated_at_micros = at_micros;
    }
}

/// Returns the bucket index for one histogram sample.
#[must_use]
pub fn histogram_bucket_index(value: u64) -> usize {
    ((u64::BITS - value.leading_zeros()) as usize).min(HISTOGRAM_BUCKET_COUNT - 1)
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct HistogramKey {
    desc_id: CounterDescId,
    context_id: ContextId,
}

/// Observation paired with a timestamp relative to runtime creation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimedObservation {
    /// Observation time in microseconds since runtime creation.
    pub at_micros: u64,
    /// Runtime-assigned recording sequence number.
    ///
    /// Sequence numbers increase in recording order across all threads and
    /// stay monotonic across session resets. Snapshots order events by
    /// timestamp and then by sequence, so events recorded by one thread
    /// within the same microsecond keep their program order.
    pub sequence: u64,
    /// Runtime-assigned id of the thread that recorded the observation.
    ///
    /// Ids are small integers assigned in first-emission order. They identify
    /// threads within one process lifetime and are not OS thread ids.
    pub thread_id: u64,
    /// Observation payload.
    pub observation: Observation,
}

/// One retained observation record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Observation {
    /// Scope begin event.
    ScopeBegin {
        /// Session-scoped descriptor id for the scope; see [`Snapshot`].
        desc_id: ScopeDescId,
        /// Stable descriptor name.
        name: &'static str,
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
        /// Session-scoped resource kind id; see [`Snapshot`].
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
        /// Session-scoped resource kind id; see [`Snapshot`].
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
        /// Session-scoped resource kind id; see [`Snapshot`].
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
        /// Session-scoped descriptor id for the message channel; see
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
            | Self::Message { context_id, .. } => Some(*context_id),
            Self::ScopeEnd { .. } => None,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct SessionState {
    capture_start_micros: u64,
    session_sequence: u64,
    session_name: Option<String>,
    descriptor_ids: DescriptorIds,
}

/// Maps descriptor addresses to dense, session-scoped snapshot ids.
///
/// Descriptor identity inside the process is the descriptor's address, which
/// must never reach a snapshot consumer: it would reveal the process memory
/// layout to anything that can attach to a session. Snapshots therefore carry
/// dense ids from one id space shared by every descriptor kind (scope,
/// counter, event, and resource kind), assigned from `1` in first-seen order:
/// events in snapshot order, then counters, then histograms. Assignments stay
/// fixed for the rest of the session, so repeated snapshots agree, and start
/// over when the session resets.
#[derive(Clone, Debug, Default)]
struct DescriptorIds {
    dense: HashMap<u64, u64>,
}

impl DescriptorIds {
    fn clear(&mut self) {
        self.dense.clear();
    }

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
                Observation::Message { desc_id, .. } => {
                    *desc_id = EventDescId::new(self.dense(desc_id.get()));
                }
                Observation::ResourceCreate { kind_id, .. }
                | Observation::ResourceUpdate { kind_id, .. }
                | Observation::ResourceDrop { kind_id, .. } => {
                    *kind_id = ResourceKindId::new(self.dense(kind_id.get()));
                }
                Observation::ScopeEnd { .. }
                | Observation::FrameMarker { .. }
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

#[derive(Debug)]
struct ContextState {
    capacity: usize,
    next_context_id: u64,
    context_ids: HashMap<Context, ContextId>,
    contexts: Vec<ContextSnapshot>,
    dropped_contexts: u64,
}

impl ContextState {
    fn new(capacity: usize) -> Self {
        let root_id = ContextId::new(0);
        let root_context = Context::root();
        let mut context_ids = HashMap::with_capacity(capacity);
        context_ids.insert(root_context, root_id);

        Self {
            capacity,
            next_context_id: 1,
            context_ids,
            contexts: vec![ContextSnapshot {
                id: root_id,
                context: root_context,
            }],
            dropped_contexts: 0,
        }
    }

    fn reset(&mut self) {
        self.next_context_id = 1;
        self.context_ids.clear();
        self.contexts.clear();
        let root_id = ContextId::new(0);
        let root_context = Context::root();
        self.context_ids.insert(root_context, root_id);
        self.contexts.push(ContextSnapshot {
            id: root_id,
            context: root_context,
        });
        self.dropped_contexts = 0;
    }

    fn context_id(&mut self, context: &Context) -> Option<ContextId> {
        if let Some(context_id) = self.context_ids.get(context) {
            return Some(*context_id);
        }

        if self.contexts.len() == self.capacity {
            self.dropped_contexts = self.dropped_contexts.saturating_add(1);
            return None;
        }

        let context_id = ContextId::new(self.next_context_id);
        self.next_context_id = self.next_context_id.saturating_add(1);
        self.context_ids.insert(*context, context_id);
        self.contexts.push(ContextSnapshot {
            id: context_id,
            context: *context,
        });
        Some(context_id)
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
    fn counter_add(&self, desc: &'static CounterDesc, context: &Context, value: u64) {
        if !self.level().captures_counters() {
            return;
        }
        let at_micros = self.now_micros();
        self.counter_add_at(desc, context, value, at_micros);
    }

    fn gauge_set(&self, desc: &'static CounterDesc, context: &Context, value: u64) {
        if !self.level().captures_counters() {
            return;
        }
        let at_micros = self.now_micros();
        self.gauge_set_at(desc, context, value, at_micros);
    }

    fn histogram_record(&self, desc: &'static CounterDesc, context: &Context, value: u64) {
        if !self.level().captures_counters() {
            return;
        }
        let at_micros = self.now_micros();
        self.histogram_record_at(desc, context, value, at_micros);
    }

    fn scope_begin(&self, desc: &'static ScopeDesc, context: &Context, args: &Args) -> ScopeToken {
        if !self.level().captures_scopes() {
            return ScopeToken::INVALID;
        }
        let at_micros = self.now_micros();
        let Some(context_id) = self.context_id(context) else {
            self.dropped_events.fetch_add(1, Ordering::Relaxed);
            return ScopeToken::INVALID;
        };
        let token = self.next_scope_token();
        self.push_event_at(
            Observation::ScopeBegin {
                desc_id: desc.id(),
                name: desc.name(),
                context_id,
                token,
                args: *args,
            },
            at_micros,
        );
        token
    }

    fn scope_end(&self, token: ScopeToken) {
        if !token.is_valid() {
            return;
        }
        if !self.level().captures_scopes() {
            return;
        }
        let at_micros = self.now_micros();
        self.push_event_at(Observation::ScopeEnd { token }, at_micros);
    }

    fn resource_create(
        &self,
        desc: &'static ResourceKindDesc,
        resource_id: ResourceId,
        context: &Context,
        size: u64,
    ) {
        if !self.level().captures_resources() {
            return;
        }
        let at_micros = self.now_micros();
        let Some(context_id) = self.context_id(context) else {
            self.dropped_events.fetch_add(1, Ordering::Relaxed);
            return;
        };
        self.push_event_at(
            Observation::ResourceCreate {
                kind_id: desc.id(),
                kind_name: desc.name(),
                resource_id,
                context_id,
                size,
            },
            at_micros,
        );
    }

    fn resource_update(
        &self,
        desc: &'static ResourceKindDesc,
        resource_id: ResourceId,
        context: &Context,
        size: u64,
    ) {
        if !self.level().captures_resources() {
            return;
        }
        let at_micros = self.now_micros();
        let Some(context_id) = self.context_id(context) else {
            self.dropped_events.fetch_add(1, Ordering::Relaxed);
            return;
        };
        self.push_event_at(
            Observation::ResourceUpdate {
                kind_id: desc.id(),
                kind_name: desc.name(),
                resource_id,
                context_id,
                size,
            },
            at_micros,
        );
    }

    fn resource_drop(
        &self,
        desc: &'static ResourceKindDesc,
        resource_id: ResourceId,
        context: &Context,
    ) {
        if !self.level().captures_resources() {
            return;
        }
        let at_micros = self.now_micros();
        let Some(context_id) = self.context_id(context) else {
            self.dropped_events.fetch_add(1, Ordering::Relaxed);
            return;
        };
        self.push_event_at(
            Observation::ResourceDrop {
                kind_id: desc.id(),
                kind_name: desc.name(),
                resource_id,
                context_id,
            },
            at_micros,
        );
    }

    fn frame_marker(&self, kind: MarkerKind, phase: MarkerPhase, context: &Context) {
        if !self.level().captures_markers() {
            return;
        }
        let at_micros = self.now_micros();
        let Some(context_id) = self.context_id(context) else {
            self.dropped_events.fetch_add(1, Ordering::Relaxed);
            return;
        };
        self.push_event_at(
            Observation::FrameMarker {
                kind,
                phase,
                context_id,
            },
            at_micros,
        );
    }

    fn flow(&self, flow_id: FlowId, phase: FlowPhase, context: &Context) {
        if !self.level().captures_flows() {
            return;
        }
        let at_micros = self.now_micros();
        let Some(context_id) = self.context_id(context) else {
            self.dropped_events.fetch_add(1, Ordering::Relaxed);
            return;
        };
        self.push_event_at(
            Observation::Flow {
                flow_id,
                phase,
                context_id,
            },
            at_micros,
        );
    }

    fn message(&self, desc: &'static EventDesc, context: &Context, text: &str, args: &Args) {
        if !self.level().captures_messages() {
            return;
        }
        let at_micros = self.now_micros();
        let Some(context_id) = self.context_id(context) else {
            self.dropped_events.fetch_add(1, Ordering::Relaxed);
            return;
        };
        self.push_event_at(
            Observation::Message {
                desc_id: desc.id(),
                name: desc.name(),
                context_id,
                text: truncate_message_text(text),
                args: *args,
            },
            at_micros,
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

std::thread_local! {
    static THREAD_ID: u64 = NEXT_THREAD_ID.fetch_add(1, Ordering::Relaxed);
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

const fn context_capacity(counter_capacity: usize, event_capacity: usize) -> usize {
    counter_capacity
        .saturating_add(event_capacity)
        .saturating_add(1)
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

fn shard_index(raw: u64, shard_count: usize) -> usize {
    let mixed = raw ^ raw.rotate_left(17) ^ raw.rotate_right(11);
    usize::try_from(mixed).unwrap_or(0) % shard_count.max(1)
}

const fn counter_kind_discriminant(kind: CounterKind) -> u64 {
    match kind {
        CounterKind::Counter => 0,
        CounterKind::Gauge => 1,
    }
}

fn scope_token_for_observation(observation: &Observation) -> u64 {
    match observation {
        Observation::ScopeEnd { token } => token.get(),
        Observation::ScopeBegin { token, .. } => token.get(),
        Observation::ResourceCreate { resource_id, .. }
        | Observation::ResourceUpdate { resource_id, .. }
        | Observation::ResourceDrop { resource_id, .. } => resource_id.get(),
        Observation::FrameMarker { .. }
        | Observation::Flow { .. }
        | Observation::Message { .. } => 0,
    }
}

fn event_sort_key(event: &TimedObservation) -> (u64, u64) {
    (event.at_micros, event.sequence)
}

fn duration_to_micros(duration: Duration) -> u64 {
    let micros = duration.as_micros();
    u64::try_from(micros).unwrap_or(u64::MAX)
}

const fn level_to_u8(level: InstrumentationLevel) -> u8 {
    match level {
        InstrumentationLevel::Off => 0,
        InstrumentationLevel::Counters => 1,
        InstrumentationLevel::Scoped => 2,
        InstrumentationLevel::Trace => 3,
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
    use super::{CounterKind, Observation, Runtime};
    use footfall_core::{
        Args, Backend as _, Context, ContextId, CounterDesc, FlowId, FlowPhase,
        InstrumentationLevel, LabelKey, MarkerKind, MarkerPhase, ResourceId, ResourceKindDesc,
        ScopeDesc, ScopeToken, Unit,
    };

    static FRAME: LabelKey = LabelKey::new("frame");
    static PASS: LabelKey = LabelKey::new("pass");
    static NODE: LabelKey = LabelKey::new("node");

    #[test]
    fn messages_are_captured_with_truncated_text() {
        let runtime = Runtime::new(8, 8);
        static CHANNEL: footfall_core::EventDesc = footfall_core::EventDesc::new("app_log");
        let context_id = runtime
            .context_id(&Context::root())
            .expect("root context should exist");

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
        let runtime = Runtime::new(8, 8);
        static LATENCY: CounterDesc = CounterDesc::new("frame_micros", Unit::Micros);
        let context = Context::root().with(&PASS, 1_u32);

        runtime.histogram_record_at(&LATENCY, &context, 0, 1);
        runtime.histogram_record_at(&LATENCY, &context, 1, 2);
        runtime.histogram_record_at(&LATENCY, &context, 3, 3);
        runtime.histogram_record_at(&LATENCY, &context, 1000, 4);

        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.histograms.len(), 1);
        let histogram = &snapshot.histograms[0];
        assert_eq!(histogram.name, "frame_micros");
        assert_eq!(histogram.unit, Unit::Micros);
        assert_eq!(histogram.count, 4);
        assert_eq!(histogram.sum, 1004);
        assert_eq!(histogram.min, 0);
        assert_eq!(histogram.max, 1000);
        assert_eq!(histogram.buckets[super::histogram_bucket_index(0)], 1);
        assert_eq!(histogram.buckets[super::histogram_bucket_index(1)], 1);
        assert_eq!(histogram.buckets[super::histogram_bucket_index(3)], 1);
        assert_eq!(histogram.buckets[super::histogram_bucket_index(1000)], 1);
        assert_eq!(histogram.updated_at_micros, 4);

        // Histogram keys share the counter capacity budget.
        let drained = runtime.drain_snapshot();
        assert_eq!(drained.histograms.len(), 1);
        assert!(runtime.snapshot().histograms.is_empty());
    }

    #[test]
    fn histogram_bucket_indices_follow_bit_length() {
        assert_eq!(super::histogram_bucket_index(0), 0);
        assert_eq!(super::histogram_bucket_index(1), 1);
        assert_eq!(super::histogram_bucket_index(2), 2);
        assert_eq!(super::histogram_bucket_index(3), 2);
        assert_eq!(super::histogram_bucket_index(4), 3);
        assert_eq!(super::histogram_bucket_index(u64::MAX), 63);
    }

    #[test]
    #[cfg_attr(target_os = "wasi", ignore = "wasm32-wasip1 cannot spawn threads")]
    fn parallel_counter_writes_aggregate_without_loss() {
        const THREADS: u64 = 8;
        const UPDATES: u64 = 1000;

        let runtime = Runtime::new(64, 64);
        static OPS: CounterDesc = CounterDesc::count("ops");
        std::thread::scope(|scope| {
            for thread in 0..THREADS {
                let runtime = &runtime;

                scope.spawn(move || {
                    let shared = Context::root().with(&PASS, 1_u32);
                    let distinct = Context::root()
                        .with(&NODE, u32::try_from(thread).expect("thread index fits"));
                    for update in 0..UPDATES {
                        runtime.counter_add_at(&OPS, &shared, 1, update);
                        runtime.counter_add_at(&OPS, &distinct, 1, update);
                    }
                });
            }
        });

        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.dropped_counters, 0);
        assert_eq!(snapshot.dropped_contexts, 0);
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

        let runtime = Runtime::new(16, CAPACITY);
        let context_id = runtime
            .context_id(&Context::root())
            .expect("root context should exist");
        std::thread::scope(|scope| {
            for _ in 0..THREADS {
                let runtime = &runtime;
                scope.spawn(move || {
                    for event in 0..EVENTS {
                        runtime.push_event_at(
                            Observation::FrameMarker {
                                kind: MarkerKind::Frame,
                                phase: MarkerPhase::Begin,
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
            snapshot.events.len() as u64 + snapshot.dropped_events,
            THREADS * EVENTS
        );
    }

    #[test]
    fn context_table_exhaustion_is_accounted() {
        // Context capacity is counter_capacity + event_capacity + 1 = 5.
        let runtime = Runtime::new(2, 2);
        static OPS: CounterDesc = CounterDesc::count("ops");
        for node in 0..32_u32 {
            let context = Context::root().with(&NODE, node);
            runtime.counter_add_at(&OPS, &context, 1, u64::from(node));
        }

        let snapshot = runtime.snapshot();
        assert!(snapshot.dropped_contexts > 0);
        assert!(snapshot.dropped_counters > 0);
        assert_eq!(
            snapshot.dropped_contexts + snapshot.contexts.len() as u64 - 1,
            32,
            "every distinct context should be interned or accounted as dropped"
        );
    }

    #[test]
    #[cfg_attr(target_os = "wasi", ignore = "wasm32-wasip1 cannot spawn threads")]
    fn events_from_different_threads_carry_distinct_thread_ids() {
        let runtime = Runtime::new(8, 8);
        let context_id = runtime
            .context_id(&Context::root())
            .expect("root context should exist");
        std::thread::scope(|scope| {
            for _ in 0..2 {
                let runtime = &runtime;
                scope.spawn(move || {
                    runtime.push_event_at(
                        Observation::FrameMarker {
                            kind: MarkerKind::Frame,
                            phase: MarkerPhase::Begin,
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
        let runtime = Runtime::new(16, 16);
        static OPS: CounterDesc = CounterDesc::count("ops");
        static ATLAS: ResourceKindDesc = ResourceKindDesc::new("atlas");
        static RECORD: ScopeDesc = ScopeDesc::new("record");
        let context = Context::root().with(&PASS, 1_u32);

        runtime.counter_add_at(&OPS, &context, 5, 10);
        runtime.gauge_set_at(&OPS, &context, 9, 11);
        let token = runtime.next_scope_token();
        let context_id = runtime.context_id(&context).expect("context should intern");
        runtime.push_event_at(
            Observation::ScopeBegin {
                desc_id: RECORD.id(),
                name: RECORD.name(),
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
        assert_eq!(snapshot.metadata.process_name, "footfall");
        assert_eq!(snapshot.metadata.session_sequence, 0);
        assert!(
            snapshot.metadata.wall_clock_anchor_micros > 0,
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
        assert_eq!(snapshot.events[0].at_micros, 12);
        assert_eq!(snapshot.events[0].thread_id, super::current_thread_id());
        assert_eq!(
            snapshot.events[0].observation.context_id(),
            Some(context_id)
        );
    }

    #[test]
    fn sequential_scopes_in_one_microsecond_keep_program_order() {
        let runtime = Runtime::new(8, 8);
        static FIRST: ScopeDesc = ScopeDesc::new("first");
        static SECOND: ScopeDesc = ScopeDesc::new("second");
        let context_id = runtime
            .context_id(&Context::root().with(&PASS, 1_u32))
            .expect("context should intern");

        for desc in [&FIRST, &SECOND] {
            let token = runtime.next_scope_token();
            runtime.push_event_at(
                Observation::ScopeBegin {
                    desc_id: desc.id(),
                    name: desc.name(),
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
            "same-microsecond scopes must not appear nested"
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
    fn drain_snapshot_clears_state_and_advances_session_sequence() {
        let runtime = Runtime::named("demo", 8, 8);
        runtime.set_session_name("recording");

        let context_id = runtime
            .context_id(&Context::root().with(&FRAME, 1_u32))
            .expect("context should intern");
        runtime.push_event_at(
            Observation::FrameMarker {
                kind: MarkerKind::Frame,
                phase: MarkerPhase::Begin,
                context_id,
            },
            1,
        );

        let drained = runtime.drain_snapshot();
        assert_eq!(drained.metadata.process_name, "demo");
        assert_eq!(drained.metadata.session_name.as_deref(), Some("recording"));
        assert_eq!(drained.metadata.session_sequence, 0);
        assert_eq!(drained.events.len(), 1);

        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.metadata.session_sequence, 1);
        assert_eq!(snapshot.events.len(), 0);
        assert_eq!(snapshot.contexts.len(), 1);
    }

    #[test]
    fn scope_tokens_stay_unique_across_session_resets() {
        let runtime = Runtime::new(8, 8);
        let before_reset = runtime.next_scope_token();
        let _ = runtime.drain_snapshot();
        let after_reset = runtime.next_scope_token();
        assert_ne!(
            before_reset, after_reset,
            "a scope straddling a drain must not collide with new tokens"
        );
        assert!(after_reset.get() > before_reset.get());
    }

    #[test]
    fn session_name_changes_through_shared_reference() {
        let runtime = Runtime::named("demo", 8, 8).with_session_name("first");
        let shared: &Runtime = &runtime;

        assert_eq!(
            shared.snapshot().metadata.session_name.as_deref(),
            Some("first")
        );

        shared.set_session_name("second");
        let drained = shared.drain_snapshot();
        assert_eq!(drained.metadata.session_name.as_deref(), Some("second"));

        // The label persists across the session reset until cleared.
        assert_eq!(
            shared.snapshot().metadata.session_name.as_deref(),
            Some("second")
        );
        shared.clear_session_name();
        assert_eq!(shared.snapshot().metadata.session_name, None);
    }

    #[test]
    fn runtime_enforces_event_capacity() {
        let runtime = Runtime::new(4, 1);
        let context_id = runtime
            .context_id(&Context::root())
            .expect("root context should exist");
        runtime.push_event_at(
            Observation::FrameMarker {
                kind: MarkerKind::Frame,
                phase: MarkerPhase::Begin,
                context_id,
            },
            1,
        );
        runtime.push_event_at(
            Observation::FrameMarker {
                kind: MarkerKind::Frame,
                phase: MarkerPhase::End,
                context_id,
            },
            2,
        );

        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.events.len(), 1);
        assert_eq!(snapshot.dropped_events, 1);
    }

    #[test]
    fn runtime_enforces_counter_capacity_across_shards() {
        let runtime = Runtime::new(1, 4);
        let context = Context::root().with(&FRAME, 1_u32);
        static FIRST: CounterDesc = CounterDesc::count("first");
        static SECOND: CounterDesc = CounterDesc::count("second");

        runtime.counter_add_at(&FIRST, &context, 1, 1);
        runtime.counter_add_at(&SECOND, &context, 1, 2);

        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.counters.len(), 1);
        assert_eq!(snapshot.dropped_counters, 1);
    }

    #[test]
    fn backend_respects_runtime_capture_level() {
        static COUNTER: CounterDesc = CounterDesc::count("ops");
        static SCOPE: ScopeDesc = ScopeDesc::new("record");
        static RESOURCE: ResourceKindDesc = ResourceKindDesc::new("atlas");

        let runtime = Runtime::with_level(16, 16, InstrumentationLevel::Counters);

        let context = Context::root().with(&FRAME, 3_u32);
        runtime.counter_add(&COUNTER, &context, 1);
        let token = runtime.scope_begin(&SCOPE, &context, &Args::none());
        assert_eq!(token, ScopeToken::INVALID);
        runtime.scope_end(token);
        runtime.resource_create(&RESOURCE, ResourceId::new(7), &context, 64);
        runtime.frame_marker(MarkerKind::Frame, MarkerPhase::Begin, &context);
        runtime.flow(FlowId::new(9), FlowPhase::Begin, &context);

        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.counters.len(), 1);
        assert_eq!(snapshot.events.len(), 0);

        // A scope begun while scopes were excluded must not record an end
        // after the level is raised.
        let excluded = runtime.scope_begin(&SCOPE, &context, &Args::none());
        runtime.set_level(InstrumentationLevel::Trace);
        runtime.scope_end(excluded);
        assert!(runtime.snapshot().events.is_empty());

        runtime.reset();
        runtime.counter_add(&COUNTER, &context, 1);
        let token = runtime.scope_begin(&SCOPE, &context, &Args::none());
        runtime.scope_end(token);
        runtime.resource_create(&RESOURCE, ResourceId::new(7), &context, 64);
        runtime.frame_marker(MarkerKind::Frame, MarkerPhase::Begin, &context);
        runtime.flow(FlowId::new(9), FlowPhase::Begin, &context);

        let snapshot = runtime.snapshot();
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

        let runtime = Runtime::with_level(16, 16, InstrumentationLevel::Trace);
        let context = Context::root().with(&FRAME, 1_u32);
        for _ in 0..2 {
            let token = runtime.scope_begin(&RECORD, &context, &Args::none());
            runtime.scope_end(token);
        }
        runtime.resource_create(&ATLAS, ResourceId::new(1), &context, 64);
        runtime.message(&LOG, &context, "hello", &Args::none());
        runtime.counter_add(&OPS, &context, 1);
        runtime.counter_add(&OPS, &Context::root(), 1);
        runtime.histogram_record(&OPS, &context, 5);
        runtime.histogram_record(&LATENCY, &context, 7);

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

        // Ids stay fixed for the rest of the session and restart after a
        // reset.
        let again = runtime.drain_snapshot();
        assert_eq!(again.counters[0].desc_id.get(), 4);
        runtime.histogram_record(&LATENCY, &context, 7);
        let fresh = runtime.snapshot();
        assert_eq!(fresh.histograms[0].desc_id.get(), 1);
    }
}
